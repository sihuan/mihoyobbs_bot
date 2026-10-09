//! 调度：常驻轮询（温和节奏 + 失败退避）+ 每日活动到期检查

use crate::config::{type_name, Config};
use crate::mihoyo::MhyClient;
use crate::push::{self, Outcome};
use crate::state::State;
use std::sync::Arc;
use std::time::Duration;
use teloxide::prelude::*;

#[derive(Debug, Default, Clone)]
pub struct PollStats {
    pub checked: usize,
    pub fresh: usize,
    pub pushed: usize,
    pub failed: usize,
    pub errors: Vec<String>,
}

/// 跑一轮：遍历「已配置游戏 × 已开启类型」
pub async fn poll_once(
    bot: &Bot,
    cfg: &Config,
    st: &State,
    mhy: &MhyClient,
    http: &reqwest::Client,
) -> PollStats {
    let mut stats = PollStats::default();
    let chats = &cfg.telegram.chats;
    for gids in cfg.game_ids() {
        let Some(game) = cfg.game(gids) else { continue };
        let game_name = game.name.clone();
        let types = match st.enabled_types(gids) {
            Ok(t) => t,
            Err(e) => {
                stats.errors.push(format!("读取开关失败 gids={gids}: {e}"));
                continue;
            }
        };
        for ty in types {
            if ty == 9 {
                // 9 = 活动预警（伪类型），不是资讯类型，由 check_activities 处理
                continue;
            }
            stats.checked += 1;
            match mhy.news_list(gids, ty, cfg.push.page_size).await {
                Ok(items) => {
                    let mut fresh: Vec<_> = items
                        .into_iter()
                        .filter(|i| !st.is_seen(&i.post_id).unwrap_or(true))
                        .collect();
                    // 由旧到新推送，保证时序
                    fresh.sort_by_key(|i| i.created_at);
                    stats.fresh += fresh.len();
                    for item in fresh {
                        let outcome = push::push_item(bot, chats, &game_name, ty, &item, http).await;
                        match outcome {
                            Outcome::Ok => {
                                if let Err(e) = st.mark_seen(&item, ty) {
                                    tracing::warn!(error = %e, "写入 seen 失败");
                                }
                                stats.pushed += 1;
                                tracing::info!(gids, ty, post_id = %item.post_id, "已推送: {}", item.subject);
                            }
                            Outcome::Partial => {
                                // 部分成功：标记已推，避免对成功的目标重复刷屏
                                let _ = st.mark_seen(&item, ty);
                                stats.pushed += 1;
                                tracing::warn!(post_id = %item.post_id, "部分目标发送失败");
                            }
                            Outcome::Failed => {
                                stats.failed += 1;
                                stats.errors.push(format!("推送失败 post_id={}", item.post_id));
                            }
                        }
                    }
                }
                Err(e) => {
                    stats
                        .errors
                        .push(format!("拉取失败 {}({}) {}: {e}", game_name, type_name(ty), ty));
                }
            }
            // 请求间隔（对突发限流的礼貌处理）
            tokio::time::sleep(Duration::from_millis(cfg.push.pace_ms)).await;
        }
    }
    let _ = st.kv_set("last_poll", &chrono::Utc::now().timestamp().to_string());
    let _ = st.kv_set(
        "last_poll_summary",
        &format!(
            "checked={} fresh={} pushed={} failed={}",
            stats.checked, stats.fresh, stats.pushed, stats.failed
        ),
    );
    stats
}

/// 每日活动到期检查
pub async fn check_activities(
    bot: &Bot,
    cfg: &Config,
    st: &State,
    mhy: &MhyClient,
) -> Vec<String> {
    use chrono::{FixedOffset, Timelike};
    let tz = FixedOffset::east_opt(8 * 3600).expect("tz");
    let now = chrono::Utc::now().with_timezone(&tz);
    let mut notes = Vec::new();
    if !cfg.activities.enabled || now.hour() < cfg.activities.alert_hour {
        return notes;
    }
    let today = now.format("%Y-%m-%d").to_string();
    if st.kv_get("last_activity_check").ok().flatten().as_deref() == Some(today.as_str()) {
        return notes;
    }
    let chats = &cfg.telegram.chats;
    for gids in cfg.game_ids() {
        let Some(game) = cfg.game(gids) else { continue };
        if !st.enabled_types(gids).unwrap_or_default().contains(&9) {
            continue;
        }
        let Some(api) = game.ann_api.clone() else { continue };
        match mhy.ann_list(&api).await {
            Ok(list) => {
                for a in list {
                    let Some(end) = a.end_ts else { continue };
                    let days = (end - now.timestamp() + 86_399) / 86_400;
                    if !(0..=cfg.activities.alert_days).contains(&days) {
                        continue;
                    }
                    if st.activity_alerted(&a.game_key, &a.ann_id).unwrap_or(false) {
                        continue;
                    }
                    let text = format!(
                        "⏰ <b>【{} 活动即将结束】</b>\n{}\n\n剩余 <b>{} 天</b>\n截止 {}\n{}",
                        crate::format::plain(&game.name),
                        crate::format::plain(&a.title),
                        days,
                        crate::format::plain(&a.end_time),
                        if a.tag.is_empty() {
                            String::new()
                        } else {
                            format!("🏷 {}", crate::format::plain(&a.tag))
                        }
                    );
                    for &chat in chats {
                        let _ = bot
                            .send_message(teloxide::types::ChatId(chat), text.clone())
                            .parse_mode(teloxide::types::ParseMode::Html)
                            .await;
                    }
                    let _ = st.mark_activity_alerted(&a.game_key, &a.ann_id, &a.end_time);
                    notes.push(format!("活动预警: {} {}", game.name, a.title));
                }
            }
            Err(e) => notes.push(format!("活动检查失败 {}: {e}", game.name)),
        }
        tokio::time::sleep(Duration::from_millis(cfg.push.pace_ms)).await;
    }
    let _ = st.kv_set("last_activity_check", &today);
    notes
}

