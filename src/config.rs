//! 配置：从 TOML 读取

use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub telegram: Telegram,
    #[serde(default)]
    pub push: Push,
    #[serde(default)]
    pub activities: Activities,
    /// key 是游戏 gids 的字符串形式（1=崩坏三 2=原神 3=崩坏二 4=未定 6=崩铁 8=绝区零）
    #[serde(default)]
    pub games: BTreeMap<String, GameCfg>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Telegram {
    pub token: String,
    /// 可执行管理指令的用户 id
    #[serde(default)]
    pub admins: Vec<i64>,
    /// 推送目标（默认聊天/群）
    #[serde(default)]
    pub chats: Vec<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Push {
    /// 兜底轮询间隔（秒）
    #[serde(default = "d_poll")]
    pub poll_interval_secs: u64,
    /// 每个 (游戏,类型) 每次拉取的条数
    #[serde(default = "d_page")]
    pub page_size: u32,
    /// 两次 API 请求之间的最小间隔（毫秒）——实测该 API 对突发流量会软限流
    #[serde(default = "d_pace")]
    pub pace_ms: u64,
    /// 连续失败时的最大退避（秒）
    #[serde(default = "d_backoff")]
    pub max_backoff_secs: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Activities {
    #[serde(default = "d_true")]
    pub enabled: bool,
    /// 活动剩余天数 <= 该值时提醒
    #[serde(default = "d_alert_days")]
    pub alert_days: i64,
    /// 每天几点检查一次活动（本地时区，0-23）
    #[serde(default = "d_alert_hour")]
    pub alert_hour: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GameCfg {
    pub name: String,
    /// 1=公告 2=活动 3=资讯
    #[serde(default)]
    pub types: Vec<u8>,
    /// 是否对该游戏做活动到期预警（**仅作首次初始化开关的默认值**，之后用 /on 原神 预警 控制）
    #[serde(default)]
    pub activities: bool,
    /// 该游戏公告 API 的标识（hk4e=原神, hkrpg=崩铁, ...）
    #[serde(default)]
    pub ann_api: Option<String>,
}

impl Default for Push {
    fn default() -> Self {
        Self {
            poll_interval_secs: d_poll(),
            page_size: d_page(),
            pace_ms: d_pace(),
            max_backoff_secs: d_backoff(),
        }
    }
}

impl Default for Activities {
    fn default() -> Self {
        Self {
            enabled: d_true(),
            alert_days: d_alert_days(),
            alert_hour: d_alert_hour(),
        }
    }
}

fn d_poll() -> u64 {
    120
}
fn d_page() -> u32 {
    10
}
fn d_pace() -> u64 {
    2000
}
fn d_backoff() -> u64 {
    1800
}
fn d_true() -> bool {
    true
}
fn d_alert_days() -> i64 {
    3
}
fn d_alert_hour() -> u32 {
    10
}

/// 类型编号 → 中文名（9 = 活动到期预警，是个"伪类型"，复用开关机制）
pub fn type_name(t: u8) -> &'static str {
    match t {
        1 => "公告",
        2 => "活动",
        3 => "资讯",
        9 => "活动预警",
        _ => "其他",
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let s = std::fs::read_to_string(path)
            .with_context(|| format!("读取配置失败: {}", path.display()))?;
        let cfg: Config = toml::from_str(&s).context("解析 TOML 失败")?;
        Ok(cfg)
    }

    pub fn game(&self, gids: i64) -> Option<&GameCfg> {
        self.games.get(&gids.to_string())
    }

    /// 所有被配置的游戏 gids（升序）
    pub fn game_ids(&self) -> Vec<i64> {
        let mut v: Vec<i64> = self.games.keys().filter_map(|k| k.parse().ok()).collect();
        v.sort_unstable();
        v
    }

    pub fn is_admin(&self, uid: i64) -> bool {
        self.telegram.admins.contains(&uid)
    }
}
