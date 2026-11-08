//! QQ 内的命令。`#push`、`/push`、`@机器人 push` 都会触发（群里 @ 部分已被平台去掉）。
//!
//!   push [num]   最近 num 个自然日（含今天）的进度更新，默认今天
//!
//! 定时推送的订阅在管理页里设置。

use anyhow::Result;

use crate::App;
use crate::qq::Target;
use crate::report::{self, Window};
use crate::status::Level;

const MAX_DAYS: u32 = 30;

/// 解析出要统计的天数；不是 push 命令时返回 None
fn parse(content: &str) -> Option<u32> {
    let mut words = content.split_whitespace();
    let head = words.next()?;
    if !head
        .trim_start_matches(['#', '/'])
        .eq_ignore_ascii_case("push")
    {
        return None;
    }
    Some(
        words
            .next()
            .and_then(|n| n.parse::<u32>().ok())
            .unwrap_or(1)
            .clamp(1, MAX_DAYS),
    )
}

pub async fn handle(app: &App, target: Target, content: &str, msg_id: &str) -> Result<()> {
    let Some(days) = parse(content) else {
        return Ok(());
    };
    tracing::info!("{} 执行 {content:?}", target.id());
    app.status
        .record(Level::Info, format!("{} 执行 {content:?}", target.id()));
    if let Err(e) = app.targets.touch(&target).await {
        tracing::warn!("记录会话失败: {e:#}");
    }
    let members = app.current_members().await?;
    let report = report::build(&app.outline, members, Window::recent_days(days)).await;
    // 出图失败（渲染或上传）时退回文字版
    let sent = match app.renderer.render(&report, &app.outline).await {
        Ok(png) => app.qq.send_image(&target, &png, Some(msg_id)).await,
        Err(e) => Err(e),
    };
    if let Err(e) = sent {
        tracing::warn!("发送图片失败，改发文字: {e:#}");
        app.status.record(
            Level::Error,
            format!("给 {} 发图失败，改发文字：{e:#}", target.id()),
        );
        app.qq
            .send_text(&target, &report.to_text(), Some(msg_id))
            .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_prefixes_and_days() {
        assert_eq!(parse("#push"), Some(1));
        assert_eq!(parse("/push 3"), Some(3));
        assert_eq!(parse("push 7"), Some(7));
        assert_eq!(parse("PUSH 0"), Some(1));
        assert_eq!(parse("push abc"), Some(1));
        assert_eq!(parse("push 999"), Some(MAX_DAYS));
        assert_eq!(parse("pushy"), None);
        assert_eq!(parse("hello push"), None);
        assert_eq!(parse("#push sub"), Some(1));
    }
}
