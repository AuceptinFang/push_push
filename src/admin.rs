//! 管理页：内嵌的单页前端 + /api。本身不做登录，需放在带鉴权的反向代理后面。
//!
//! 改状态的请求必须带 `X-Push-Admin: 1`，并且 Origin（有的话）要和 Host 一致，挡住跨站请求：
//! 浏览器跨站发不了自定义头（会先 CORS 预检，这里不应答预检）。

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, NaiveDate, Utc};
use futures_util::{StreamExt, stream};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::App;
use crate::members::{self, display_name, doc_id, looks_like_real_name};
use crate::report::{Window, tz};
use crate::status::Level;

const INDEX_HTML: &str = include_str!("../web/admin.html");
const ADMIN_JS: &str = include_str!("../web/admin.js");
const ADMIN_CSS: &str = include_str!("../web/admin.css");

pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/", get(index))
        .route(
            "/admin.js",
            get(|| async { asset("text/javascript; charset=utf-8", ADMIN_JS) }),
        )
        .route(
            "/admin.css",
            get(|| async { asset("text/css; charset=utf-8", ADMIN_CSS) }),
        )
        .route("/api/live", get(live))
        .route("/api/status", get(status))
        .route(
            "/api/members",
            get(members_list)
                .post(members_add)
                .patch(members_patch)
                .delete(members_remove),
        )
        .route("/api/members/sync", post(members_sync))
        .route("/api/targets", get(targets_list).patch(targets_patch))
        .route("/api/report/preview", post(report_preview))
        .route("/api/report/send", post(report_send))
        .layer(middleware::from_fn(guard))
        .with_state(app)
}

struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        ApiError(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}"))
    }
}

type ApiResult<T> = Result<T, ApiError>;

fn bad_request(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, msg.into())
}

async fn guard(req: Request, next: Next) -> Response {
    if req.method() != Method::GET && req.method() != Method::HEAD {
        let headers = req.headers();
        let marked = headers.get("x-push-admin").is_some_and(|v| v == "1");
        let same_origin = match (headers.get(header::ORIGIN), headers.get(header::HOST)) {
            (Some(origin), Some(host)) => origin
                .to_str()
                .ok()
                .and_then(|o| o.split_once("://"))
                .is_some_and(|(_, o_host)| host.to_str().is_ok_and(|h| h == o_host)),
            (Some(_), None) => false,
            (None, _) => true,
        };
        if !marked || !same_origin {
            return ApiError(StatusCode::FORBIDDEN, "跨站请求被拒绝".into()).into_response();
        }
    }
    let mut resp = next.run(req).await;
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}

async fn index() -> Response {
    let mut resp = asset("text/html; charset=utf-8", INDEX_HTML);
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; img-src 'self' blob: data:; style-src 'self'; script-src 'self'; frame-ancestors 'none'; base-uri 'none'",
        ),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    resp
}

fn asset(content_type: &'static str, body: &'static str) -> Response {
    ([(header::CONTENT_TYPE, content_type)], body).into_response()
}

/// 健康检查：QQ 网关断开时返回 503，响应里不含任何私密信息
async fn live(State(app): State<Arc<App>>) -> Response {
    let online = app.status.gateway().online;
    let code = if online {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (code, Json(json!({ "online": online }))).into_response()
}

async fn status(State(app): State<Arc<App>>) -> ApiResult<Json<serde_json::Value>> {
    let list = members::load(&app.members_file)?;
    let ignored = list.iter().filter(|m| m.ignore).count();
    Ok(Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "started_at": app.status.started_at,
        "gateway": app.status.gateway(),
        "members": { "total": list.len(), "ignored": ignored },
        "members_since": app.members_sync.as_ref().map(|s| s.since.with_timezone(&tz()).date_naive()),
        "subscribers": app.store.all().await.len(),
        "daily": app.daily_at.map(|at| json!({
            "at": at.format("%H:%M").to_string(),
            "next": crate::daily::next_run(chrono::Local::now(), at),
        })),
        "targets": app.targets.all().await.len(),
        "events": app.status.events(),
    })))
}

#[derive(Serialize)]
struct MemberView {
    doc: String,
    alias: Option<String>,
    ignore: bool,
    /// 自动加入时记下的标题
    title: Option<String>,
    resolved: Option<Resolved>,
    error: Option<String>,
}

#[derive(Serialize)]
struct Resolved {
    title: String,
    url: String,
    /// 账号名像真名时不下发（None），只给 account_looks_real；规则漏判的昵称照常显示，方便发现后补 alias
    account_name: Option<String>,
    account_looks_real: bool,
    /// None 表示定不出显示名，这篇文档会被战报跳过
    display_name: Option<String>,
    updated_at: DateTime<Utc>,
}

