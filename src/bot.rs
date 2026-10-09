//! Telegram 指令与消息处理

use crate::config::{type_name, Config};
use crate::format;
use crate::mihoyo::MhyClient;
use crate::push;
use crate::sched;
use crate::state::State;
use std::sync::Arc;
use teloxide::prelude::*;
use teloxide::utils::command::BotCommands;

#[derive(BotCommands, Clone, Debug)]
#[command(rename_rule = "lowercase", description = "可用命令：")]
pub enum Command {
    #[command(description = "显示帮助")]
    Help,
    #[command(description = "查看各游戏/类型的推送开关")]
    List,
    #[command(description = "开启推送，例：/on 原神 公告")]
    On { args: String },
    #[command(description = "关闭推送，例：/off 原神 资讯")]
    Off { args: String },
    #[command(description = "手动看最新几条，例：/latest 原神 公告")]
    Latest { args: String },
    #[command(description = "立即跑一轮检查（管理员）")]
    Push,
    #[command(description = "查看即将结束的活动")]
    Events,
    #[command(description = "运行状态")]
    Status,
}

pub fn resolve_game(cfg: &Config, s: &str) -> Option<i64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Ok(n) = s.parse::<i64>() {
        if cfg.game(n).is_some() {
            return Some(n);
        }
    }
    for gids in cfg.game_ids() {
        if let Some(g) = cfg.game(gids) {
            if g.name == s || g.name.contains(s) {
                return Some(gids);
            }
        }
    }
    const ALIAS: &[(&str, i64)] = &[
        ("原神", 2),
        ("ys", 2),
        ("gen", 2),
        ("崩铁", 6),
        ("星穹铁道", 6),
        ("星穹", 6),
        ("sr", 6),
        ("hsr", 6),
        ("绝区零", 8),
        ("绝区", 8),
        ("zzz", 8),
        ("崩坏三", 1),
        ("崩三", 1),
        ("bh3", 1),
        ("崩坏二", 3),
        ("崩二", 3),
        ("bh2", 3),
        ("未定事件簿", 4),
        ("未定", 4),
        ("wd", 4),
    ];
    ALIAS
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(s))
        .map(|(_, v)| *v)
        .filter(|g| cfg.game(*g).is_some())
}

pub fn resolve_type(s: &str) -> Option<u8> {
    match s.trim().to_ascii_lowercase().as_str() {
        "1" | "公告" | "ann" | "announce" => Some(1),
        "2" | "活动" | "act" | "activity" => Some(2),
        "3" | "资讯" | "news" | "info" => Some(3),
        "9" | "预警" | "到期" | "活动预警" | "活动到期" | "events" | "expiry" => Some(9),
        _ => None,
    }
}

fn is_admin(cfg: &Config, msg: &Message) -> bool {
    if cfg.telegram.admins.is_empty() {
        return true;
    }
    msg.from.as_ref().map(|u| cfg.is_admin(u.id.0 as i64)).unwrap_or(false)
}

async fn reply(bot: &Bot, msg: &Message, text: impl Into<String>) -> ResponseResult<()> {
    bot.send_message(msg.chat.id, text.into())
        .parse_mode(teloxide::types::ParseMode::Html)
        .await?;
    Ok(())
}

async fn toggle(
    bot: &Bot,
    msg: &Message,
    cfg: &Config,
    st: &State,
    args: &str,
    on: bool,
) -> ResponseResult<()> {
    if !is_admin(cfg, msg) {
        reply(bot, msg, "没有权限").await?;
        return Ok(());
    }
    let parts: Vec<&str> = args.split_whitespace().collect();
    if parts.len() != 2 {
        reply(bot, msg, "用法：<code>/on 原神 公告</code>").await?;
        return Ok(());
    }
    let Some(gids) = resolve_game(cfg, parts[0]) else {
        reply(bot, msg, format!("不认识游戏：{}", format::plain(parts[0]))).await?;
        return Ok(());
    };
    let Some(ty) = resolve_type(parts[1]) else {
        reply(bot, msg, format!("不认识类型：{}", format::plain(parts[1]))).await?;
        return Ok(());
    };
    if let Err(e) = st.set_toggle(gids, ty, on) {
        reply(bot, msg, format!("写入失败：{e}")).await?;
        return Ok(());
    }
    let name = cfg.game(gids).map(|g| g.name.clone()).unwrap_or_default();
    reply(
        bot,
        msg,
        format!(
            "已{} <b>{}</b> 的{}推送",
            if on { "开启" } else { "关闭" },
            format::plain(&name),
            type_name(ty)
        ),
    )
    .await?;
    Ok(())
}

