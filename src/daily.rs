//! 每天定时把过去 24 小时的战报以主动消息发给所有订阅者（在管理页勾选）。
//! 时间按机器本地时区。DAILY_PUSH_AT 默认 22:00，设为 off 关闭。

use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::{DateTime, Days, Local, NaiveTime, TimeZone, Utc};

use crate::App;
use crate::report::Window;
use crate::status::Level;

const WINDOW_HOURS: i64 = 24;

pub fn schedule_from_env() -> Result<Option<NaiveTime>> {
    let value = std::env::var("DAILY_PUSH_AT").unwrap_or_else(|_| "22:00".into());
    if value.eq_ignore_ascii_case("off") {
        return Ok(None);
    }
    NaiveTime::parse_from_str(&value, "%H:%M")
        .map(Some)
        .with_context(|| format!("DAILY_PUSH_AT 格式应为 HH:MM 或 off：{value}"))
}

/// 到点 due 时发的战报窗口：due 往前 24 小时，截至 now。到点时 now≈due；提前调用就是预览「到现在为止会发什么」
pub fn window(due: DateTime<Local>, now: DateTime<Local>) -> Window {
    Window {
        start: (due - chrono::Duration::hours(WINDOW_HOURS)).with_timezone(&Utc),
        end: now.with_timezone(&Utc),
        until_now: true,
    }
}

/// 下一次定时战报到现在为止的内容，管理页默认预览这个
pub fn upcoming(at: NaiveTime) -> Window {
    let now = Local::now();
    window(next_run(now, at), now)
}

/// now 之后最近的一个 at 时刻
pub fn next_run(now: DateTime<Local>, at: NaiveTime) -> DateTime<Local> {
    let mut day = now.date_naive();
    loop {
        // 夏令时跳过的时刻没有对应的本地时间，顺延到下一天
        if let Some(t) = Local.from_local_datetime(&day.and_time(at)).earliest()
            && t > now
        {
            return t;
        }
        day = day + Days::new(1);
    }
}

pub async fn run(app: Arc<App>, at: NaiveTime) {
    loop {
        let next = next_run(Local::now(), at);
        tracing::info!("下一次定时战报：{next}");
        let wait = (next - Local::now()).to_std().unwrap_or_default();
        tokio::time::sleep(wait).await;
        if let Err(e) = send(&app, window(next, Local::now())).await {
            tracing::warn!("定时战报失败: {e:#}");
            app.status
                .record(Level::Error, format!("定时战报失败：{e:#}"));
        }
    }
}

async fn send(app: &App, window: Window) -> Result<()> {
    let subscribers = app.store.all().await;
    if subscribers.is_empty() {
        app.status.record(Level::Warn, "定时战报：没有订阅者，跳过");
        return Ok(());
    }
    let members = app.current_members().await?;
    if members.iter().all(|m| m.ignore) {
        app.status
            .record(Level::Info, "定时战报：成员名单为空，跳过");
        return Ok(());
    }
    let report = app.build_report(members, window).await;
    let png = match app.renderer.render(&report, &app.outline).await {
        Ok(png) => Some(png),
        Err(e) => {
            app.status
                .record(Level::Error, format!("定时战报出图失败，改发文字：{e:#}"));
            None
        }
    };
    for target in &subscribers {
        let sent = match &png {
            Some(png) => app.qq.send_image(target, png, None).await,
            None => app.qq.send_text(target, &report.to_text(), None).await,
        };
        match sent {
            Ok(()) => app
                .status
                .record(Level::Info, format!("定时战报已发到 {}", target.id())),
            Err(e) => app.status.record(
                Level::Error,
                format!("定时战报发到 {} 失败：{e:#}", target.id()),
            ),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_run_is_today_or_tomorrow() {
        let at = NaiveTime::from_hms_opt(22, 0, 0).unwrap();
        let morning = Local.with_ymd_and_hms(2026, 10, 27, 9, 0, 0).unwrap();
        assert_eq!(
            next_run(morning, at),
            Local.with_ymd_and_hms(2026, 10, 27, 22, 0, 0).unwrap()
        );
        let exactly = Local.with_ymd_and_hms(2026, 10, 27, 22, 0, 0).unwrap();
        assert_eq!(
            next_run(exactly, at),
            Local.with_ymd_and_hms(2026, 10, 28, 22, 0, 0).unwrap()
        );
        let night = Local.with_ymd_and_hms(2026, 10, 27, 23, 30, 0).unwrap();
        assert_eq!(
            next_run(night, at),
            Local.with_ymd_and_hms(2026, 10, 28, 22, 0, 0).unwrap()
        );
    }

    #[test]
    fn window_covers_24h_before_due_until_now() {
        let due = Local.with_ymd_and_hms(2026, 10, 28, 22, 0, 0).unwrap();
        let now = Local.with_ymd_and_hms(2026, 10, 28, 14, 30, 0).unwrap();
        let w = window(due, now);
        assert_eq!(
            w.start,
            Local.with_ymd_and_hms(2026, 10, 27, 22, 0, 0).unwrap()
        );
        assert_eq!(w.end, now);
        assert!(w.until_now);
    }

    #[test]
    fn parses_schedule() {
        assert!(NaiveTime::parse_from_str("22:00", "%H:%M").is_ok());
        assert!(NaiveTime::parse_from_str("25:00", "%H:%M").is_err());
    }
}
