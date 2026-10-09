//! HTML 消息格式化（统一转义，根治 Markdown 解析失败）

use crate::config::type_name;
use crate::mihoyo::{post_url, NewsItem};
use chrono::{FixedOffset, TimeZone};

fn esc(s: &str) -> String {
    html_escape::encode_text(s).to_string()
}

/// 按字符数截断，尽量在句读处断开
pub fn truncate(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max).collect();
    let stop = ['.', '。', '!', '！', '?', '？', ';', '；', '\n']
        .iter()
        .filter_map(|c| cut.rfind(*c))
        .max();
    match stop {
        Some(i) if i > max / 3 => format!("{}…", &cut[..=i]),
        _ => format!("{cut}…"),
    }
}

fn fmt_time(ts: i64) -> String {
    let tz = FixedOffset::east_opt(8 * 3600).expect("tz");
    match tz.timestamp_opt(ts, 0).single() {
        Some(dt) => dt.format("%Y-%m-%d %H:%M").to_string(),
        None => ts.to_string(),
    }
}

/// 推送消息正文（HTML）
pub fn post_caption(game: &str, ty: u8, item: &NewsItem, body_limit: usize) -> String {
    let url = post_url(item.gids, &item.post_id);
    let mut s = String::new();
    s.push_str(&format!(
        "<b>【{} · {}】{}</b>\n",
        esc(game),
        esc(type_name(ty)),
        esc(&item.subject)
    ));
    let body = truncate(&item.summary, body_limit);
    if !body.is_empty() {
        s.push_str(&format!("\n{esc}\n", esc = esc(&body)));
    }
    s.push_str(&format!("\n🕒 {}\n", fmt_time(item.created_at)));
    if !item.topics.is_empty() {
        let tags: Vec<String> = item
            .topics
            .iter()
            .take(6)
            .map(|t| format!("#{}", esc(t)))
            .collect();
        s.push_str(&format!("{}\n", tags.join(" ")));
    }
    s.push_str(&format!("<a href=\"{}\">阅读全文 ↗</a>", esc(&url)));
    s
}

/// 链接解析回复
pub fn post_detail(
    detail: &crate::mihoyo::PostDetail,
    gids_hint: Option<i64>,
) -> String {
    let url = match gids_hint {
        Some(g) => post_url(g, &detail.post_id),
        None => format!("https://www.miyoushe.com/ys/article/{}", detail.post_id),
    };
    let mut s = String::new();
    s.push_str(&format!("<b>{}</b>\n", esc(&detail.subject)));
    let body = truncate(&detail.content, 1500);
    if !body.is_empty() {
        s.push_str(&format!("\n{esc}\n", esc = esc(&body)));
    }
    s.push_str(&format!("\n🕒 {}\n", fmt_time(detail.created_at)));
    s.push_str(&format!("<a href=\"{}\">原文 ↗</a>", esc(&url)));
    s
}

/// 纯文本转义（给需要 HTML 的普通回复用）
pub fn plain(s: &str) -> String {
    esc(s)
}
