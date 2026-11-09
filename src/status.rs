//! 运行状态：网关连接情况和最近活动，供管理页查看。只在内存里，重启后清空。

use std::collections::VecDeque;
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use serde::Serialize;

const MAX_EVENTS: usize = 200;

#[derive(Debug, Clone, Default, Serialize)]
pub struct Gateway {
    pub online: bool,
    pub bot_name: Option<String>,
    pub connected_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub disconnects: u32,
    /// 服务端要求的例行重连（约每小时一次）进行中，恢复后不记活动
    #[serde(skip)]
    planned: bool,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, Serialize)]
pub struct Event {
    pub at: DateTime<Utc>,
    pub level: Level,
    pub message: String,
}

pub struct Status {
    pub started_at: DateTime<Utc>,
    gateway: Mutex<Gateway>,
    events: Mutex<VecDeque<Event>>,
}

impl Status {
    pub fn new() -> Self {
        Self {
            started_at: Utc::now(),
            gateway: Mutex::default(),
            events: Mutex::default(),
        }
    }

    pub fn record(&self, level: Level, message: impl Into<String>) {
        let mut events = self.events.lock().unwrap();
        events.push_back(Event {
            at: Utc::now(),
            level,
            message: message.into(),
        });
        if events.len() > MAX_EVENTS {
            events.pop_front();
        }
    }

    /// 最近的活动，新的在前
    pub fn events(&self) -> Vec<Event> {
        self.events.lock().unwrap().iter().rev().cloned().collect()
    }

    pub fn gateway(&self) -> Gateway {
        self.gateway.lock().unwrap().clone()
    }

    pub fn online(&self, bot_name: Option<String>) {
        let mut g = self.gateway.lock().unwrap();
        if bot_name.is_some() {
            g.bot_name = bot_name;
        }
        if std::mem::take(&mut g.planned) && g.online {
            return;
        }
        g.online = true;
        g.connected_at = Some(Utc::now());
        drop(g);
        self.record(Level::Info, "网关已连接");
    }

    /// 服务端要求重连：马上就会 Resume 回来，状态保持在线，不记活动
    pub fn reconnecting(&self) {
        self.gateway.lock().unwrap().planned = true;
    }

    pub fn offline(&self, error: String) {
        let mut g = self.gateway.lock().unwrap();
        g.planned = false;
        g.online = false;
        g.disconnects += 1;
        g.last_error = Some(error.clone());
        drop(g);
        self.record(Level::Warn, format!("网关断开：{error}"));
    }
}
