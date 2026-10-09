//! SQLite 状态：已见帖去重、推送开关、活动预警去重、KV

use crate::mihoyo::NewsItem;
use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct State {
    conn: Arc<Mutex<Connection>>,
}

impl State {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("打开 SQLite 失败: {}", path.display()))?;
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            PRAGMA synchronous = NORMAL;
            CREATE TABLE IF NOT EXISTS seen_posts (
                post_id   TEXT PRIMARY KEY,
                gids      INTEGER NOT NULL,
                ty        INTEGER NOT NULL,
                subject   TEXT,
                pushed_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS toggles (
                gids    INTEGER NOT NULL,
                ty      INTEGER NOT NULL,
                enabled INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (gids, ty)
            );
            CREATE TABLE IF NOT EXISTS activity_alerts (
                game       TEXT NOT NULL,
                ann_id     TEXT NOT NULL,
                end_time   TEXT,
                alerted_at INTEGER NOT NULL,
                PRIMARY KEY (game, ann_id)
            );
            CREATE TABLE IF NOT EXISTS kv (
                key   TEXT PRIMARY KEY,
                value TEXT
            );
            "#,
        )
        .context("初始化表失败")?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().expect("sqlite mutex poisoned")
    }

    /// 用配置初始化开关（已存在的不覆盖，保留运行时改动）
    /// games: (gids, 类型列表, 是否开启活动预警)
    pub fn seed_toggles(&self, games: &[(i64, Vec<u8>, bool)]) -> Result<()> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "INSERT OR IGNORE INTO toggles(gids, ty, enabled) VALUES (?1, ?2, ?3)",
        )?;
        for (gids, types, activities) in games {
            for ty in [1u8, 2, 3, 9] {
                let on = if ty == 9 {
                    *activities
                } else {
                    types.contains(&ty)
                };
                stmt.execute(params![gids, ty, on as i64])?;
            }
        }
        Ok(())
    }

    /// 某游戏当前开启的类型集合
    pub fn enabled_types(&self, gids: i64) -> Result<BTreeSet<u8>> {
        let conn = self.lock();
        let mut stmt =
            conn.prepare("SELECT ty FROM toggles WHERE gids = ?1 AND enabled = 1 ORDER BY ty")?;
        let rows = stmt.query_map(params![gids], |r| r.get::<_, u8>(0))?;
        let mut set = BTreeSet::new();
        for r in rows {
            set.insert(r?);
        }
        Ok(set)
    }

    /// 全部开关状态：(gids, ty, enabled)
    pub fn all_toggles(&self) -> Result<Vec<(i64, u8, bool)>> {
        let conn = self.lock();
        let mut stmt = conn.prepare("SELECT gids, ty, enabled FROM toggles ORDER BY gids, ty")?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, u8>(1)?, r.get::<_, i64>(2)? != 0))
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn set_toggle(&self, gids: i64, ty: u8, on: bool) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO toggles(gids, ty, enabled) VALUES (?1, ?2, ?3)
             ON CONFLICT(gids, ty) DO UPDATE SET enabled = excluded.enabled",
            params![gids, ty, on as i64],
        )?;
        Ok(())
    }

    pub fn is_seen(&self, post_id: &str) -> Result<bool> {
        let conn = self.lock();
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM seen_posts WHERE post_id = ?1",
            params![post_id],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    pub fn mark_seen(&self, item: &NewsItem, ty: u8) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT OR REPLACE INTO seen_posts(post_id, gids, ty, subject, pushed_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                &item.post_id,
                item.gids,
                ty as i64,
                &item.subject,
                chrono::Utc::now().timestamp()
            ],
        )?;
        Ok(())
    }

    /// 保留每个 (gids) 最近 N 条记录，避免无限增长
    pub fn prune_seen(&self, keep_per_game: i64) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "DELETE FROM seen_posts WHERE post_id IN (
                 SELECT post_id FROM (
                     SELECT post_id, ROW_NUMBER() OVER (
                         PARTITION BY gids ORDER BY pushed_at DESC
                     ) AS rn FROM seen_posts
                 ) WHERE rn > ?1
             )",
            params![keep_per_game],
        )?;
        Ok(())
    }

    pub fn activity_alerted(&self, game: &str, ann_id: &str) -> Result<bool> {
        let conn = self.lock();
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM activity_alerts WHERE game = ?1 AND ann_id = ?2",
            params![game, ann_id],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    pub fn mark_activity_alerted(&self, game: &str, ann_id: &str, end_time: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT OR REPLACE INTO activity_alerts(game, ann_id, end_time, alerted_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![game, ann_id, end_time, chrono::Utc::now().timestamp()],
        )?;
        Ok(())
    }

    pub fn kv_get(&self, k: &str) -> Result<Option<String>> {
        let conn = self.lock();
        let v = conn
            .query_row("SELECT value FROM kv WHERE key = ?1", params![k], |r| {
                r.get::<_, Option<String>>(0)
            })
            .ok()
            .flatten();
        Ok(v)
    }

    pub fn kv_set(&self, k: &str, v: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO kv(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![k, v],
        )?;
        Ok(())
    }

    pub fn seen_count(&self) -> Result<i64> {
        let conn = self.lock();
        Ok(conn.query_row("SELECT COUNT(*) FROM seen_posts", [], |r| r.get(0))?)
    }
}
