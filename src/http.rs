//! 推送 HTTP 接口：外部脚本调用它，机器人把内容主动发给订阅者。
//!
//!   GET  /push?key=KEY&content=hello[&to=user:xxx]
//!   POST /push  (Authorization: Bearer KEY)  {"content": "hello", "to": "group:xxx"}

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::App;

#[derive(Deserialize)]
pub struct PushReq {
    key: Option<String>,
    content: String,
    /// 推送 ID（#push status 可查），省略则发给所有订阅者
    to: Option<String>,
}

pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/push", get(push_get).post(push_post))
        .route("/health", get(|| async { "ok" }))
        .with_state(app)
}

async fn push_get(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Query(req): Query<PushReq>,
) -> (StatusCode, Json<Value>) {
    push(&app, &headers, req).await
}

async fn push_post(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Json(req): Json<PushReq>,
) -> (StatusCode, Json<Value>) {
    push(&app, &headers, req).await
}

async fn push(app: &App, headers: &HeaderMap, req: PushReq) -> (StatusCode, Json<Value>) {
    let bearer = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    if bearer.or(req.key.as_deref()) != Some(app.push_key.as_str()) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "invalid key" })),
        );
    }
    if req.content.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "content is empty" })),
        );
    }

    let targets: Vec<_> = app
        .store
        .all()
        .await
        .into_iter()
        .filter(|t| req.to.as_ref().is_none_or(|to| *to == t.id()))
        .collect();
    if targets.is_empty() {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "no subscriber matched; send #push to the bot first" })),
        );
    }

    let mut results = Vec::new();
    let mut ok = 0;
    for t in &targets {
        match app.qq.send_text(t, &req.content, None).await {
            Ok(()) => {
                ok += 1;
                results.push(json!({ "to": t.id(), "ok": true }));
            }
            Err(e) => {
                tracing::warn!("推送到 {} 失败: {e:#}", t.id());
                app.status.record(
                    crate::status::Level::Error,
                    format!("HTTP 推送到 {} 失败：{e:#}", t.id()),
                );
                results.push(json!({ "to": t.id(), "ok": false, "error": format!("{e:#}") }));
            }
        }
    }
    let status = if ok == 0 {
        StatusCode::BAD_GATEWAY
    } else {
        StatusCode::OK
    };
    (
        status,
        Json(json!({ "sent": ok, "total": targets.len(), "results": results })),
    )
}
