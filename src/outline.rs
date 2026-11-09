//! Outline 知识库 API 客户端（只读）。

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use reqwest::Url;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::sync::Mutex;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct User {
    pub id: String,
    pub name: String,
    /// 可能是站内相对路径（/api/files.get?...），也可能为空
    pub avatar_url: Option<String>,
    pub color: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Document {
    pub id: String,
    pub url_id: String,
    pub title: String,
    pub url: String,
    pub text: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub created_by: User,
    pub collection_id: Option<String>,
    pub parent_document_id: Option<String>,
}

/// documents.list 返回的文档摘要（只取用到的字段）
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocSummary {
    pub id: String,
    pub url_id: String,
    pub title: String,
    pub url: String,
    pub parent_document_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Revision {
    pub id: String,
    pub created_at: DateTime<Utc>,
}

/// 除 Outline 本站外允许下载的图床（含子域名）。目前只有 QQ 头像
const EXTRA_IMAGE_HOSTS: &[&str] = &["qlogo.cn"];

/// 文档和头像里的链接由成员填写，只访问 Outline 本站和 EXTRA_IMAGE_HOSTS，重定向的每一跳也按这个查，
/// 免得机器人替人去请求内网或任意网址
fn resource_allowed(origin: &Url, url: &Url) -> bool {
    if url.origin() == origin.origin() {
        return true;
    }
    matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some_and(|h| {
            EXTRA_IMAGE_HOSTS
                .iter()
                .any(|d| h == *d || h.strip_suffix(d).is_some_and(|p| p.ends_with('.')))
        })
}

pub struct OutlineClient {
    base: String,
    origin: Url,
    key: String,
    http: reqwest::Client,
    /// 历史版本不会再变，正文按 revision id 缓存
    revision_text: Mutex<HashMap<String, String>>,
}

impl OutlineClient {
    pub fn new(base: String, key: String) -> Result<Self> {
        let base = base.trim_end_matches('/').to_owned();
        let origin = Url::parse(&base).with_context(|| format!("OUTLINE_URL 无效：{base}"))?;
        let policy_origin = origin.clone();
        let redirect = reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= 5 {
                attempt.error("重定向次数过多")
            } else if resource_allowed(&policy_origin, attempt.url()) {
                attempt.follow()
            } else {
                let msg = format!("拒绝重定向到 {}", attempt.url());
                attempt.error(msg)
            }
        });
        Ok(Self {
            base,
            origin,
            key,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .redirect(redirect)
                .build()
                .context("build http client")?,
            revision_text: Mutex::new(HashMap::new()),
        })
    }

    /// 站内相对路径转绝对 URL（头像等）
    pub fn absolute(&self, path: &str) -> String {
        if path.starts_with("http") {
            path.to_owned()
        } else {
            format!("{}{}", self.base, path)
        }
    }

    /// 下载头像、文档附件等资源，只限本站和 EXTRA_IMAGE_HOSTS。
    /// 只对本站链接带 API Key（附件需要鉴权），外站不带，避免泄露 Key
    pub async fn fetch(&self, url: &str) -> Result<Vec<u8>> {
        const MAX_BYTES: usize = 15 * 1024 * 1024;
        let url = Url::parse(url).with_context(|| format!("链接无效：{url}"))?;
        if !resource_allowed(&self.origin, &url) {
            bail!("不是 Outline 本站的地址，不下载：{url}");
        }
        let same_origin = url.origin() == self.origin.origin();
        let mut req = self.http.get(url);
        if same_origin {
            req = req.bearer_auth(&self.key);
        }
        let resp = req.send().await?.error_for_status()?;
        if resp
            .content_length()
            .is_some_and(|n| n as usize > MAX_BYTES)
        {
            bail!("文件超过 {MAX_BYTES} 字节");
        }
        let bytes = resp.bytes().await?;
        if bytes.len() > MAX_BYTES {
            bail!("文件超过 {MAX_BYTES} 字节");
        }
        Ok(bytes.to_vec())
    }

    async fn call<T: DeserializeOwned>(&self, method: &str, body: Value) -> Result<T> {
        let resp = self
            .http
            .post(format!("{}/api/{method}", self.base))
            .bearer_auth(&self.key)
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        let v: Value = resp
            .json()
            .await
            .with_context(|| format!("{method} 响应不是 JSON"))?;
        if !status.is_success() || v["ok"] != json!(true) {
            bail!(
                "{method} -> HTTP {status}: {}",
                v["message"].as_str().unwrap_or("?")
            );
        }
        serde_json::from_value(v["data"].clone()).with_context(|| format!("解析 {method} 响应"))
    }

    /// id 可以是 uuid、urlId 或文档链接里的 slug
    pub async fn document(&self, id: &str) -> Result<Document> {
        self.call("documents.info", json!({ "id": id })).await
    }

    /// collection 里 since 之后新建的已发布文档，按创建时间升序
    pub async fn documents_created_since(
        &self,
        collection_id: &str,
        since: DateTime<Utc>,
    ) -> Result<Vec<DocSummary>> {
        const PAGE: usize = 100;
        let mut out = Vec::new();
        loop {
            let page: Vec<DocSummary> = self
                .call(
                    "documents.list",
                    json!({
                        "filters": [
                            { "field": "collectionId", "operator": "eq", "value": collection_id },
                            { "field": "createdAt", "operator": "gte", "value": since.to_rfc3339() },
                        ],
                        "sort": "createdAt",
                        "direction": "ASC",
                        "offset": out.len(),
                        "limit": PAGE,
                    }),
                )
                .await?;
            let n = page.len();
            out.extend(page);
            if n < PAGE {
                return Ok(out);
            }
        }
    }

    /// 按时间倒序分页列出版本
    pub async fn revisions(
        &self,
        document_id: &str,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<Revision>> {
        self.call(
            "revisions.list",
            json!({ "documentId": document_id, "offset": offset, "limit": limit, "sort": "createdAt", "direction": "DESC" }),
        )
        .await
    }

    pub async fn revision_text(&self, id: &str) -> Result<String> {
        if let Some(t) = self.revision_text.lock().await.get(id) {
            return Ok(t.clone());
        }
        #[derive(Deserialize)]
        struct R {
            text: String,
        }
        let r: R = self.call("revisions.info", json!({ "id": id })).await?;
        self.revision_text
            .lock()
            .await
            .insert(id.to_owned(), r.text.clone());
        Ok(r.text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_outline_and_image_hosts() {
        let origin = Url::parse("https://docs.example.com").unwrap();
        let ok = |u: &str| resource_allowed(&origin, &Url::parse(u).unwrap());
        assert!(ok("https://docs.example.com/api/files.get?key=x"));
        assert!(ok("https://docs.example.com:443/a"));
        assert!(ok("http://q1.qlogo.cn/g?b=qq&nk=1"));
        assert!(ok("https://qlogo.cn/a"));
        // 换协议、端口、相似域名、内网都不行
        assert!(!ok("http://docs.example.com/a"));
        assert!(!ok("https://docs.example.com:8443/a"));
        assert!(!ok("https://docs.example.com.evil.com/a"));
        assert!(!ok("https://docs.example.com@evil.com/a"));
        assert!(!ok("https://evilqlogo.cn/a"));
        assert!(!ok("http://127.0.0.1:8088/api/members"));
        assert!(!ok("http://100.120.87.76:8088/"));
        assert!(!ok("file:///etc/passwd"));
    }
}
