//! 成员名单：一个 JSON 数组文件，每项对应一个人的进度文档。
//!
//! ```json
//! [{"doc": "https://docs.example.com/doc/foo-AbCdEf1234", "alias": "可选", "ignore": false}]
//! ```
//!
//! 设置了 MEMBERS_SINCE 时，机器人在每次 push 前把 pushpush 目录里该日期之后新建的
//! 顶层文档自动追加进来；已经在文件里的文档（不管是否 ignore）不会再动。
//! 不想统计某个人时把那项的 ignore 改成 true，不要删掉，删掉会被重新自动加回来。

use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::outline::{Document, OutlineClient};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Member {
    /// 文档链接、uuid 或 urlId
    pub doc: String,
    /// 覆盖显示名，默认用文档创建者的名字
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    /// 为 true 时不参与统计
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ignore: bool,
    /// 自动加入时记下的文档标题，只是方便手动编辑时辨认
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// 保留手动加的其他字段，自动写回时不丢
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

pub fn load(path: &Path) -> Result<Vec<Member>> {
    match std::fs::read_to_string(path) {
        Ok(s) => serde_json::from_str(&s).with_context(|| format!("解析 {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e.into()),
    }
}

pub fn save(path: &Path, members: &[Member]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(members)?)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// 文档链接形如 https://host/doc/slug-urlId[/edit]，取 /doc/ 后面那段；uuid、urlId 原样返回
pub fn doc_id(doc: &str) -> &str {
    match doc.split_once("/doc/") {
        Some((_, rest)) => rest.split(['/', '?', '#']).next().unwrap_or(rest),
        None => doc.trim(),
    }
}

/// 改名单里一项的 alias / ignore。alias 为 Some("") 表示清掉；None 表示不改。返回 false 表示没有这个 doc
pub fn update(path: &Path, doc: &str, alias: Option<String>, ignore: Option<bool>) -> Result<bool> {
    let mut members = load(path)?;
    let Some(m) = members.iter_mut().find(|m| m.doc == doc) else {
        return Ok(false);
    };
    if let Some(a) = alias {
        m.alias = Some(a.trim().to_owned()).filter(|a| !a.is_empty());
    }
    if let Some(i) = ignore {
        m.ignore = i;
    }
    save(path, &members)?;
    Ok(true)
}

/// 文档在名单里的几种写法（uuid、urlId、链接里的 slug），任一命中就算已在名单里
fn is_listed(members: &[Member], keys: [&str; 3]) -> bool {
    members.iter().any(|m| keys.contains(&doc_id(&m.doc)))
}

/// 手动加一篇文档。返回 false 表示已经在名单里
pub fn add(path: &Path, outline: &OutlineClient, doc: &Document) -> Result<bool> {
    let mut members = load(path)?;
    if is_listed(&members, [&doc.id, &doc.url_id, doc_id(&doc.url)]) {
        return Ok(false);
    }
    members.push(Member {
        doc: outline.absolute(&doc.url),
        title: Some(doc.title.clone()),
        ..Default::default()
    });
    save(path, &members)?;
    Ok(true)
}

/// 从名单里删掉一项（按条目里写的 doc 原样匹配）。返回 false 表示没有这一项
pub fn remove(path: &Path, doc: &str) -> Result<bool> {
    let mut members = load(path)?;
    let before = members.len();
    members.retain(|m| m.doc != doc);
    if members.len() == before {
        return Ok(false);
    }
    save(path, &members)?;
    Ok(true)
}

/// 自动加成员会不会把这篇文档加回来：同一个 collection、顶层、since 之后新建
pub fn would_auto_add(doc: &Document, collection_id: &str, since: DateTime<Utc>) -> bool {
    doc.collection_id.as_deref() == Some(collection_id)
        && doc.parent_document_id.is_none()
        && doc.created_at >= since
}

/// Outline 账号名来自 SSO，真名是「名 姓」格式（`三 张`、`A B`），含空白的一律当真名
pub fn looks_like_real_name(name: &str) -> bool {
    name.trim().contains(char::is_whitespace)
}

/// 显示名，按优先级：
/// 1. alias（手动覆盖，也用来盖掉规则漏判的真名）
/// 2. Outline 账号名，但只在它是昵称时用。账号名来自 SSO，真名是「名 姓」格式（`三 张`、`A B`），
///    所以含空白的一律当真名不用
/// 3. 文档标题去掉「的进度」（`nova 的进度` -> `nova`）
///
/// 都没有时返回 None，这篇文档不进战报，等人工补 alias。
pub fn display_name(alias: Option<&str>, account: Option<&str>, title: &str) -> Option<String> {
    let usable = |s: &str| !s.is_empty();
    if let Some(a) = alias.map(str::trim).filter(|a| usable(a)) {
        return Some(a.to_owned());
    }
    if let Some(n) = account
        .map(str::trim)
        .filter(|n| usable(n) && !looks_like_real_name(n))
    {
        return Some(n.to_owned());
    }
    let id = title.trim().strip_suffix("的进度")?.trim();
    usable(id).then(|| id.to_owned())
}

/// 把 collection 里 since 之后新建、还不在名单里的顶层文档追加到名单，返回新增数量
pub async fn sync(
    outline: &OutlineClient,
    path: &Path,
    collection_id: &str,
    since: DateTime<Utc>,
) -> Result<usize> {
    let mut members = load(path)?;
    let mut added = 0;
    for d in outline
        .documents_created_since(collection_id, since)
        .await?
    {
        // 挂在别人文档下面的子文章不算成员
        if d.parent_document_id.is_some() {
            continue;
        }
        if is_listed(&members, [&d.id, &d.url_id, doc_id(&d.url)]) {
            continue;
        }
        tracing::info!("自动加入成员：{}", d.title);
        members.push(Member {
            doc: outline.absolute(&d.url),
            title: Some(d.title),
            ..Default::default()
        });
        added += 1;
    }
    if added > 0 {
        save(path, &members)?;
    }
    Ok(added)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_doc_id() {
        assert_eq!(
            doc_id("https://docs.example.com/doc/foo-AbCdEf1234"),
            "foo-AbCdEf1234"
        );
        assert_eq!(
            doc_id("https://docs.example.com/doc/foo-AbCdEf1234/edit?x=1"),
            "foo-AbCdEf1234"
        );
        assert_eq!(
            doc_id(" 00000000-0000-4000-8000-000000000000 "),
            "00000000-0000-4000-8000-000000000000"
        );
    }

    #[test]
    fn display_name_priority() {
        // alias 最优先
        assert_eq!(
            display_name(Some("小Z"), Some("alice42"), "zs的进度").as_deref(),
            Some("小Z")
        );
        // 账号名是昵称就用账号名，即使标题不是「xxx的进度」
        assert_eq!(
            display_name(Some("  "), Some("alice42"), "alice的进度").as_deref(),
            Some("alice42")
        );
        assert_eq!(
            display_name(None, Some("Mr.X"), "?").as_deref(),
            Some("Mr.X")
        );
        // 账号名是「名 姓」真名格式，退回标题
        assert_eq!(
            display_name(None, Some("三 张"), "zs的进度").as_deref(),
            Some("zs")
        );
        assert_eq!(
            display_name(None, Some("A B"), "TEST的进度").as_deref(),
            Some("TEST")
        );
        assert_eq!(
            display_name(None, Some("Jane Doe"), "jane 的进度 ").as_deref(),
            Some("jane")
        );
        // 都没有就跳过
        assert_eq!(display_name(None, Some("四 李"), "说明"), None);
        assert_eq!(display_name(None, None, "的进度"), None);
    }

    #[test]
    fn keeps_unknown_fields_and_skips_defaults() {
        let m: Vec<Member> = serde_json::from_str(
            r#"[{"doc": "a", "note": "手动备注"}, {"doc": "b", "ignore": true}]"#,
        )
        .unwrap();
        let out = serde_json::to_string(&m).unwrap();
        assert_eq!(
            out,
            r#"[{"doc":"a","note":"手动备注"},{"doc":"b","ignore":true}]"#
        );
    }
}
