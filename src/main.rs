//! mysbot —— 米游社公告/活动/资讯推送 bot（Rust）
//!
//! 用法：mysbot [config.toml]

mod bot;
mod config;
mod format;
mod mihoyo;
mod push;
mod sched;
mod state;

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use teloxide::prelude::*;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "mysbot=info,warn".into()),
        )
        .init();

    let cfg_path: PathBuf = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "config.toml".to_string())
        .into();
    let cfg = Arc::new(config::Config::load(&cfg_path)?);

    let db_path = cfg_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("state.db");
    let st = state::State::open(&db_path)?;
    let games: Vec<(i64, Vec<u8>, bool)> = cfg
        .game_ids()
        .into_iter()
        .filter_map(|g| cfg.game(g).map(|c| (g, c.types.clone(), c.activities)))
        .collect();
    st.seed_toggles(&games).context("初始化开关失败")?;

    let http = reqwest::Client::builder()
        .user_agent("mysbot/0.1")
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let mhy = mihoyo::MhyClient::new()?;

    if cfg.telegram.chats.is_empty() {
        tracing::warn!("telegram.chats 为空，不会有推送目标");
    }
    tracing::info!(
        "启动：{} 个游戏，目标 {}，DB {}",
        cfg.game_ids().len(),
        cfg.telegram.chats.len(),
        db_path.display()
    );

    let bot = Bot::new(&cfg.telegram.token);
    // 校验 token
    match bot.get_me().await {
        Ok(_) => tracing::info!("Telegram getMe 成功，token 有效"),
        Err(e) => {
            tracing::error!("getMe 失败（token 是否正确？）: {e}");
            return Err(anyhow::anyhow!("Telegram 认证失败: {e}"));
        }
    }

    // 后台调度
    {
        let bot = bot.clone();
        let cfg = cfg.clone();
        let st = st.clone();
        let mhy = mhy.clone();
        let http = http.clone();
        tokio::spawn(async move {
            sched::run(bot, cfg, st, mhy, http).await;
        });
    }

    // 指令 + 文本
    let handler = Update::filter_message()
        .branch(
            Update::filter_message()
                .filter_command::<bot::Command>()
                .endpoint(bot::handle_command),
        )
        .branch(Message::filter_text().endpoint(bot::handle_text));

    Dispatcher::builder(bot, handler)
        .dependencies(teloxide::dptree::deps![cfg, st, mhy, http])
        .enable_ctrlc_handler()
        .build()
        .dispatch()
        .await;

    Ok(())
}
