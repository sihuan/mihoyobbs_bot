//! 消息推送：优先"自己下载封面再上传"（避免让 Telegram 去抓 URL 的不可控失败）

use crate::format;
use crate::mihoyo::NewsItem;
use teloxide::prelude::*;
use teloxide::types::{ChatId, InputFile, ParseMode};

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// 所有目标都成功
    Ok,
    /// 部分成功
    Partial,
    /// 全部失败
    Failed,
}

const IMG_MAX: usize = 8 * 1024 * 1024;

async fn fetch_image(http: &reqwest::Client, url: &str) -> Option<(Vec<u8>, String)> {
    if url.is_empty() {
        return None;
    }
    let resp = http.get(url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    if let Some(len) = resp.content_length() {
        if len as usize > IMG_MAX {
            return None;
        }
    }
    let name = if url.ends_with(".png") {
        "cover.png"
    } else {
        "cover.jpg"
    };
    let bytes = resp.bytes().await.ok()?;
    if bytes.is_empty() || bytes.len() > IMG_MAX {
        return None;
    }
    Some((bytes.to_vec(), name.to_string()))
}

/// 把一条资讯推给所有目标聊天
pub async fn push_item(
    bot: &Bot,
    chats: &[i64],
    game: &str,
    ty: u8,
    item: &NewsItem,
    http: &reqwest::Client,
) -> Outcome {
    if chats.is_empty() {
        tracing::warn!(post_id = %item.post_id, "没有配置推送目标");
        return Outcome::Failed;
    }
    let caption = format::post_caption(game, ty, item, 600);
    let image = fetch_image(http, &item.cover).await;

    let mut ok = 0usize;
    let mut err = 0usize;
    for &chat in chats {
        let res = match &image {
            Some((bytes, name)) => {
                let file = InputFile::memory(bytes.clone()).file_name(name.clone());
                bot.send_photo(ChatId(chat), file)
                    .caption(caption.clone())
                    .parse_mode(ParseMode::Html)
                    .await
                    .map(|_| ())
            }
            None => send_text(bot, chat, &caption).await.map(|_| ()),
        };
        match res {
            Ok(()) => ok += 1,
            Err(e) => {
                err += 1;
                tracing::warn!(post_id = %item.post_id, chat, error = %e, "发送失败，改用纯文本重试");
                // 图片/格式失败时降级为纯文本
                if image.is_some() {
                    match send_text(bot, chat, &caption).await {
                        Ok(_) => {
                            ok += 1;
                            err -= 1;
                        }
                        Err(e2) => tracing::error!(post_id = %item.post_id, chat, error = %e2, "纯文本兜底也失败"),
                    }
                }
            }
        }
    }
    match (ok, err) {
        (_, 0) if ok > 0 => Outcome::Ok,
        (0, _) => Outcome::Failed,
        _ => Outcome::Partial,
    }
}

async fn send_text(bot: &Bot, chat: i64, text: &str) -> ResponseResult<teloxide::types::Message> {
    bot.send_message(ChatId(chat), text)
        .parse_mode(ParseMode::Html)
        .await
}
