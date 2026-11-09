//! push 报告：找出时间窗口内每个成员进度文档的新增内容。

use anyhow::Result;
use chrono::{DateTime, Days, FixedOffset, NaiveDate, TimeZone, Utc};
use futures_util::{StreamExt, stream};
use similar::{ChangeTag, TextDiff};

use crate::members::{Member, display_name, doc_id};
use crate::outline::{Document, OutlineClient, User};

pub fn tz() -> FixedOffset {
    FixedOffset::east_opt(8 * 3600).unwrap()
}

/// 北京时间自然日窗口 [start, end)
#[derive(Debug, Clone, Copy)]
pub struct Window {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    /// end 是否就是“现在”。是的话取文档当前正文，避免最新改动还没生成版本
    pub until_now: bool,
}

impl Window {
    /// 最近 days 个自然日（含今天），days < 1 按 1 算
    pub fn recent_days(days: u32) -> Self {
        let now = Utc::now();
        let today = now.with_timezone(&tz()).date_naive();
        let first = today - Days::new(u64::from(days.max(1) - 1));
        Self {
            start: midnight(first),
            end: now,
            until_now: true,
        }
    }

    /// 指定日期区间 [from, to]，都按整天算，用于回看历史数据
    pub fn dates(from: NaiveDate, to: NaiveDate) -> Self {
        let end = midnight(to + Days::new(1));
        let now = Utc::now();
        Self {
            start: midnight(from),
            end: end.min(now),
            until_now: end >= now,
        }
    }

    pub fn first_day(&self) -> NaiveDate {
        self.start.with_timezone(&tz()).date_naive()
    }

    pub fn last_day(&self) -> NaiveDate {
        (self.end - chrono::Duration::seconds(1))
            .with_timezone(&tz())
            .date_naive()
    }
}

/// 北京时间 d 日 0 点
pub fn midnight(d: NaiveDate) -> DateTime<Utc> {
    tz().from_local_datetime(&d.and_hms_opt(0, 0, 0).unwrap())
        .unwrap()
        .with_timezone(&Utc)
}

#[derive(Debug)]
pub enum Status {
    Updated {
        /// 新增行，已去掉 markdown 转义，用于文字版
        added: Vec<String>,
        /// 连续新增的原始 markdown 段落，用于出图
        hunks: Vec<String>,
        removed: usize,
        revisions: usize,
        at: DateTime<Utc>,
    },
    Idle,
}

#[derive(Debug)]
pub struct Entry {
    pub name: String,
    pub user: Option<User>,
    pub doc_title: String,
    pub doc_url: String,
    pub updated_at: Option<DateTime<Utc>>,
    pub status: Status,
}

/// 读取失败的文档：不进战报，由调用方记到管理页的最近活动
#[derive(Debug)]
pub struct Failed {
    pub name: String,
    pub error: String,
}

pub struct Report {
    pub window: Window,
    pub entries: Vec<Entry>,
    pub failed: Vec<Failed>,
}

pub async fn build(outline: &OutlineClient, members: Vec<Member>, window: Window) -> Report {
    let results: Vec<Result<Option<Entry>, Failed>> =
        stream::iter(members.into_iter().filter(|m| !m.ignore))
            .map(|m| async move {
                member_entry(outline, &m, window).await.map_err(|e| {
                    // 读取失败时拿不到标题，用 alias 或自动加入时记下的标题，再不行就显示链接
                    let name = display_name(
                        m.alias.as_deref(),
                        None,
                        m.title.as_deref().unwrap_or_default(),
                    )
                    .unwrap_or_else(|| m.doc.clone());
                    tracing::warn!("{name} 读取失败，不计入战报: {e:#}");
                    Failed {
                        name,
                        error: format!("{e:#}"),
                    }
                })
            })
            .buffered(4)
            .collect()
            .await;
    let mut entries = Vec::new();
    let mut failed = Vec::new();
    for r in results {
        match r {
            Ok(e) => entries.extend(e),
            Err(f) => failed.push(f),
        }
    }
    // 有更新的排前面，最近更新的在前；未更新的接在后面，最久没更新的在前（最该被 push）
    entries.sort_by(|a, b| {
        let group = |e: &Entry| match e.status {
            Status::Updated { .. } => 0,
            Status::Idle => 1,
        };
        group(a).cmp(&group(b)).then_with(|| match a.status {
            Status::Updated { .. } => b.updated_at.cmp(&a.updated_at),
            Status::Idle => a.updated_at.cmp(&b.updated_at),
        })
    });
    Report {
        window,
        entries,
        failed,
    }
}

/// 定不出显示名（见 members::display_name）时返回 None，这篇文档不进战报
async fn member_entry(outline: &OutlineClient, m: &Member, w: Window) -> Result<Option<Entry>> {
    let doc = outline.document(doc_id(&m.doc)).await?;
    let Some(name) = display_name(m.alias.as_deref(), Some(&doc.created_by.name), &doc.title)
    else {
        tracing::warn!(
            "文档「{}」({}) 没有 alias，账号名像真名，标题也不是「xxx的进度」，跳过；补上 alias 后才会统计",
            doc.title,
            m.doc
        );
        return Ok(None);
    };
    let status = doc_status(outline, &doc, w).await?;
    // 有更新时显示窗口内最后一次修改的时间，否则显示文档最后修改时间
    let updated_at = match &status {
        Status::Updated { at, .. } => *at,
        _ => doc.updated_at,
    };
    Ok(Some(Entry {
        name,
        doc_url: outline.absolute(&doc.url),
        doc_title: doc.title,
        updated_at: Some(updated_at),
        user: Some(doc.created_by),
        status,
    }))
}

