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
        g.online = true;
        g.connected_at = Some(Utc::now());
        if bot_name.is_some() {
            g.bot_name = bot_name;
        }
        drop(g);
        self.record(Level::Info, "网关已连接");
    }

    pub fn offline(&self, error: String) {
        let mut g = self.gateway.lock().unwrap();
        g.online = false;
        g.disconnects += 1;
        g.last_error = Some(error.clone());
        drop(g);
        self.record(Level::Warn, format!("网关断开：{error}"));
    }
}
