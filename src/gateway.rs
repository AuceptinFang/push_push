//! WebSocket 网关：鉴权、心跳、断线 Resume，并把消息事件交给命令处理。

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;

use crate::App;
use crate::commands;
use crate::qq::Target;

/// 单聊 + 群聊 @ 消息事件（C2C_MESSAGE_CREATE / GROUP_AT_MESSAGE_CREATE 等）
const INTENT_GROUP_AND_C2C: u64 = 1 << 25;

#[derive(Deserialize)]
struct Payload {
    op: u8,
    #[serde(default)]
    d: Value,
    s: Option<u64>,
    t: Option<String>,
}

#[derive(Default)]
struct Session {
    id: Option<String>,
    seq: Option<u64>,
}

pub async fn run(app: Arc<App>) {
    let mut session = Session::default();
    let mut backoff = 1;
    loop {
        match connect_once(&app, &mut session).await {
            Ok(()) => {
                app.status.offline("服务端要求重连".into());
                backoff = 1;
            }
            Err(e) => {
                app.status.offline(format!("{e:#}"));
                tracing::warn!("网关连接断开: {e:#}");
                backoff = (backoff * 2).min(60);
            }
        }
        tracing::info!("{backoff}s 后重连");
        tokio::time::sleep(Duration::from_secs(backoff)).await;
    }
}

async fn connect_once(app: &Arc<App>, session: &mut Session) -> Result<()> {
    let url = app.qq.gateway_url().await?;
    let (ws, _) = tokio_tungstenite::connect_async(&url)
        .await
        .context("连接网关")?;
    let (mut tx, mut rx) = ws.split();

    // 第一条必须是 op 10 Hello
    let hello: Payload = match rx.next().await {
        Some(Ok(Message::Text(t))) => serde_json::from_str(&t)?,
        other => bail!("期待 Hello，收到 {other:?}"),
    };
    if hello.op != 10 {
        bail!("期待 op 10，收到 op {}", hello.op);
    }
    let interval = hello.d["heartbeat_interval"].as_u64().unwrap_or(41250);

    let token = format!("QQBot {}", app.qq.access_token().await?);
    let auth = match (&session.id, session.seq) {
        (Some(id), Some(seq)) => {
            tracing::info!("尝试 Resume 会话");
            json!({ "op": 6, "d": { "token": token, "session_id": id, "seq": seq } })
        }
        _ => json!({
            "op": 2,
            "d": {
                "token": token,
                "intents": INTENT_GROUP_AND_C2C,
                "shard": [0, 1],
                "properties": { "$os": std::env::consts::OS, "$browser": "push_bot", "$device": "push_bot" }
            }
        }),
    };
    tx.send(Message::text(auth.to_string())).await?;

    let mut heartbeat = tokio::time::interval(Duration::from_millis(interval));
    heartbeat.tick().await;
    loop {
        tokio::select! {
            _ = heartbeat.tick() => {
                tx.send(Message::text(json!({ "op": 1, "d": session.seq }).to_string())).await?;
            }
            msg = rx.next() => {
                let text = match msg {
                    Some(Ok(Message::Text(t))) => t,
                    Some(Ok(Message::Close(frame))) => bail!("网关关闭连接: {frame:?}"),
                    Some(Ok(_)) => continue,
                    Some(Err(e)) => return Err(e.into()),
                    None => bail!("连接已结束"),
                };
                let p: Payload = match serde_json::from_str(&text) {
                    Ok(p) => p,
                    Err(e) => { tracing::warn!("无法解析网关消息 {text}: {e}"); continue; }
                };
                match p.op {
                    0 => {
                        if p.s.is_some() { session.seq = p.s; }
                        handle_dispatch(app, p.t.as_deref().unwrap_or(""), p.d, session);
                    }
                    7 => { tracing::info!("服务端要求重连"); return Ok(()); }
                    9 => {
                        // 会话失效，下次重新 Identify
                        *session = Session::default();
                        bail!("Invalid Session（检查 AppID/Secret 及是否开通单聊/群聊事件权限）");
                    }
                    11 => {}
                    op => tracing::debug!("未处理的 op {op}"),
                }
            }
        }
    }
}

fn handle_dispatch(app: &Arc<App>, t: &str, d: Value, session: &mut Session) {
    match t {
        "READY" => {
            session.id = d["session_id"].as_str().map(str::to_owned);
            let name = d["user"]["username"].as_str().map(str::to_owned);
            tracing::info!("已上线: {}", name.as_deref().unwrap_or("?"));
            app.status.online(name);
        }
        "RESUMED" => {
            tracing::info!("会话已恢复");
            app.status.online(None);
        }
        "C2C_MESSAGE_CREATE" | "GROUP_AT_MESSAGE_CREATE" => {
            let target = if t == "C2C_MESSAGE_CREATE" {
                d["author"]["user_openid"]
                    .as_str()
                    .map(|o| Target::User(o.to_owned()))
            } else {
                d["group_openid"]
                    .as_str()
                    .map(|o| Target::Group(o.to_owned()))
            };
            let (Some(target), Some(msg_id)) = (target, d["id"].as_str()) else {
                tracing::warn!("{t} 缺少字段: {d}");
                return;
            };
            let content = d["content"].as_str().unwrap_or("").trim().to_owned();
            let msg_id = msg_id.to_owned();
            let app = app.clone();
            tokio::spawn(async move {
                if let Err(e) = commands::handle(&app, target, &content, &msg_id).await {
                    tracing::warn!("处理消息失败: {e:#}");
                }
            });
        }
        other => tracing::debug!("忽略事件 {other}"),
    }
}
