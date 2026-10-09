//! 米游社 / 米哈游 公开 API 客户端（只读，无需登录）
//!
//! 用到的端点：
//! - getNewsList  : 官方资讯列表（公告/活动/资讯），type 必填
//! - getPostFull  : 帖子详情（链接自动解析用）
//! - getAnnList   : 游戏内公告（含 start_time/end_time，活动预警用）
//!
//! 注意：getNewsList 无任何缓存头（无 ETag/Last-Modified），且对突发请求会
//! 软限流（返回 data=null / 非 JSON）。因此调用方必须控制节奏并在失败时退避。

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::time::Duration;

const NEWS_LIST: &str = "https://bbs-api-static.miyoushe.com/painter/wapi/getNewsList";
const POST_FULL: &str = "https://bbs-api.miyoushe.com/post/wapi/getPostFull";
const ANN_LIST: &str = "https://{api}-api.mihoyo.com/common/{api}_cn/announcement/api/getAnnList";

const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                  (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

/// 米游社文章前缀（按游戏）
pub fn post_url(gids: i64, post_id: &str) -> String {
    let key = match gids {
        1 => "bh3",
        2 => "ys",
        3 => "bh2",
        4 => "wd",
        6 => "sr",
        8 => "zzz",
        _ => "ys",
    };
    format!("https://bbs.mihoyo.com/{key}/article/{post_id}")
}

#[derive(Clone)]
pub struct MhyClient {
    http: reqwest::Client,
}

#[derive(Debug, Clone)]
pub struct NewsItem {
    pub gids: i64,
    pub post_id: String,
    pub subject: String,
    /// 摘要（优先 summary，其次 text_summary，最后截 content）
    pub summary: String,
    pub cover: String,
    pub created_at: i64,
    pub topics: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Activity {
    pub game_key: String,
    pub ann_id: String,
    pub title: String,
    pub tag: String,
    pub start_time: String,
    pub end_time: String,
    /// 结束时间戳（用于计算剩余天数）；解析失败为 None
    pub end_ts: Option<i64>,
}

impl MhyClient {
    pub fn new() -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(UA)
            .timeout(Duration::from_secs(20))
            .build()
            .context("构建 http client 失败")?;
        Ok(Self { http })
    }

    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }

    /// 拉取某个游戏某类型的资讯列表
    pub async fn news_list(&self, gids: i64, ty: u8, page_size: u32) -> Result<Vec<NewsItem>> {
        let resp = self
            .http
            .get(NEWS_LIST)
            .query(&[
                ("client_type", "4"),
                ("gids", &gids.to_string()),
                ("last_id", ""),
                ("page_size", &page_size.to_string()),
                ("type", &ty.to_string()),
            ])
            .send()
            .await
            .context("请求 getNewsList 失败")?;

        let status = resp.status();
        let text = resp.text().await.context("读取响应失败")?;
        if !status.is_success() {
            bail!("getNewsList HTTP {status}");
        }
        let env: Envelope<NewsData> = serde_json::from_str(&text)
            .with_context(|| format!("getNewsList 返回非预期 JSON（可能被限流）: {}", preview(&text)))?;
        if env.retcode != 0 {
            bail!(
                "getNewsList retcode={} message={:?}",
                env.retcode,
                env.message
            );
        }
        let list = env.data.map(|d| d.list).unwrap_or_default();
        Ok(list
            .into_iter()
            .map(|e| {
                let p = e.post;
                let subject = p.subject.unwrap_or_default();
                let summary = p
                    .summary
                    .filter(|s| !s.trim().is_empty())
                    .or_else(|| e.text_summary.clone().filter(|s| !s.trim().is_empty()))
                    .or_else(|| p.content.clone().filter(|s| !s.trim().is_empty()))
                    .unwrap_or_default();
                let cover = e
                    .image_list
                    .first()
                    .and_then(|i| i.url.clone())
                    .or(p.cover.clone())
                    .unwrap_or_default();
                NewsItem {
                    gids,
                    post_id: p.post_id,
                    subject: subject.trim().to_string(),
                    summary: strip_html(&summary),
                    cover,
                    created_at: p.created_at.unwrap_or(0),
                    topics: e.topics.into_iter().filter_map(|t| t.name).collect(),
                }
            })
            .collect())
    }

    /// 帖子详情（链接解析）
    pub async fn post_full(&self, post_id: &str) -> Result<PostDetail> {
        let resp = self
            .http
            .get(POST_FULL)
            .query(&[("post_id", post_id)])
            .send()
            .await
            .context("请求 getPostFull 失败")?;
        let text = resp.text().await?;
        let env: Envelope<PostFullData> = serde_json::from_str(&text)
            .with_context(|| format!("getPostFull 返回非预期 JSON: {}", preview(&text)))?;
        if env.retcode != 0 {
            bail!("getPostFull retcode={} message={:?}", env.retcode, env.message);
        }
        let d = env.data.context("getPostFull data 为空")?;
        let p = d.post;
        Ok(PostDetail {
            post_id: p.post_id.unwrap_or_else(|| post_id.to_string()),
            subject: p.subject.unwrap_or_default(),
            content: strip_html(&p.content.unwrap_or_default()),
            cover: p.cover.clone().unwrap_or_default(),
            created_at: p.created_at.unwrap_or(0),
        })
    }

    /// 游戏内公告（含起止时间）——用于活动到期预警
    pub async fn ann_list(&self, api: &str) -> Result<Vec<Activity>> {
        let url = ANN_LIST
            .replace("{api}", api)
            .to_string();
        // 不同游戏的必需参数不同，这里给出通用的一组（实测原神/星铁可用）
        let mut q: Vec<(&str, String)> = vec![
            ("game", api.to_string()),
            ("game_biz", format!("{api}_cn")),
            ("lang", "zh-cn".into()),
            ("bundle_id", format!("{api}_cn")),
            ("platform", "pc".into()),
            ("region", if api == "hkrpg" { "prod_gf_cn".into() } else { "cn_gf01".into() }),
            ("level", "60".into()),
            ("uid", "100000000".into()),
        ];
        if api == "hkrpg" {
            q.push(("auth_appid", "announcement".into()));
            q.push(("authkey_ver", "1".into()));
            q.push(("channel_id", "1".into()));
            q.push(("sign_type", "2".into()));
        }
        let resp = self
            .http
            .get(&url)
            .query(&q)
            .send()
            .await
            .with_context(|| format!("请求 getAnnList({api}) 失败"))?;
        let text = resp.text().await?;
        let env: Envelope<AnnData> = serde_json::from_str(&text)
            .with_context(|| format!("getAnnList 返回非预期 JSON: {}", preview(&text)))?;
        if env.retcode != 0 {
            bail!("getAnnList retcode={} message={:?}", env.retcode, env.message);
        }
        let mut out = Vec::new();
        for cat in env.data.map(|d| d.list).unwrap_or_default() {
            for a in cat.list {
                let end_time = a.end_time.unwrap_or_default();
                out.push(Activity {
                    game_key: api.to_string(),
                    ann_id: a.ann_id.map(|v| value_to_string(&v)).unwrap_or_default(),
                    title: a.title.unwrap_or_default(),
                    tag: a.tag_label.unwrap_or_default(),
                    start_time: a.start_time.unwrap_or_default(),
                    end_ts: parse_local(&end_time),
                    end_time,
                });
            }
        }
        Ok(out)
    }
}

