//! 用 Typst 模板把 Report 渲染成 PNG。
//!
//! 每次渲染建一个临时目录，写入 data.json 和下载好的头像，再调用 typst CLI。
//! 模板目录里需要 report.typ、bg.png 和 packages/（cmarker），字体只从 FONT_DIR 加载，保证各处出图一致。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use futures_util::{StreamExt, stream};
use serde::Serialize;

use crate::outline::OutlineClient;
use crate::report::{Report, Status, tz};

/// QQ 单个文件上传上限是 20MB，留点余量。超过时降低分辨率重新渲染
const MAX_PNG_BYTES: usize = 18 * 1024 * 1024;
const PPI_STEPS: [u32; 3] = [144, 108, 72];

pub struct Renderer {
    pub typst: String,
    pub template_dir: PathBuf,
    pub font_dir: PathBuf,
}

#[derive(Serialize)]
struct Data {
    bg: String,
    range: String,
    updated: usize,
    total: usize,
    entries: Vec<EntryData>,
    idle: Vec<String>,
    footer: String,
}

#[derive(Serialize)]
struct EntryData {
    name: String,
    initial: String,
    color: String,
    avatar: Option<String>,
    time: String,
    added: usize,
    removed: usize,
    /// 新增内容的原始 markdown（全部，不截断），由模板里的 cmarker 渲染
    markdown: String,
}

impl Renderer {
    pub async fn render(&self, report: &Report, outline: &OutlineClient) -> Result<Vec<u8>> {
        let dir =
            std::env::temp_dir().join(format!("push_bot-{}-{}", std::process::id(), rand_suffix()));
        tokio::fs::create_dir_all(&dir).await?;
        let result = self.render_in(&dir, report, outline).await;
        // 调试用：设置 PUSH_BOT_KEEP_RENDER 时保留临时目录（data.json、头像、图片）
        if std::env::var_os("PUSH_BOT_KEEP_RENDER").is_some() {
            tracing::info!("渲染目录保留在 {}", dir.display());
        } else {
            let _ = tokio::fs::remove_dir_all(&dir).await;
        }
        result
    }

    async fn render_in(
        &self,
        dir: &Path,
        report: &Report,
        outline: &OutlineClient,
    ) -> Result<Vec<u8>> {
        let template_dir = std::path::absolute(&self.template_dir)?;
        let updated: Vec<_> = report
            .entries
            .iter()
            .filter(|e| matches!(e.status, Status::Updated { .. }))
            .collect();

        // 先取出拥有所有权的 (url, 保存路径)，避免 stream 闭包借用 entry 导致 future 不满足 Send 的生命周期约束
        let jobs: Vec<Option<(String, PathBuf)>> = updated
            .iter()
            .enumerate()
            .map(|(i, e)| {
                let url = e.user.as_ref()?.avatar_url.as_deref()?;
                Some((outline.absolute(url), dir.join(format!("avatar{i}"))))
            })
            .collect();
        let avatars: Vec<Option<String>> = stream::iter(jobs)
            .map(|job| async move {
                let (url, stem) = job?;
                match download_image(outline, &url, &stem).await {
                    Ok(p) => Some(p.to_string_lossy().into_owned()),
                    Err(err) => {
                        tracing::warn!("头像 {url} 下载失败: {err:#}");
                        None
                    }
                }
            })
            .buffered(6)
            .collect()
            .await;

        // 新增内容里的图片：下载到临时目录，再把 markdown 里的链接换成本地路径
        let markdowns: Vec<String> = updated
            .iter()
            .map(|e| {
                let Status::Updated { hunks, .. } = &e.status else {
                    unreachable!()
                };
                join_markdown(hunks)
            })
            .collect();
        let urls: Vec<String> = markdowns
            .iter()
            .flat_map(|md| find_images(md).into_iter().map(|i| i.url))
            .collect();
        let downloads: Vec<(String, Option<String>)> = stream::iter(urls.into_iter().enumerate())
            .map(|(i, url)| async move {
                match download_image(
                    outline,
                    &outline.absolute(&url),
                    &dir.join(format!("img{i}")),
                )
                .await
                {
                    Ok(p) => (url, Some(p.to_string_lossy().into_owned())),
                    Err(err) => {
                        tracing::warn!("图片 {url} 下载失败: {err:#}");
                        (url, None)
                    }
                }
            })
            .buffered(6)
            .collect()
            .await;
        let local: std::collections::HashMap<String, Option<String>> =
            downloads.into_iter().collect();

        let entries = updated
            .iter()
            .zip(avatars)
            .zip(markdowns)
            .map(|((e, avatar), markdown)| {
                let Status::Updated { added, removed, .. } = &e.status else {
                    unreachable!()
                };
                let markdown = localize_images(&markdown, &local);
                EntryData {
                    initial: e
                        .name
                        .chars()
                        .next()
                        .map(|c| c.to_uppercase().collect())
                        .unwrap_or_default(),
                    color: e
                        .user
                        .as_ref()
                        .and_then(|u| u.color.clone())
                        .unwrap_or_else(|| "#8B949E".into()),
                    avatar,
                    time: e
                        .updated_at
                        .map(|t| t.with_timezone(&tz()).format("%m/%d %H:%M").to_string())
                        .unwrap_or_default(),
                    added: added.len(),
                    removed: *removed,
                    markdown,
                    name: e.name.clone(),
                }
            })
            .collect();

        let (first, last) = (report.window.first_day(), report.window.last_day());
        let data = Data {
            bg: template_dir.join("bg.png").to_string_lossy().into_owned(),
            range: if first == last {
                first.format("%Y/%m/%d").to_string()
            } else {
                format!("{} – {}", first.format("%Y/%m/%d"), last.format("%m/%d"))
            },
            updated: updated.len(),
            total: report.entries.len(),
            entries,
            idle: report
                .entries
                .iter()
                .filter(|e| matches!(e.status, Status::Idle))
                .map(|e| e.name.clone())
                .collect(),
            footer: "Merak CTF Team · docs.buptmerak.cn".into(),
        };

        let data_path = dir.join("data.json");
        tokio::fs::write(&data_path, serde_json::to_vec(&data)?).await?;

        // 内容全部渲染，图可能很长；超过上传上限时逐级降低分辨率
        let mut png = Vec::new();
        for ppi in PPI_STEPS {
            png = self
                .compile(&template_dir, &data_path, &dir.join("out.png"), ppi)
                .await?;
            if png.len() <= MAX_PNG_BYTES {
                break;
            }
            tracing::warn!(
                "{ppi} ppi 渲染结果 {} 字节，超过上传上限，降低分辨率重试",
                png.len()
            );
        }
        Ok(png)
    }