fn jitter(interval_secs: u64) -> u64 {
    let span = (interval_secs / 10).max(1);
    (chrono::Utc::now().timestamp_subsec_millis() as u64) % span
}

/// 首次运行：把当前内容标记为已见（**不推送**），避免刷屏
pub async fn baseline(cfg: &Config, st: &State, mhy: &MhyClient) -> usize {
    let mut n = 0;
    for gids in cfg.game_ids() {
        let types = st.enabled_types(gids).unwrap_or_default();
        for ty in types {
            match mhy.news_list(gids, ty, cfg.push.page_size).await {
                Ok(items) => {
                    for it in items {
                        if st.mark_seen(&it, ty).is_ok() {
                            n += 1;
                        }
                    }
                }
                Err(e) => tracing::warn!("基线抓取失败 gids={gids} ty={ty}: {e}"),
            }
            tokio::time::sleep(Duration::from_millis(cfg.push.pace_ms)).await;
        }
    }
    n
}

/// 首次运行：把"当前已进入提醒窗口"的活动标记为已提醒，避免刷屏
pub async fn activities_baseline(cfg: &Config, st: &State, mhy: &MhyClient) {
    use chrono::FixedOffset;
    let tz = FixedOffset::east_opt(8 * 3600).expect("tz");
    let now = chrono::Utc::now().with_timezone(&tz);
    for gids in cfg.game_ids() {
        let Some(g) = cfg.game(gids) else { continue };
        if !st.enabled_types(gids).unwrap_or_default().contains(&9) {
            continue;
        }
        let Some(api) = g.ann_api.clone() else { continue };
        if let Ok(list) = mhy.ann_list(&api).await {
            for a in list {
                if let Some(end) = a.end_ts {
                    let days = (end - now.timestamp() + 86_399) / 86_400;
                    if (0..=cfg.activities.alert_days).contains(&days) {
                        let _ = st.mark_activity_alerted(&a.game_key, &a.ann_id, &a.end_time);
                    }
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(cfg.push.pace_ms)).await;
    }
    let _ = st.kv_set("last_activity_check", &now.format("%Y-%m-%d").to_string());
    tracing::info!("活动基线完成");
}

/// 常驻轮询主循环
pub async fn run(bot: Bot, cfg: Arc<Config>, st: State, mhy: MhyClient, http: reqwest::Client) {
    tracing::info!(
        interval = cfg.push.poll_interval_secs,
        "调度启动（{} 个游戏）",
        cfg.game_ids().len()
    );
    if st.kv_get("seeded").ok().flatten().is_none() {
        tracing::info!("首次运行：建立基线（只记录、不推送）");
        let n = baseline(&cfg, &st, &mhy).await;
        activities_baseline(&cfg, &st, &mhy).await;
        let _ = st.kv_set("seeded", "1");
        tracing::info!("基线完成，标记 {n} 条资讯为已见");
    }
    let mut backoff = cfg.push.poll_interval_secs.max(30);
    loop {
        let stats = poll_once(&bot, &cfg, &st, &mhy, &http).await;
        if stats.errors.is_empty() {
            backoff = cfg.push.poll_interval_secs.max(30);
            tracing::info!(
                checked = stats.checked,
                fresh = stats.fresh,
                pushed = stats.pushed,
                "轮询完成"
            );
        } else {
            for e in &stats.errors {
                tracing::warn!("{e}");
            }
            backoff = (backoff * 2).min(cfg.push.max_backoff_secs.max(60));
            tracing::warn!(backoff_secs = backoff, "本轮有错误，退避");
        }
        if let Err(e) = st.prune_seen(500).map_err(anyhow::Error::from) {
            tracing::warn!(error = %e, "prune 失败");
        }
        let notes = check_activities(&bot, &cfg, &st, &mhy).await;
        for n in notes {
            tracing::info!("{n}");
        }
        tokio::time::sleep(Duration::from_secs(backoff + jitter(backoff))).await;
    }
}
