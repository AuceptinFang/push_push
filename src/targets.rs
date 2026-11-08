//! 见过的会话（发过命令的私聊和群）。QQ 只给 openid，没有群名，所以管理页里可以给它们起备注，
//! 在管理页手动发战报时从这里选目标。

use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::qq::Target;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Known {
    pub target: Target,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub last_seen: DateTime<Utc>,
}

pub struct Targets {
    path: PathBuf,
    list: Mutex<Vec<Known>>,
}

impl Targets {
    pub fn load(path: PathBuf) -> Result<Self> {
        let list = match std::fs::read_to_string(&path) {
            Ok(s) => {
                serde_json::from_str(&s).with_context(|| format!("解析 {}", path.display()))?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e.into()),
        };
        Ok(Self {
            path,
            list: Mutex::new(list),
        })
    }

    async fn save(&self, list: &[Known]) -> Result<()> {
        let tmp = self.path.with_extension("tmp");
        tokio::fs::write(&tmp, serde_json::to_vec_pretty(list)?).await?;
        tokio::fs::rename(&tmp, &self.path).await?;
        Ok(())
    }

    /// 记一次来自 target 的命令
    pub async fn touch(&self, target: &Target) -> Result<()> {
        let mut list = self.list.lock().await;
        match list.iter_mut().find(|k| k.target == *target) {
            Some(k) => k.last_seen = Utc::now(),
            None => list.push(Known {
                target: target.clone(),
                label: None,
                last_seen: Utc::now(),
            }),
        }
        self.save(&list).await
    }

    pub async fn all(&self) -> Vec<Known> {
        self.list.lock().await.clone()
    }

    pub async fn find(&self, id: &str) -> Option<Target> {
        self.list
            .lock()
            .await
            .iter()
            .find(|k| k.target.id() == id)
            .map(|k| k.target.clone())
    }

    /// 返回 false 表示没有这个会话
    pub async fn set_label(&self, id: &str, label: Option<String>) -> Result<bool> {
        let mut list = self.list.lock().await;
        let Some(k) = list.iter_mut().find(|k| k.target.id() == id) else {
            return Ok(false);
        };
        k.label = label.map(|l| l.trim().to_owned()).filter(|l| !l.is_empty());
        self.save(&list).await?;
        Ok(true)
    }
}