async fn doc_status(outline: &OutlineClient, doc: &Document, w: Window) -> Result<Status> {
    if doc.updated_at < w.start {
        return Ok(Status::Idle);
    }
    // 倒序翻版本，找窗口内最新的版本(head) 和窗口开始前最后一个版本(base)
    const PAGE: usize = 25;
    let mut head = None;
    let mut base = None;
    let mut in_window = 0;
    let mut offset = 0;
    'pages: loop {
        let page = outline.revisions(&doc.id, offset, PAGE).await?;
        for r in &page {
            if r.created_at >= w.end {
                continue;
            }
            if r.created_at < w.start {
                base = Some(r.id.clone());
                break 'pages;
            }
            in_window += 1;
            head.get_or_insert_with(|| (r.id.clone(), r.created_at));
        }
        if page.len() < PAGE {
            break;
        }
        offset += PAGE;
    }

    let (new_text, at) = match (w.until_now, &head) {
        (true, _) => (doc.text.clone(), doc.updated_at),
        (false, Some((id, at))) => (outline.revision_text(id).await?, *at),
        (false, None) => return Ok(Status::Idle),
    };
    let old_text = match &base {
        Some(b) => outline.revision_text(b).await?,
        None => String::new(),
    };

    let title_line = format!("# {}", doc.title);
    let mut added = Vec::new();
    let mut removed = 0;
    // 连续新增的行（中间可夹空行）合成一段原始 markdown，交给模板按文档样式渲染
    let mut hunks: Vec<String> = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    let diff = TextDiff::from_lines(&old_text, &new_text);
    for change in diff.iter_all_changes() {
        let line = change.value().trim_end();
        // Outline 用单独一个 `\` 表示空段落；只有 `#` 的空标题也没有内容，都按空行处理
        if line.trim().is_empty() || line.trim() == "\\" || line.trim().chars().all(|c| c == '#') {
            if change.tag() == ChangeTag::Insert && !current.is_empty() {
                current.push("");
            }
            continue;
        }
        if line == title_line {
            continue;
        }
        match change.tag() {
            ChangeTag::Insert => {
                added.push(unescape(line));
                current.push(line);
            }
            ChangeTag::Delete => removed += 1,
            ChangeTag::Equal => flush_hunk(&mut current, &mut hunks),
        }
    }
    flush_hunk(&mut current, &mut hunks);
    if added.is_empty() && removed == 0 {
        return Ok(Status::Idle);
    }
    Ok(Status::Updated {
        added,
        hunks,
        removed,
        revisions: in_window,
        at,
    })
}

fn flush_hunk(current: &mut Vec<&str>, hunks: &mut Vec<String>) {
    while current.last() == Some(&"") {
        current.pop();
    }
    if !current.is_empty() {
        hunks.push(current.join("\n"));
        current.clear();
    }
}

/// 去掉 markdown 转义：`\~` -> `~`，单独一个 `\`（硬换行）去掉
fn unescape(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.peek() {
                Some(n) if n.is_ascii_punctuation() => out.push(chars.next().unwrap()),
                None => {}
                _ => out.push(c),
            }
        } else {
            out.push(c);
        }
    }
    out
}

const MAX_LINES: usize = 6;
const MAX_CHARS: usize = 60;

impl Report {
    pub fn title(&self) -> String {
        let (a, b) = (self.window.first_day(), self.window.last_day());
        if a == b {
            format!("{}", a.format("%m/%d"))
        } else {
            format!("{}–{}", a.format("%m/%d"), b.format("%m/%d"))
        }
    }

    /// 纯文本版，出图前先用这个
    pub fn to_text(&self) -> String {
        if self.entries.is_empty() {
            return "成员名单为空。".to_owned();
        }
        let updated: Vec<_> = self
            .entries
            .iter()
            .filter(|e| matches!(e.status, Status::Updated { .. }))
            .collect();
        let mut out = format!(
            "📮 push 战报 · {}\n已更新 {}/{}\n",
            self.title(),
            updated.len(),
            self.entries.len()
        );
        for e in &updated {
            let Status::Updated { added, removed, .. } = &e.status else {
                unreachable!()
            };
            let when = e
                .updated_at
                .map(|t| t.with_timezone(&tz()).format("%m/%d %H:%M").to_string())
                .unwrap_or_default();
            out += &format!("\n✅ {}（{when}，+{} -{}）\n", e.name, added.len(), removed);
            for line in added.iter().take(MAX_LINES) {
                let line = &text_images(line.trim_start());
                let short: String = line.chars().take(MAX_CHARS).collect();
                let ellipsis = if short.len() < line.len() { "…" } else { "" };
                out += &format!("  {short}{ellipsis}\n");
            }
            if added.len() > MAX_LINES {
                out += &format!("  …还有 {} 行\n", added.len() - MAX_LINES);
            }
        }
        let idle: Vec<_> = self
            .entries
            .iter()
            .filter(|e| matches!(e.status, Status::Idle))
            .map(|e| e.name.as_str())
            .collect();
        if !idle.is_empty() {
            out += &format!("\n😴 未更新：{}\n", idle.join("、"));
        }
        out
    }
}

/// 文字版里把图片 markdown 换成「[图片]」
fn text_images(line: &str) -> String {
    let mut out = String::new();
    let mut last = 0;
    for img in crate::render::find_images(line) {
        out += &line[last..img.range.start];
        out += "[图片]";
        last = img.range.end;
    }
    out + &line[last..]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unescapes_markdown() {
        assert_eq!(unescape(r"好耶\~"), "好耶~");
        assert_eq!(unescape(r"@\~@ a\*b"), "@~@ a*b");
        assert_eq!(unescape(r"\"), "");
        assert_eq!(unescape(r"C:\path"), r"C:\path");
    }
}
