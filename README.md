# mysbot

米游社（miHoYo BBS）公告 / 活动 / 资讯的 Telegram 推送 bot，Rust 单二进制。

替代了我之前的 Python 版本：不依赖 redis，不需要 Python 运行时，常驻 systemd 服务即可。

## 功能

- **多游戏 × 多类型推送**：原神、崩坏：星穹铁道、绝区零等，类型为 公告 / 活动 / 资讯
- **运行时开关**：`/on 原神 公告`、`/off 崩铁 预警`（含活动到期预警），无需改配置重启
- **活动到期预警**：读取游戏内公告 API（含 `start_time`/`end_time`），剩余 ≤ N 天提醒，每日一次
- **链接自动解析**：把米游社文章链接发给 bot，自动回复标题 + 正文 + 封面
- **命令**：`/help` `/list` `/on` `/off` `/latest` `/events` `/status` `/push`
- **抗刷屏**：首次运行建立「资讯 + 活动」基线，只记录不推送
- **抗限流**：请求间隔 + 失败指数退避（`getNewsList` 无缓存头，突发请求会被软限流）
- **版式**：HTML + 完整转义；封面由 bot 自己下载后上传（不依赖 Telegram 抓取 URL）

## 构建

```bash
cargo build --release
# 产物: target/release/mysbot
```

## 运行

```bash
cp config.example.toml config.toml
$EDITOR config.toml
./target/release/mysbot config.toml
```

`state.db`（SQLite，记录已见帖/开关/预警状态）会生成在 config 同目录下。

### systemd

```ini
[Unit]
Description=mysbot (miHoYo BBS push + Telegram commands)
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=youruser
WorkingDirectory=/path/to/mysbot
ExecStart=/path/to/mysbot/target/release/mysbot /path/to/mysbot/config.toml
Environment=RUST_LOG=mysbot=info
Restart=always
RestartSec=10

[Install]
WantedBy=multi-user.target
```

## 配置

见 `config.example.toml`。要点：

- `telegram.token` / `telegram.chats` / `telegram.admins`
- `[games.<gids>]` 里 `types` 决定**首次**开启哪些类型的推送；之后用 `/on`、`/off` 调整（存在 SQLite）
- `ann_api` 填该游戏的公告 API 标识（`hk4e`=原神、`hkrpg`=崩铁），活动预警需要它

游戏 gids：`1`=崩坏三 `2`=原神 `3`=崩坏二 `4`=未定事件簿 `6`=崩坏：星穹铁道 `8`=绝区零

## 用到的接口

| 用途 | 端点 |
|---|---|
| 资讯列表 | `bbs-api-static.miyoushe.com/painter/wapi/getNewsList`（`type` 必填：1公告 2活动 3资讯） |
| 帖子详情 | `bbs-api.miyoushe.com/post/wapi/getPostFull?post_id=` |
| 游戏公告 | `{hk4e,hkrpg}-api.mihoyo.com/common/{api}_cn/announcement/api/getAnnList` |

均为公开只读接口，无需登录。

## 说明

- 请勿把 `config.toml`（含 bot token）提交到仓库，`.gitignore` 已包含
- `getNewsList` 没有 `ETag`/`Last-Modified`，无法做条件请求，因此只能轮询；请勿把轮询间隔设得过小