async fn members_list(State(app): State<Arc<App>>) -> ApiResult<Json<Vec<MemberView>>> {
    let list = members::load(&app.members_file)?;
    let outline = &app.outline;
    let views = stream::iter(list)
        .map(|m| async move {
            let (resolved, error) = match outline.document(doc_id(&m.doc)).await {
                Ok(d) => (
                    Some(Resolved {
                        display_name: display_name(
                            m.alias.as_deref(),
                            Some(&d.created_by.name),
                            &d.title,
                        ),
                        account_looks_real: looks_like_real_name(&d.created_by.name),
                        account_name: (!looks_like_real_name(&d.created_by.name))
                            .then_some(d.created_by.name),
                        url: outline.absolute(&d.url),
                        title: d.title,
                        updated_at: d.updated_at,
                    }),
                    None,
                ),
                Err(e) => (None, Some(format!("{e:#}"))),
            };
            MemberView {
                doc: m.doc,
                alias: m.alias,
                ignore: m.ignore,
                title: m.title,
                resolved,
                error,
            }
        })
        .buffered(6)
        .collect()
        .await;
    Ok(Json(views))
}

#[derive(Deserialize)]
struct MemberPatch {
    doc: String,
    /// 省略表示不改，空字符串表示清掉
    alias: Option<String>,
    ignore: Option<bool>,
}

async fn members_patch(
    State(app): State<Arc<App>>,
    Json(p): Json<MemberPatch>,
) -> ApiResult<StatusCode> {
    let _guard = app.members_lock.lock().await;
    if !members::update(&app.members_file, &p.doc, p.alias.clone(), p.ignore)? {
        return Err(ApiError(StatusCode::NOT_FOUND, "名单里没有这篇文档".into()));
    }
    app.status.record(
        Level::Info,
        format!(
            "管理页修改成员 {}：alias={:?} ignore={:?}",
            p.doc, p.alias, p.ignore
        ),
    );
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct MemberRef {
    /// 添加时是文档链接 / uuid / urlId；移除时是名单条目里的 doc 原文
    doc: String,
}

async fn members_add(
    State(app): State<Arc<App>>,
    Json(r): Json<MemberRef>,
) -> ApiResult<Json<serde_json::Value>> {
    let id = doc_id(&r.doc);
    if id.is_empty() {
        return Err(bad_request("请粘贴文档链接"));
    }
    let doc = app
        .outline
        .document(id)
        .await
        .map_err(|e| bad_request(format!("找不到这篇文档：{e:#}")))?;
    let _guard = app.members_lock.lock().await;
    if !members::add(&app.members_file, &app.outline, &doc)? {
        return Err(ApiError(
            StatusCode::CONFLICT,
            format!("「{}」已经在名单里了", doc.title),
        ));
    }
    app.status
        .record(Level::Info, format!("管理页添加成员：{}", doc.title));
    Ok(Json(json!({ "title": doc.title })))
}

/// 自动加成员会把它加回来的文档不真删，改成忽略
async fn members_remove(
    State(app): State<Arc<App>>,
    Json(r): Json<MemberRef>,
) -> ApiResult<Json<serde_json::Value>> {
    let doc = app.outline.document(doc_id(&r.doc)).await.ok();
    let auto = match (&app.members_sync, &doc) {
        (Some(s), Some(d)) => members::would_auto_add(d, &s.collection_id, s.since),
        _ => false,
    };
    let title = doc
        .as_ref()
        .map_or(r.doc.as_str(), |d| d.title.as_str())
        .to_owned();
    let _guard = app.members_lock.lock().await;
    let found = if auto {
        members::update(&app.members_file, &r.doc, None, Some(true))?
    } else {
        members::remove(&app.members_file, &r.doc)?
    };
    if !found {
        return Err(ApiError(StatusCode::NOT_FOUND, "名单里没有这篇文档".into()));
    }
    let what = if auto { "改为忽略" } else { "移除" };
    app.status
        .record(Level::Info, format!("管理页{what}成员：{title}"));
    Ok(Json(json!({ "ignored": auto })))
}

async fn members_sync(State(app): State<Arc<App>>) -> ApiResult<Json<serde_json::Value>> {
    let sync = app
        .members_sync
        .as_ref()
        .ok_or_else(|| bad_request("没有配置 MEMBERS_SINCE，自动加成员未启用"))?;
    let _guard = app.members_lock.lock().await;
    let added = members::sync(
        &app.outline,
        &app.members_file,
        &sync.collection_id,
        sync.since,
    )
    .await?;
    app.status
        .record(Level::Info, format!("管理页手动同步成员，新增 {added} 人"));
    Ok(Json(json!({ "added": added })))
}

#[derive(Serialize)]
struct TargetView {
    id: String,
    kind: &'static str,
    label: Option<String>,
    last_seen: DateTime<Utc>,
    subscribed: bool,
}

async fn targets_list(State(app): State<Arc<App>>) -> Json<Vec<TargetView>> {
    let mut out = Vec::new();
    for k in app.targets.all().await {
        out.push(TargetView {
            id: k.target.id(),
            kind: match k.target {
                crate::qq::Target::User(_) => "user",
                crate::qq::Target::Group(_) => "group",
            },
            subscribed: app.store.contains(&k.target).await,
            label: k.label,
            last_seen: k.last_seen,
        });
    }
    out.sort_by_key(|t| std::cmp::Reverse(t.last_seen));
    Json(out)
}

#[derive(Deserialize)]
struct TargetPatch {
    id: String,
    /// 省略表示不改，空字符串表示清掉
    label: Option<String>,
    /// 是否接收每天的定时战报
    subscribed: Option<bool>,
}

async fn targets_patch(
    State(app): State<Arc<App>>,
    Json(p): Json<TargetPatch>,
) -> ApiResult<StatusCode> {
    let target = app
        .targets
        .find(&p.id)
        .await
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "没有这个会话".into()))?;
    if let Some(label) = p.label {
        app.targets.set_label(&p.id, Some(label)).await?;
    }
    if let Some(on) = p.subscribed {
        if on {
            app.store.add(target).await?;
        } else {
            app.store.remove(&target).await?;
        }
        app.status.record(
            Level::Info,
            format!(
                "管理页{}定时战报：{}",
                if on { "开启" } else { "关闭" },
                p.id
            ),
        );
    }
    Ok(StatusCode::NO_CONTENT)
}