pub async fn handle_command(
    bot: Bot,
    msg: Message,
    cmd: Command,
    cfg: Arc<Config>,
    st: State,
    mhy: MhyClient,
    http: reqwest::Client,
) -> ResponseResult<()> {
    match cmd {
        Command::Help => {
            let text = format!(
                "{}\n\n游戏：{games}\n类型：公告 / 活动 / 资讯 / 预警\n\n开关示例：<code>/on 原神 公告</code>、<code>/off 原神 预警</code>\n也可发米游社文章链接，我会解析。",
                Command::descriptions().to_string(),
                games = cfg
                    .game_ids()
                    .iter()
                    .filter_map(|g| cfg.game(*g).map(|x| format!("{} ({})", x.name, g)))
                    .collect::<Vec<_>>()
                    .join("、")
            );
            reply(&bot, &msg, text).await?;
        }
        Command::List => {
            let toggles = st.all_toggles().unwrap_or_default();
            let mut s = String::from("<b>推送开关</b>\n");
            let mut last = None;
            for (gids, ty, on) in toggles {
                if last != Some(gids) {
                    let name = cfg.game(gids).map(|g| g.name.clone()).unwrap_or_default();
                    s.push_str(&format!("\n<b>{name}</b> ({gids})\n"));
                    last = Some(gids);
                }
                s.push_str(&format!(
                    "  {} {}\n",
                    if on { "✅" } else { "❌" },
                    type_name(ty)
                ));
            }
            reply(&bot, &msg, s).await?;
        }
        Command::On { args } => {
            toggle(&bot, &msg, &cfg, &st, &args, true).await?;
        }
        Command::Off { args } => {
            toggle(&bot, &msg, &cfg, &st, &args, false).await?;
        }
        Command::Latest { args } => {
            let parts: Vec<&str> = args.split_whitespace().collect();
            let (gids, ty) = match (parts.first(), parts.get(1)) {
                (Some(g), Some(t)) => match (resolve_game(&cfg, g), resolve_type(t)) {
                    (Some(gids), Some(ty)) => (gids, ty),
                    _ => {
                        reply(&bot, &msg, "用法：<code>/latest 原神 公告</code>").await?;
                        return Ok(());
                    }
                },
                _ => {
                    reply(&bot, &msg, "用法：<code>/latest 原神 公告</code>").await?;
                    return Ok(());
                }
            };
            let game_name = cfg.game(gids).map(|g| g.name.clone()).unwrap_or_default();
            match mhy.news_list(gids, ty, 3).await {
                Ok(items) => {
                    if items.is_empty() {
                        reply(&bot, &msg, "没有拿到内容").await?;
                    }
                    for it in items.into_iter().take(3) {
                        let _ = push::push_item(&bot, &[msg.chat.id.0], &game_name, ty, &it, &http)
                            .await;
                        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    }
                }
                Err(e) => reply(&bot, &msg, format!("拉取失败：{}", format::plain(&e.to_string()))).await?,
            }
        }
        Command::Push => {
            if !is_admin(&cfg, &msg) {
                reply(&bot, &msg, "没有权限").await?;
                return Ok(());
            }
            reply(&bot, &msg, "开始检查…").await?;
            let stats = sched::poll_once(&bot, &cfg, &st, &mhy, &http).await;
            let mut text = format!(
                "本轮：检查 {} 项，新 {} 条，推送 {} 条，失败 {} 条",
                stats.checked, stats.fresh, stats.pushed, stats.failed
            );
            for e in stats.errors.iter().take(5) {
                text.push_str(&format!("\n⚠️ {}", format::plain(e)));
            }
            reply(&bot, &msg, text).await?;
        }
        Command::Events => {
            let mut text = String::from("<b>即将结束的活动</b>\n");
            let mut any = false;
            for gids in cfg.game_ids() {
                let Some(g) = cfg.game(gids) else { continue };
                let Some(api) = g.ann_api.clone() else { continue };
                match mhy.ann_list(&api).await {
                    Ok(list) => {
                        for a in list {
                            let Some(end) = a.end_ts else { continue };
                            let days = (end - chrono::Utc::now().timestamp() + 86_399) / 86_400;
                            if (0..=7).contains(&days) {
                                any = true;
                                text.push_str(&format!(
                                    "\n<b>{}</b> {}\n  剩余 {} 天（止 {}）\n",
                                    format::plain(&g.name),
                                    format::plain(&a.title),
                                    days,
                                    format::plain(&a.end_time)
                                ));
                            }
                        }
                    }
                    Err(e) => text.push_str(&format!(
                        "\n⚠️ {} 查询失败: {}\n",
                        format::plain(&g.name),
                        format::plain(&e.to_string())
                    )),
                }
                tokio::time::sleep(std::time::Duration::from_millis(cfg.push.pace_ms)).await;
            }
            if !any && text.len() < 40 {
                text.push_str("\n近 7 天没有要结束的活动\n");
            }
            reply(&bot, &msg, text).await?;
        }
        Command::Status => {
            let last = st.kv_get("last_poll").ok().flatten().unwrap_or_else(|| "-".into());
            let summary = st
                .kv_get("last_poll_summary")
                .ok()
                .flatten()
                .unwrap_or_else(|| "-".into());
            let seen = st.seen_count().unwrap_or(-1);
            let last_str = last
                .parse::<i64>()
                .ok()
                .map(|ts| {
                    use chrono::{FixedOffset, TimeZone};
                    let tz = FixedOffset::east_opt(8 * 3600).unwrap();
                    tz.timestamp_opt(ts, 0)
                        .single()
                        .map(|d| d.format("%Y-%m-%d %H:%M:%S").to_string())
                        .unwrap_or(last.clone())
                })
                .unwrap_or(last);
            reply(
                &bot,
                &msg,
                format!(
                    "<b>状态</b>\n最后轮询：{last_str}\n{summary}\n已记录帖数：{seen}\n轮询间隔：{}s\n目标聊天：{}",
                    cfg.push.poll_interval_secs,
                    cfg.telegram
                        .chats
                        .iter()
                        .map(|c| c.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            )
            .await?;
        }
    }
    Ok(())
}

/// 普通文本：解析米游社链接
pub async fn handle_text(
    bot: Bot,
    msg: Message,
    _cfg: Arc<Config>,
    mhy: MhyClient,
) -> ResponseResult<()> {
    let Some(text) = msg.text() else {
        return Ok(());
    };
    let re = regex::Regex::new(
        r"(?:miyoushe\.com|bbs\.mihoyo\.com|mihoyo\.com)/[a-z0-9]+/article/(\d+)",
    )
    .expect("regex");
    let Some(caps) = re.captures(text) else {
        return Ok(());
    };
    let Some(id) = caps.get(1) else { return Ok(()) };
    match mhy.post_full(id.as_str()).await {
        Ok(detail) => {
            let text = format::post_detail(&detail, None);
            let _ = bot
                .send_message(msg.chat.id, text)
                .parse_mode(teloxide::types::ParseMode::Html)
                .await;
        }
        Err(e) => {
            let _ = bot
                .send_message(
                    msg.chat.id,
                    format!("解析失败：{}", format::plain(&e.to_string())),
                )
                .await;
        }
    }
    Ok(())
}