    async fn compile(
        &self,
        template_dir: &Path,
        data_path: &Path,
        out: &Path,
        ppi: u32,
    ) -> Result<Vec<u8>> {
        let output = tokio::process::Command::new(&self.typst)
            .arg("compile")
            .args(["--root", "/", "--ignore-system-fonts", "--format", "png"])
            .arg("--ppi")
            .arg(ppi.to_string())
            .arg("--font-path")
            .arg(std::path::absolute(&self.font_dir)?)
            // cmarker 等 Typst 包随模板一起放在 template/packages，渲染时不联网
            .arg("--package-path")
            .arg(template_dir.join("packages"))
            .arg("--input")
            .arg(format!("data={}", data_path.display()))
            .arg(template_dir.join("report.typ"))
            .arg(out)
            .output()
            .await
            .with_context(|| format!("运行 {}", self.typst))?;
        if !output.status.success() {
            bail!(
                "typst 渲染失败: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Ok(tokio::fs::read(out).await?)
    }
}

fn rand_suffix() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

/// 下载图片，按文件头判断格式，typst 不支持的格式返回错误（调用方退回占位）
async fn download_image(outline: &OutlineClient, url: &str, stem: &Path) -> Result<PathBuf> {
    let bytes = outline.fetch(url).await?;
    let ext = match bytes.as_slice() {
        [0x89, b'P', b'N', b'G', ..] => "png",
        [0xFF, 0xD8, ..] => "jpg",
        [b'G', b'I', b'F', ..] => "gif",
        [
            b'R',
            b'I',
            b'F',
            b'F',
            _,
            _,
            _,
            _,
            b'W',
            b'E',
            b'B',
            b'P',
            ..,
        ] => "webp",
        b if b.starts_with(b"<svg") || b.starts_with(b"<?xml") => "svg",
        _ => bail!("不支持的图片格式"),
    };
    let path = stem.with_extension(ext);
    tokio::fs::write(&path, bytes).await?;
    Ok(path)
}

/// 把各段新增内容拼成一份 markdown。diff 可能只新增了代码块的一部分（比如只有开头的 ```），
/// 这时给该段补上收尾的 ```，避免后面的段落全被当成代码。
fn join_markdown(hunks: &[String]) -> String {
    hunks
        .iter()
        .map(|hunk| {
            let hunk = strip_indent(hunk);
            let fences = hunk
                .lines()
                .filter(|l| l.trim_start().starts_with("```"))
                .count();
            if fences % 2 == 1 {
                format!("{hunk}\n```")
            } else {
                hunk
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// 去掉会被当成缩进代码块的行首空格。markdown 里行首满 4 个空格就是代码块，会画成代码框，
/// 但 Outline 的代码块一律用 ``` 围起来，段落开头的空格只是有人手打的首行缩进。
/// 列表的缩进是嵌套层次，按这一段里最外层的列表项对齐后保留（diff 可能只新增了深层的列表项）；``` 里面的内容不动。
fn strip_indent(hunk: &str) -> String {
    let mut out = Vec::new();
    // 当前列表最外层列表项的缩进；不在列表里时为 None
    let mut list_base: Option<usize> = None;
    let mut in_fence = false;
    // 当前这一块去掉的缩进宽度，代码块里的行按开头那行去掉同样多
    let mut shift = 0;
    for line in hunk.lines() {
        let body = line.trim_start_matches([' ', '\t']);
        let indent = line.len() - body.len();
        if in_fence {
            out.push(&line[indent.min(shift)..]);
            in_fence = !body.starts_with("```");
            continue;
        }
        if body.is_empty() {
            out.push("");
            continue;
        }
        let item = is_list_item(body);
        shift = match list_base {
            // 同级、更深的列表项，或列表项里的续行
            Some(base) if indent > base || (item && indent == base) => base,
            _ if item => {
                list_base = Some(indent);
                indent
            }
            // 没缩进的普通段落结束列表；不在列表里的缩进全去掉
            _ => {
                list_base = None;
                indent
            }
        };
        in_fence = body.starts_with("```");
        out.push(&line[shift..]);
    }
    out.join("\n")
}

/// `- x`、`* x`、`+ x`、`1. x`、`1) x`
fn is_list_item(body: &str) -> bool {
    let rest = body.trim_start_matches(|c: char| c.is_ascii_digit());
    let after = if rest.len() < body.len() {
        rest.strip_prefix(['.', ')'])
    } else {
        body.strip_prefix(['-', '*', '+'])
    };
    after.is_some_and(|r| r.is_empty() || r.starts_with(' '))
}

pub(crate) struct ImageRef {
    /// `![...](...)` 在 markdown 里的字节范围
    pub range: std::ops::Range<usize>,
    pub alt: String,
    pub url: String,
}

/// 找出 markdown 里的图片 `![alt](url "title")`。Outline 会在 title 里写尺寸（" =500x659"），这里忽略 title
pub(crate) fn find_images(md: &str) -> Vec<ImageRef> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(pos) = md[from..].find("![") {
        let start = from + pos;
        let Some(close) = md[start + 2..].find("](").map(|p| start + 2 + p) else {
            break;
        };
        let Some(end) = md[close + 2..].find(')').map(|p| close + 2 + p) else {
            break;
        };
        let inner = md[close + 2..end].trim();
        let url = inner
            .split_whitespace()
            .next()
            .unwrap_or("")
            .trim_matches(['<', '>']);
        if !url.is_empty() && !md[start + 2..close].contains('\n') {
            out.push(ImageRef {
                range: start..end + 1,
                alt: md[start + 2..close].to_owned(),
                url: url.to_owned(),
            });
        }
        from = end + 1;
    }
    out
}

/// 把图片链接换成已下载的本地绝对路径；下载失败的换成文字占位
fn localize_images(md: &str, local: &std::collections::HashMap<String, Option<String>>) -> String {
    let mut out = String::with_capacity(md.len());
    let mut last = 0;
    for img in find_images(md) {
        out += &md[last..img.range.start];
        match local.get(&img.url) {
            Some(Some(path)) => out += &format!("![{}](<{path}>)", img.alt),
            _ => out += "\\[图片\\]",
        }
        last = img.range.end;
    }
    out += &md[last..];
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_hunks_and_closes_fences() {
        let hunks = vec![
            "## 3.28\n\n学了 tcache".to_owned(),
            "```c\nint a;".to_owned(),
            "下一段".to_owned(),
        ];
        assert_eq!(
            join_markdown(&hunks),
            "## 3.28\n\n学了 tcache\n\n```c\nint a;\n```\n\n下一段"
        );
    }

    #[test]
    fn strips_indent_that_would_become_code() {
        // 首行缩进的段落
        assert_eq!(
            strip_indent("### 2026/10/27\n\n    写了个blog\n\n\\"),
            "### 2026/10/27\n\n写了个blog\n\n\\"
        );
        // 只新增了深层列表项：对齐到最外层，保留嵌套
        assert_eq!(
            strip_indent("    - a\n      - b\n\n        续行\n    - c"),
            "- a\n  - b\n\n    续行\n- c"
        );
        // 普通段落结束列表，之后的缩进段落照样去掉
        assert_eq!(strip_indent("- a\n正文\n    缩进"), "- a\n正文\n缩进");
        // 代码块里的内容不动，只跟着开头那行一起左移
        assert_eq!(
            strip_indent("  ```c\n    int a;\n  ```\n  后文"),
            "```c\n  int a;\n```\n后文"
        );
        assert!(is_list_item("1. x") && is_list_item("-") && is_list_item("* x"));
        assert!(
            !is_list_item("---")
                && !is_list_item("**粗体**")
                && !is_list_item("10.26")
                && !is_list_item("2026.10.27 x")
        );
    }

    #[test]
    fn rewrites_images() {
        let md = r#"图一 ![](/api/a?id=1 " =500x659") 图二 ![截图](/api/a?id=2) ![x](/api/a?id=3 "right-50 =1x1")"#;
        let imgs = find_images(md);
        assert_eq!(
            imgs.iter().map(|i| i.url.as_str()).collect::<Vec<_>>(),
            ["/api/a?id=1", "/api/a?id=2", "/api/a?id=3"]
        );
        let local = [
            ("/api/a?id=1".to_owned(), Some("/tmp/img0.png".to_owned())),
            ("/api/a?id=2".to_owned(), None),
        ]
        .into_iter()
        .collect();
        assert_eq!(
            localize_images(md, &local),
            r"图一 ![](</tmp/img0.png>) 图二 \[图片\] \[图片\]"
        );
    }
}
