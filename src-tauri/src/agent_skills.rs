//! 技能文件：内置技能随应用分发（resources/skills/<id>/SKILL.md），作者覆盖版放提示词目录 skills/<id>/SKILL.md。
//! 文件是内容来源；数据库保存启停、确认状态与内容索引。旧版数据库覆盖在首次同步时迁入作者目录。
use crate::agent_store::{get, now, put, NS};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::SqliteConnection;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// 内置技能替换旧的数据库引导技能时，旧技能停用但保留记录，作者仍可查看。
const RETIRED_BY_FILES: &[(&str, &str)] = &[("chapter-editor", "chapter-writing")];

#[derive(Debug, Clone)]
pub struct SkillFile {
    pub id: String,
    pub version: String,
    pub body: String,
    pub path: PathBuf,
    /// builtin | user
    pub source: String,
}

fn hash(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
        .chars()
        .take(16)
        .collect()
}

/// 解析 frontmatter（`---` 之间的 `key: value` 行）与正文；没有 frontmatter 时整篇是正文。
pub fn parse_skill_document(text: &str) -> (BTreeMap<String, String>, String) {
    let text = text.trim_start_matches('\u{feff}');
    let mut lines = text.lines();
    let Some(first) = lines.next() else {
        return (BTreeMap::new(), String::new());
    };
    if first.trim() != "---" {
        return (BTreeMap::new(), text.trim().to_owned());
    }
    let mut meta = BTreeMap::new();
    let mut body = Vec::new();
    let mut in_meta = true;
    for line in lines {
        if in_meta {
            if line.trim() == "---" {
                in_meta = false;
                continue;
            }
            if let Some((key, value)) = line.split_once(':') {
                let raw = value.trim();
                let value = serde_json::from_str::<String>(raw)
                    .unwrap_or_else(|_| raw.trim_matches('"').trim_matches('\'').to_owned());
                meta.insert(key.trim().to_owned(), value);
            }
        } else {
            body.push(line);
        }
    }
    (meta, body.join("\n").trim().to_owned())
}

/// 扫描目录：每个子目录一个技能，入口 SKILL.md；目录名即技能 id。
pub fn discover(dir: &Path, source: &str) -> Vec<SkillFile> {
    let mut skills = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return skills;
    };
    for entry in entries.flatten() {
        let path = entry.path().join("SKILL.md");
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let id = entry.file_name().to_string_lossy().to_string();
        if id.trim().is_empty() || id.starts_with('.') {
            continue;
        }
        let (meta, body) = parse_skill_document(&text);
        if body.is_empty() {
            continue;
        }
        skills.push(SkillFile {
            id: id.clone(),
            version: meta.get("version").cloned().unwrap_or_else(|| "1".into()),
            body,
            path,
            source: source.to_owned(),
        });
    }
    skills.sort_by(|a, b| a.id.cmp(&b.id));
    skills
}

/// 内置目录在前、作者目录在后；同 id 以作者目录为准。
pub fn collect(builtin_dirs: &[PathBuf], user_dir: Option<&Path>) -> Vec<SkillFile> {
    let mut by_id: BTreeMap<String, SkillFile> = BTreeMap::new();
    for dir in builtin_dirs {
        for skill in discover(dir, "builtin") {
            by_id.entry(skill.id.clone()).or_insert(skill);
        }
    }
    if let Some(dir) = user_dir {
        for skill in discover(dir, "user") {
            by_id.insert(skill.id.clone(), skill);
        }
    }
    by_id.into_values().collect()
}

fn record_from(skill: &SkillFile, existing: Option<&Value>) -> Value {
    let mut record = existing
        .cloned()
        .unwrap_or_else(|| json!({"id":skill.id,"enabled":true,"python":false,"createdAt":now()}));
    record["source"] = json!(if skill.source == "user" {
        if existing.is_some_and(|record| record["source"] == "third-party") {
            "third-party"
        } else {
            "custom"
        }
    } else {
        "builtin"
    });
    record["builtinVersion"] = json!(skill.version);
    record["filePath"] = json!(skill.path.to_string_lossy());
    record["fileSource"] = json!(skill.source);
    record["updatedAt"] = json!(now());
    record
}

/// 把技能文件同步进 `skill:` 记录。返回 {created, updated, kept, retired}。
pub async fn sync(
    db: &mut SqliteConnection,
    builtin_dirs: &[PathBuf],
    user_dir: Option<&Path>,
) -> Result<Value, String> {
    if let Some(dir) = user_dir {
        crate::agent_skill_files::migrate(db, dir).await?;
    }
    let files = collect(builtin_dirs, user_dir);
    let (mut created, mut updated, mut kept) = (0, 0, 0);
    for skill in &files {
        let key = format!("skill:{}", skill.id);
        let existing = get(db, NS, &key).await?;
        let mut record = record_from(skill, existing.as_ref());
        crate::agent_skill_files::hydrate(&mut record, skill.path.parent().ok_or("技能路径无效")?)?;
        match &existing {
            None => created += 1,
            Some(old) if old["revision"] != record["revision"] => updated += 1,
            Some(_) => kept += 1,
        }
        record["builtinHash"] = json!(hash(&skill.body));
        put(db, NS, &key, &record).await?;
    }
    let mut retired = 0;
    for (old_id, new_id) in RETIRED_BY_FILES {
        if !files.iter().any(|s| s.id == *new_id) {
            continue;
        }
        let key = format!("skill:{old_id}");
        if let Some(mut old) = get(db, NS, &key).await? {
            if old["retiredAt"].is_null() {
                old["enabled"] = json!(false);
                old["retiredAt"] = json!(now());
                old["retiredBy"] = json!(new_id);
                put(db, NS, &key, &old).await?;
                retired += 1;
            } else if old["enabled"] == true {
                // 退役后又被打开过（旧版界面能开）：接替者在，它就不该再启用
                old["enabled"] = json!(false);
                put(db, NS, &key, &old).await?;
            }
        }
    }
    Ok(
        json!({"created":created,"updated":updated,"kept":kept,"retired":retired,"files":files.len()}),
    )
}

/// 恢复某个技能到文件内容（作者“恢复默认”）。
pub async fn reset(
    db: &mut SqliteConnection,
    builtin_dirs: &[PathBuf],
    user_dir: Option<&Path>,
    skill_id: &str,
) -> Result<Value, String> {
    let skill = collect(builtin_dirs, user_dir)
        .into_iter()
        .find(|s| s.id == skill_id)
        .ok_or("该技能没有对应的文件，无法恢复默认")?;
    let key = format!("skill:{skill_id}");
    let existing = get(db, NS, &key).await?;
    let mut record = record_from(&skill, existing.as_ref());
    crate::agent_skill_files::hydrate(&mut record, skill.path.parent().ok_or("技能路径无效")?)?;
    record["builtinHash"] = json!(hash(&skill.body));
    record["builtinLatest"] = Value::Null;
    put(db, NS, &key, &record).await?;
    Ok(record)
}
