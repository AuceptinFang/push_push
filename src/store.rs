//! 订阅者列表，持久化为 JSON 文件。

use std::path::PathBuf;

use anyhow::Result;
use tokio::sync::Mutex;

use crate::qq::Target;

pub struct Store {
    path: PathBuf,
    subs: Mutex<Vec<Target>>,
}

impl Store {
    pub fn load(path: PathBuf) -> Result<Self> {
        let subs = match std::fs::read_to_string(&path) {
            Ok(s) => serde_json::from_str(&s)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e.into()),
        };
        Ok(Self {
            path,
            subs: Mutex::new(subs),
        })
    }

    async fn save(&self, subs: &[Target]) -> Result<()> {
        let tmp = self.path.with_extension("tmp");
        tokio::fs::write(&tmp, serde_json::to_vec_pretty(subs)?).await?;
        tokio::fs::rename(&tmp, &self.path).await?;
        Ok(())
    }

    /// 返回 true 表示新增。
    pub async fn add(&self, t: Target) -> Result<bool> {
        let mut subs = self.subs.lock().await;
        if subs.contains(&t) {
            return Ok(false);
        }
        subs.push(t);
        self.save(&subs).await?;
        Ok(true)
    }

    /// 返回 true 表示确实删除了。
    pub async fn remove(&self, t: &Target) -> Result<bool> {
        let mut subs = self.subs.lock().await;
        let before = subs.len();
        subs.retain(|s| s != t);
        if subs.len() == before {
            return Ok(false);
        }
        self.save(&subs).await?;
        Ok(true)
    }

    pub async fn contains(&self, t: &Target) -> bool {
        self.subs.lock().await.contains(t)
    }

    pub async fn all(&self) -> Vec<Target> {
        self.subs.lock().await.clone()
    }
}