/// from、to 都省略时是下一次定时战报到现在为止的内容（按本地时间和 DAILY_PUSH_AT 算）
#[derive(Deserialize)]
struct ReportRange {
    #[serde(default)]
    from: Option<NaiveDate>,
    #[serde(default)]
    to: Option<NaiveDate>,
}

impl ReportRange {
    fn window(&self, app: &App) -> ApiResult<Window> {
        match (self.from, self.to) {
            (Some(from), Some(to)) if from > to => Err(bad_request("开始日期晚于结束日期")),
            (Some(from), Some(to)) => Ok(Window::dates(from, to)),
            (None, None) => {
                let at = app
                    .daily_at
                    .ok_or_else(|| bad_request("定时战报已关闭，请选日期"))?;
                Ok(crate::daily::upcoming(at))
            }
            _ => Err(bad_request("开始和结束日期要一起给")),
        }
    }

    fn label(&self) -> String {
        match (self.from, self.to) {
            (Some(from), Some(to)) => format!("{from}–{to}"),
            _ => "下一次定时战报".into(),
        }
    }
}

async fn render(app: &App, range: &ReportRange) -> ApiResult<Vec<u8>> {
    let window = range.window(app)?;
    // 和 QQ 命令、定时战报一样，出图前先自动加新成员
    let list = app.current_members().await?;
    let report = app.build_report(list, window).await;
    Ok(app.renderer.render(&report, &app.outline).await?)
}

async fn report_preview(
    State(app): State<Arc<App>>,
    Json(range): Json<ReportRange>,
) -> ApiResult<Response> {
    let png = render(&app, &range).await?;
    Ok(([(header::CONTENT_TYPE, "image/png")], png).into_response())
}

#[derive(Deserialize)]
struct SendRequest {
    #[serde(flatten)]
    range: ReportRange,
    target: String,
}

/// 以主动消息发出（不带 msg_id），需要群里给了机器人「主动推送消息」权限
async fn report_send(
    State(app): State<Arc<App>>,
    Json(req): Json<SendRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let target = app
        .targets
        .find(&req.target)
        .await
        .ok_or_else(|| bad_request("没有这个会话"))?;
    let png = render(&app, &req.range).await?;
    let label = format!("{} {}", req.target, req.range.label());
    match app.qq.send_image(&target, &png, None).await {
        Ok(()) => {
            app.status
                .record(Level::Info, format!("管理页发送战报到 {label}"));
            Ok(Json(json!({ "ok": true, "bytes": png.len() })))
        }
        Err(e) => {
            app.status.record(
                Level::Error,
                format!("管理页发送战报到 {label} 失败：{e:#}"),
            );
            Err(ApiError(
                StatusCode::BAD_GATEWAY,
                format!("发送失败：{e:#}"),
            ))
        }
    }
}
