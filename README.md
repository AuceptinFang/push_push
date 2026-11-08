# push_bot

QQ 机器人：读取 Outline 某个目录里各人的进度文档，把一段时间内的更新渲染成战报图发到 QQ 群。

## 使用

在群里 @ 机器人，或私聊发送：

```
push [天数]    最近几天（含今天）的更新，默认 1，最多 30
```

另有每天定时战报和一个管理页（成员名单、订阅、预览）。管理页本身没有登录，只能放在带鉴权的反向代理后面。

## 运行

```sh
cargo build --release
```

需要 [Typst](https://typst.app) 和一个字体目录。配置都走环境变量，必填的是：

| 变量 | 说明 |
|---|---|
| `QQ_APP_ID` / `QQ_APP_SECRET` | QQ 开放平台凭据 |
| `OUTLINE_URL` / `OUTLINE_API_KEY` / `OUTLINE_COLLECTION_ID` | Outline 地址、只读 API Key、目录 id |
| `PUSH_KEY` | HTTP 推送接口的密钥 |

其余可选项见 `src/main.rs` 和 `src/daily.rs`。