#[derive(Debug, Clone)]
pub struct PostDetail {
    pub post_id: String,
    pub subject: String,
    pub content: String,
    pub cover: String,
    pub created_at: i64,
}

// ---------- serde 结构（字段大量 Option，抗 API 漂移）----------

#[derive(Deserialize)]
struct Envelope<T> {
    retcode: i64,
    #[serde(default)]
    message: Option<String>,
    /// 注意：不能加 #[serde(default)]，否则 serde 会要求 T: Default
    data: Option<T>,
}

#[derive(Deserialize)]
struct NewsData {
    #[serde(default)]
    list: Vec<NewsEntry>,
}

#[derive(Deserialize)]
struct NewsEntry {
    post: Post,
    #[serde(default)]
    topics: Vec<Topic>,
    #[serde(default)]
    image_list: Vec<Image>,
    #[serde(default)]
    text_summary: Option<String>,
}

#[derive(Deserialize)]
struct Topic {
    #[serde(default)]
    name: Option<String>,
}

#[derive(Deserialize)]
struct Image {
    #[serde(default)]
    url: Option<String>,
}

#[derive(Deserialize)]
struct Post {
    post_id: String,
    #[serde(default)]
    subject: Option<String>,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    cover: Option<String>,
    #[serde(default)]
    created_at: Option<i64>,
}

#[derive(Deserialize)]
struct PostFullData {
    post: PostDetailRaw,
}

#[derive(Deserialize)]
struct PostDetailRaw {
    #[serde(default)]
    post_id: Option<String>,
    #[serde(default)]
    subject: Option<String>,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    cover: Option<String>,
    #[serde(default)]
    created_at: Option<i64>,
}

#[derive(Deserialize)]
struct AnnData {
    #[serde(default)]
    list: Vec<AnnCategory>,
}

#[derive(Deserialize)]
struct AnnCategory {
    #[serde(default)]
    list: Vec<AnnItem>,
}

#[derive(Deserialize)]
struct AnnItem {
    #[serde(default)]
    ann_id: Option<serde_json::Value>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    tag_label: Option<String>,
    #[serde(default)]
    start_time: Option<String>,
    #[serde(default)]
    end_time: Option<String>,
}

// ---------- 工具 ----------

fn value_to_string(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// 把 "2026-11-04 06:00:00"（服务器本地时区，即 +08:00）解析为时间戳
fn parse_local(s: &str) -> Option<i64> {
    use chrono::TimeZone;
    let naive = chrono::NaiveDateTime::parse_from_str(s.trim(), "%Y-%m-%d %H:%M:%S").ok()?;
    let tz: chrono::FixedOffset = chrono::FixedOffset::east_opt(8 * 3600)?;
    tz.from_local_datetime(&naive).single().map(|dt| dt.timestamp())
}

/// 粗略去掉 HTML 标签并压缩空白（米游社正文常为富文本）
pub fn strip_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for ch in s.chars() {
        match ch {
            '<' => {
                in_tag = true;
                out.push(' ');
            }
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    let decoded = html_escape::decode_html_entities(&out).to_string();
    decoded.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn preview(s: &str) -> String {
    let t: String = s.chars().take(120).collect();
    t.replace('\n', " ")
}
