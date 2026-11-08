mod admin;
mod commands;
mod daily;
mod gateway;
mod http;
mod members;
mod outline;
mod qq;
mod render;
mod report;
mod status;
mod store;
mod targets;

use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, bail};

pub struct App {
    pub qq: qq::QqClient,
    pub store: store::Store,
    pub push_key: String,
    pub outline: outline::OutlineClient,
    pub members_file: PathBuf,
    pub renderer: render::Renderer,
    /// 设置后每次 push 前自动把这之后新建的文档加进名单
    pub members_sync: Option<MembersSync>,
    /// 串行化名单文件的读写
    pub members_lock: tokio::sync::Mutex<()>,
    /// 网关状态和最近活动，给管理页看
    pub status: status::Status,
    /// 发过命令的私聊和群
    pub targets: targets::Targets,
    /// 每天定时发战报的本地时刻，None 表示关闭
    pub daily_at: Option<chrono::NaiveTime>,
}

impl App {
    /// 当前名单：设置了 MEMBERS_SINCE 时先自动加新成员（失败不影响，用现有名单继续）
    pub async fn current_members(&self) -> Result<Vec<members::Member>> {
        let _guard = self.members_lock.lock().await;
        if let Some(sync) = &self.members_sync {
            match members::sync(
                &self.outline,
                &self.members_file,
                &sync.collection_id,
                sync.since,
            )
            .await
            {
                Ok(0) => {}
                Ok(n) => {
                    tracing::info!("自动加入 {n} 名新成员");
                    self.status
                        .record(status::Level::Info, format!("自动加入 {n} 名新成员"));
                }
                Err(e) => {
                    tracing::warn!("自动加成员失败: {e:#}");
                    self.status
                        .record(status::Level::Warn, format!("自动加成员失败：{e:#}"));
                }
            }
        }
        members::load(&self.members_file)
    }
}

pub struct MembersSync {
    pub collection_id: String,
    pub since: chrono::DateTime<chrono::Utc>,
}

/// MEMBERS_SINCE（YYYY-MM-DD，北京时间）和 OUTLINE_COLLECTION_ID 都设置了才启用自动加成员
fn members_sync() -> Result<Option<MembersSync>> {
    let Ok(since) = std::env::var("MEMBERS_SINCE") else {
        return Ok(None);
    };
    let since = since
        .parse()
        .with_context(|| format!("MEMBERS_SINCE 格式应为 YYYY-MM-DD：{since}"))?;
    Ok(Some(MembersSync {
        collection_id: env("OUTLINE_COLLECTION_ID")?,
        since: report::midnight(since),
    }))
}

fn env(name: &str) -> Result<String> {
    std::env::var(name).with_context(|| format!("缺少环境变量 {name}"))
}

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.into())
}

fn outline_client() -> Result<outline::OutlineClient> {
    Ok(outline::OutlineClient::new(
        env_or("OUTLINE_URL", "https://docs.buptmerak.cn"),
        env("OUTLINE_API_KEY")?,
    ))
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(env_or("RUST_LOG", "info"))
        .with_ansi(std::io::stdout().is_terminal())
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None => run_bot().await,
        Some("report") => run_report(&args[1..]).await,
        Some("sync-members") => run_sync_members().await,
        Some(other) => bail!(
            "未知子命令 {other}。用法：push_bot [report <from> <to> [members.json] [out.png] | sync-members]"
        ),
    }
}

/// 离线生成指定日期区间的报告，用于回看历史数据和调试，例如：
/// `push_bot report 2026-03-14 2026-03-17 members.json out.png`
async fn run_report(args: &[String]) -> Result<()> {
    let [from, to, rest @ ..] = args else {
        bail!("用法：push_bot report <from> <to> [members.json] [out.png]")
    };
    let window = report::Window::dates(from.parse()?, to.parse()?);
    let members_file = rest
        .first()
        .cloned()
        .unwrap_or_else(|| env_or("MEMBERS_FILE", "members.json"));
    let members = members::load(members_file.as_ref())?;
    let outline = outline_client()?;
    let r = report::build(&outline, members, window).await;
    match rest.get(1) {
        Some(png) => std::fs::write(png, renderer().render(&r, &outline).await?)?,
        None => println!("{}", r.to_text()),
    }
    Ok(())
}

/// 手动执行一次自动加成员（读 MEMBERS_FILE、MEMBERS_SINCE、OUTLINE_COLLECTION_ID）
async fn run_sync_members() -> Result<()> {
    let sync = members_sync()?.context("需要设置 MEMBERS_SINCE 和 OUTLINE_COLLECTION_ID")?;
    let file = env_or("MEMBERS_FILE", "members.json");
    let added = members::sync(
        &outline_client()?,
        file.as_ref(),
        &sync.collection_id,
        sync.since,
    )
    .await?;
    println!("新增 {added} 名成员");
    Ok(())
}

fn renderer() -> render::Renderer {
    render::Renderer {
        typst: env_or("TYPST_BIN", "typst"),
        template_dir: env_or("TEMPLATE_DIR", "template").into(),
        font_dir: env_or("FONT_DIR", "fonts").into(),
    }
}

async fn run_bot() -> Result<()> {
    let sandbox = std::env::var("QQ_SANDBOX").is_ok_and(|v| v == "1" || v == "true");
    let app = Arc::new(App {
        qq: qq::QqClient::new(env("QQ_APP_ID")?, env("QQ_APP_SECRET")?, sandbox),
        store: store::Store::load(env_or("DATA_FILE", "subscribers.json").into())?,
        push_key: env("PUSH_KEY")?,
        outline: outline_client()?,
        members_file: env_or("MEMBERS_FILE", "members.json").into(),
        renderer: renderer(),
        members_sync: members_sync()?,
        members_lock: tokio::sync::Mutex::new(()),
        status: status::Status::new(),
        targets: targets::Targets::load(env_or("TARGETS_FILE", "targets.json").into())?,
        daily_at: daily::schedule_from_env()?,
    });

    // 启动前先验证一次凭据，失败直接退出
    app.qq.access_token().await?;

    let addr = env_or("LISTEN_ADDR", "127.0.0.1:8080");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("推送接口监听 http://{addr}/push");

    // 管理页只在设置了 ADMIN_ADDR 时启动，需放在带鉴权的反向代理后面
    if let Ok(admin_addr) = std::env::var("ADMIN_ADDR") {
        let listener = tokio::net::TcpListener::bind(&admin_addr)
            .await
            .with_context(|| format!("监听管理页 {admin_addr}"))?;
        tracing::info!("管理页监听 http://{admin_addr}/");
        let router = admin::router(app.clone());
        tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, router).await {
                tracing::error!("管理页退出: {e:#}");
            }
        });
    }

    // 本地调试管理页时设 PUSH_BOT_NO_GATEWAY=1，避免和线上实例抢同一个 QQ 网关连接
    if std::env::var_os("PUSH_BOT_NO_GATEWAY").is_none() {
        tokio::spawn(gateway::run(app.clone()));
        // 定时战报只在线上实例跑，本地调试不发
        if let Some(at) = app.daily_at {
            tokio::spawn(daily::run(app.clone(), at));
        }
    }
    axum::serve(listener, http::router(app)).await?;
    Ok(())
}
