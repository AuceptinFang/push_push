//! QQ 开放平台 API v2 的 HTTP 部分：AccessToken 获取/缓存、网关地址、发消息。

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::Mutex;

const TOKEN_URL: &str = "https://bots.qq.com/app/getAppAccessToken";
const API_BASE: &str = "https://api.sgroup.qq.com";
const SANDBOX_API_BASE: &str = "https://sandbox.api.sgroup.qq.com";

/// 消息发送目标。openid 是机器人维度的，只能从收到的事件里拿到。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, Deserialize)]
#[serde(tag = "kind", content = "openid", rename_all = "lowercase")]
pub enum Target {
    User(String),
    Group(String),
}

impl Target {
    pub fn id(&self) -> String {
        match self {
            Target::User(o) => format!("user:{o}"),
            Target::Group(o) => format!("group:{o}"),
        }
    }
}

pub struct QqClient {
    app_id: String,
    app_secret: String,
    api_base: &'static str,
    http: reqwest::Client,
    token: Mutex<Option<(String, Instant)>>,
    msg_seq: AtomicU32,
}

#[derive(Deserialize)]
struct TokenResp {
    access_token: Option<String>,
    // 官方返回的是字符串 "7200"，这里两种都兼容
    expires_in: Option<Value>,
    code: Option<i64>,
    message: Option<String>,
}

impl QqClient {
    pub fn new(app_id: String, app_secret: String, sandbox: bool) -> Self {
        Self {
            app_id,
            app_secret,
            api_base: if sandbox { SANDBOX_API_BASE } else { API_BASE },
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(60))
                .build()
                .expect("build http client"),
            token: Mutex::new(None),
            msg_seq: AtomicU32::new(1),
        }
    }

    /// 返回有效的 AccessToken，剩余有效期不足 60 秒时自动刷新。
    pub async fn access_token(&self) -> Result<String> {
        let mut guard = self.token.lock().await;
        if let Some((tok, exp)) = guard.as_ref()
            && Instant::now() + Duration::from_secs(60) < *exp
        {
            return Ok(tok.clone());
        }
        let resp: TokenResp = self
            .http
            .post(TOKEN_URL)
            .json(&json!({ "appId": self.app_id, "clientSecret": self.app_secret }))
            .send()
            .await?
            .json()
            .await
            .context("parse token response")?;
        let Some(tok) = resp.access_token else {
            bail!(
                "获取 AccessToken 失败: code={:?} message={:?}（11298 通常是 IP 白名单未配置）",
                resp.code,
                resp.message
            );
        };
        let secs = match resp.expires_in {
            Some(Value::String(s)) => s.parse().unwrap_or(7200),
            Some(Value::Number(n)) => n.as_u64().unwrap_or(7200),
            _ => 7200,
        };
        tracing::info!("AccessToken 已刷新，有效期 {secs}s");
        *guard = Some((tok.clone(), Instant::now() + Duration::from_secs(secs)));
        Ok(tok)
    }

    async fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value> {
        let token = self.access_token().await?;
        let mut req = self
            .http
            .request(method, format!("{}{}", self.api_base, path))
            .header("Authorization", format!("QQBot {token}"))
            .header("X-Union-Appid", &self.app_id);
        if let Some(b) = body {
            req = req.json(&b);
        }
        let resp = req.send().await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            bail!("{path} -> HTTP {status}: {text}");
        }
        Ok(serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    pub async fn gateway_url(&self) -> Result<String> {
        let v = self.request(reqwest::Method::GET, "/gateway", None).await?;
        v["url"]
            .as_str()
            .map(str::to_owned)
            .context("gateway 响应缺少 url")
    }

    fn base_path(target: &Target) -> String {
        match target {
            Target::User(o) => format!("/v2/users/{o}"),
            Target::Group(o) => format!("/v2/groups/{o}"),
        }
    }

    /// 发送图片：先以 base64 上传拿到 file_info（单文件上限 20MB），再发 msg_type=7 的富媒体消息。
    pub async fn send_image(
        &self,
        target: &Target,
        png: &[u8],
        reply_to: Option<&str>,
    ) -> Result<()> {
        use base64::Engine;
        let base = Self::base_path(target);
        let uploaded = self
            .request(
                reqwest::Method::POST,
                &format!("{base}/files"),
                Some(json!({
                    "file_type": 1,
                    "file_data": base64::engine::general_purpose::STANDARD.encode(png),
                    "srv_send_msg": false,
                })),
            )
            .await?;
        let file_info = uploaded["file_info"]
            .as_str()
            .context("上传图片响应缺少 file_info")?;
        let mut body = json!({ "msg_type": 7, "media": { "file_info": file_info } });
        self.set_reply(&mut body, reply_to);
        self.request(
            reqwest::Method::POST,
            &format!("{base}/messages"),
            Some(body),
        )
        .await?;
        Ok(())
    }

    fn set_reply(&self, body: &mut Value, reply_to: Option<&str>) {
        if let Some(id) = reply_to {
            body["msg_id"] = json!(id);
            // 同一 msg_id 多次回复需要不同的 msg_seq
            body["msg_seq"] = json!(self.msg_seq.fetch_add(1, Ordering::Relaxed));
        }
    }

    /// 发送文本消息。`reply_to` 为收到的消息 id 时是被动回复，为 None 时是主动消息。
    pub async fn send_text(
        &self,
        target: &Target,
        content: &str,
        reply_to: Option<&str>,
    ) -> Result<()> {
        let path = format!("{}/messages", Self::base_path(target));
        let mut body = json!({ "content": content, "msg_type": 0 });
        self.set_reply(&mut body, reply_to);
        self.request(reqwest::Method::POST, &path, Some(body))
            .await?;
        Ok(())
    }
}
