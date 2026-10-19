//! 技能包的唯一文件入口：编辑、导入、迁移共用校验与整包替换。
//! 文件是内容来源，数据库里的说明只是供 Agent 查询的可重建索引。
use crate::agent_store::{err, get, id, now, put, NS};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::SqliteConnection;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

pub(crate) const MAX_FILE: usize = 256 * 1024;
const MAX_PACKAGE: usize = 8 * 1024 * 1024;
type Package = BTreeMap<String, Vec<u8>>;

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceFile {
    pub path: String,
    pub content: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Draft {
    #[serde(default)]
    pub id: String,
    pub name: String,
    pub description: String,
    pub instructions: String,
    pub kind: Option<String>,
    pub replaces: Option<String>,
    pub revision: Option<String>,
    #[serde(default)]
    pub files: Vec<SourceFile>,
}

pub fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 100
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}

pub fn editable(path: &str) -> bool {
    if path == "requirements.txt" {
        return true;
    }
    let reference = path.starts_with("references/");
    let Some(name) = path.strip_prefix("scripts/").or_else(|| path.strip_prefix("references/")) else {
        return false;
    };
    !name.is_empty()
        && name.split('/').all(|part| {
            !part.is_empty()
                && !part.starts_with('.')
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "_-.".contains(c) || (reference && c.is_alphanumeric()))
        })
        && (!reference || !path.ends_with(".py"))
        && [
            ".py", ".txt", ".json", ".md", ".csv", ".toml", ".yaml", ".yml",
        ]
        .iter()
        .any(|ext| name.ends_with(ext))
}

fn read_tree(dir: &Path, base: &Path, out: &mut Package, size: &mut usize) -> Result<(), String> {
    if std::fs::symlink_metadata(dir)
        .map_err(err)?
        .file_type()
        .is_symlink()
    {
        return Err("技能文件夹不能是符号链接".into());
    }
    for entry in std::fs::read_dir(dir).map_err(err)? {
        let entry = entry.map_err(err)?;
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || name == "__pycache__" {
            continue;
        }
        if name.contains(['\\', ':']) {
            return Err(format!("不支持的文件名：{name}"));
        }
        let kind = entry.file_type().map_err(err)?;
        if kind.is_symlink() {
            return Err(format!("技能不能包含符号链接：{name}"));
        }
        if kind.is_dir() {
            read_tree(&entry.path(), base, out, size)?;
        } else if kind.is_file() {
            let len = entry.metadata().map_err(err)?.len() as usize;
            if len > MAX_PACKAGE || *size + len > MAX_PACKAGE || out.len() >= 256 {
                return Err("技能包最多 256 个文件、合计 8 MB".into());
            }
            let path = entry
                .path()
                .strip_prefix(base)
                .map_err(err)?
                .to_string_lossy()
                .replace('\\', "/");
            let bytes = std::fs::read(entry.path()).map_err(err)?;
            *size += bytes.len();
            if editable(&path) && (bytes.len() > MAX_FILE || std::str::from_utf8(&bytes).is_err()) {
                return Err(format!("{path} 必须是小于 256 KB 的 UTF-8 文本"));
            }
            if out
                .keys()
                .any(|existing| existing.eq_ignore_ascii_case(&path))
            {
                return Err(format!("文件名大小写冲突：{path}"));
            }
            out.insert(path, bytes);
        } else {
            return Err(format!("技能不支持特殊文件：{name}"));
        }
    }
    Ok(())
}

fn read_package(dir: &Path) -> Result<Package, String> {
    let mut package = Package::new();
    read_tree(dir, dir, &mut package, &mut 0)?;
    if !package.contains_key("SKILL.md") {
        return Err("技能说明文件 SKILL.md 不存在，请恢复文件或重新导入".into());
    }
    Ok(package)
}

fn fingerprint(package: &Package, include_instructions: bool) -> String {
    let mut digest = Sha256::new();
    for (path, bytes) in package {
        if !include_instructions && path == "SKILL.md" {
            continue;
        }
        digest.update(path.as_bytes());
        digest.update([0]);
        digest.update(bytes);
        digest.update([0]);
    }
    format!("{:x}", digest.finalize())
}

pub fn script_hash(dir: &Path) -> Result<String, String> {
    Ok(fingerprint(&read_package(dir)?, false))
}

/// 执行的必须是确认过的文件快照，保存或外部修改不能在校验之后换掉代码。
pub fn snapshot(dir: &Path, target: &Path, expected_hash: &str) -> Result<(), String> {
    let package = read_package(dir)?;
    if fingerprint(&package, false) != expected_hash {
        return Err("脚本在准备运行时发生变化，请重新确认".into());
    }
    for (path, bytes) in package {
        let output = target.join(path);
        std::fs::create_dir_all(output.parent().unwrap()).map_err(err)?;
        std::fs::write(output, bytes).map_err(err)?;
    }
    Ok(())
}

pub fn inspect(dir: &Path) -> Result<Value, String> {
    let package = read_package(dir)?;
    let text = std::str::from_utf8(package.get("SKILL.md").ok_or("文件夹里没有 SKILL.md")?)
        .map_err(err)?;
    let (meta, body) = crate::agent_skills::parse_skill_document(text);
    if body.trim().is_empty() {
        return Err("SKILL.md 没有正文".into());
    }
    let skill_id = dir.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let files: Vec<Value> = package.iter().filter(|(path, _)| path.as_str() != "SKILL.md").map(|(path, bytes)| {
        json!({"path":path,"content":if editable(path) { std::str::from_utf8(bytes).ok() } else { None },"editable":editable(path),"size":bytes.len()})
    }).collect();
    let requirements = package
        .get("requirements.txt")
        .or_else(|| package.get("scripts/requirements.txt"))
        .map(|v| String::from_utf8_lossy(v).to_string())
        .unwrap_or_default();
    Ok(
        json!({"id":skill_id,"name":meta.get("name").map(String::as_str).unwrap_or(skill_id),"description":meta.get("description").cloned().unwrap_or_default(),
        "kind":meta.get("kind"),"replaces":meta.get("replaces"),"instructions":body,"files":files,"requirements":requirements,
        "scriptHash":fingerprint(&package,false),"revision":fingerprint(&package,true),"directory":dir.to_string_lossy()}),
    )
}

fn document(draft: &Draft) -> Result<Vec<u8>, String> {
    for (label, value, max) in [
        ("技能名称", &draft.name, 80),
        ("用途说明", &draft.description, 1000),
        ("技能说明", &draft.instructions, 50000),
    ] {
        if value.trim().is_empty() || value.chars().count() > max {
            return Err(format!("{label}不能为空且不能超过 {max} 字"));
        }
    }
    if draft.name.contains(['\r', '\n']) || draft.description.contains(['\r', '\n']) {
        return Err("名称与用途说明请使用单行文本".into());
    }
    if draft
        .kind
        .as_deref()
        .is_some_and(|k| !["method", "style", "check", "tool"].contains(&k))
    {
        return Err("技能类型无效".into());
    }
    if draft
        .replaces
        .as_deref()
        .is_some_and(|s| !s.is_empty() && !valid_id(s))
    {
        return Err("替换方法编号无效".into());
    }
    let mut front = vec![
        format!(
            "name: {}",
            serde_json::to_string(draft.name.trim()).map_err(err)?
        ),
        format!(
            "description: {}",
            serde_json::to_string(draft.description.trim()).map_err(err)?
        ),
    ];
    if let Some(kind) = &draft.kind {
        front.push(format!("kind: {kind}"));
    }
    if let Some(target) = &draft.replaces {
        if !target.is_empty() {
            front.push(format!("replaces: {target}"));
        }
    }
    Ok(format!(
        "---\n{}\nversion: 1\n---\n\n{}\n",
        front.join("\n"),
        draft.instructions.trim()
    )
    .into_bytes())
}

pub fn validate(draft: &Draft) -> Result<(), String> {
    validate_candidate(draft)?;
    let document_size = document(draft)?.len();
    if document_size
        + draft
            .files
            .iter()
            .map(|file| file.content.len())
            .sum::<usize>()
        > MAX_PACKAGE
    {
        return Err("技能包超过 8 MB".into());
    }
    if draft.files.len() > 64 {
        return Err("最多添加 64 个参考资料、脚本与依赖文件".into());
    }
    let mut names = std::collections::BTreeSet::new();
    for file in &draft.files {
        if !editable(&file.path) {
            return Err(format!("不支持的文件路径：{}", file.path));
        }
        if !names.insert(file.path.to_lowercase()) {
            return Err(format!("文件名重复：{}", file.path));
        }
        if file.path.ends_with(".py") && file.content.trim().is_empty() {
            return Err(format!("{} 尚未填写脚本内容", file.path));
        }
        if file.content.len() > MAX_FILE {
            return Err(format!("{} 超过 256 KB", file.path));
        }
    }
    if names.contains("requirements.txt") && names.contains("scripts/requirements.txt") {
        return Err("依赖清单只保留一份 requirements.txt".into());
    }
    Ok(())
}

pub fn validate_candidate(draft: &Draft) -> Result<(), String> {
    if draft.name.chars().count() > 80
        || draft.description.chars().count() > 1000
        || draft.instructions.chars().count() > 50000
        || draft.files.len() > 64
        || draft
            .files
            .iter()
            .any(|file| file.path.len() > 255 || file.content.len() > MAX_FILE)
        || draft
            .files
            .iter()
            .map(|file| file.content.len())
            .sum::<usize>()
            + draft.instructions.len()
            > MAX_PACKAGE
    {
        return Err("技能候选超过名称、说明或文件大小限制".into());
    }
    Ok(())
}

/// 中断恢复：替换前的完整包始终保留，重启时只恢复缺失的目标。
pub fn recover(user_dir: &Path) -> Result<(), String> {
    if !user_dir.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(user_dir).map_err(err)? {
        let entry = entry.map_err(err)?;
        let name = entry.file_name().to_string_lossy().to_string();
        if let Some(skill_id) = name.strip_prefix(".previous-").filter(|s| valid_id(s)) {
            let target = user_dir.join(skill_id);
            if target.exists() {
                std::fs::remove_dir_all(entry.path()).map_err(err)?;
            } else {
                std::fs::rename(entry.path(), target).map_err(err)?;
            }
        }
    }
    Ok(())
}

fn write_package(
    user_dir: &Path,
    skill_id: &str,
    package: &Package,
    expected: Option<&str>,
) -> Result<PathBuf, String> {
    if !valid_id(skill_id) {
        return Err("技能编号只能用英文字母、数字、- 和 _".into());
    }
    if package.values().map(Vec::len).sum::<usize>() > MAX_PACKAGE {
        return Err("技能包超过 8 MB".into());
    }
    std::fs::create_dir_all(user_dir).map_err(err)?;
    let target = user_dir.join(skill_id);
    let stage = user_dir.join(format!(".pending-{}", id()));
    let backup = user_dir.join(format!(".previous-{skill_id}"));
    let result = (|| {
        std::fs::create_dir(&stage).map_err(err)?;
        for (path, bytes) in package {
            let output = stage.join(path);
            std::fs::create_dir_all(output.parent().unwrap()).map_err(err)?;
            std::fs::write(output, bytes).map_err(err)?;
        }
        if target.exists() {
            let current = fingerprint(&read_package(&target)?, true);
            if expected != Some(current.as_str()) {
                return Err("目标技能已存在或保存期间被修改，请重新读取".into());
            }
            if backup.exists() {
                return Err("技能还有未恢复的保存记录，请重新打开技能页".into());
            }
            std::fs::rename(&target, &backup).map_err(err)?;
        }
        if let Err(error) = std::fs::rename(&stage, &target) {
            if backup.exists() {
                let _ = std::fs::rename(&backup, &target);
            }
            return Err(err(error));
        }
        if backup.exists() {
            let _ = std::fs::remove_dir_all(&backup);
        }
        Ok(target.clone())
    })();
    if stage.exists() {
        let _ = std::fs::remove_dir_all(stage);
    }
    result
}

pub fn read_draft(record: &Value) -> Result<Value, String> {
    if record["filePath"].as_str().is_none() {
        let mut out = record.clone();
        out["files"] = json!([]);
        out["readOnly"] = json!(true);
        out["directory"] = json!("");
        out["revision"] = json!(legacy_revision(record));
        return Ok(out);
    }
    let dir = crate::agent_skill_runner::skill_dir(record)?;
    let mut out = inspect(&dir)?;
    out["id"] = record["id"].clone();
    out["readOnly"] = json!(record["fileSource"] == "builtin");
    Ok(out)
}

/// 保存时重新读取原记录，不接受客户端传入的路径、授权和来源。
pub async fn save(
    db: &mut SqliteConnection,
    user_dir: &Path,
    draft: &Draft,
) -> Result<Value, String> {
    save_inner(db, user_dir, draft, None).await
}

/// 候选采用使用宿主生成的稳定目标编号，重试不能重复创建技能。
pub async fn save_candidate(
    db: &mut SqliteConnection,
    user_dir: &Path,
    draft: &Draft,
    target_id: &str,
) -> Result<Value, String> {
    save_inner(db, user_dir, draft, Some(target_id)).await
}

async fn save_inner(
    db: &mut SqliteConnection,
    user_dir: &Path,
    draft: &Draft,
    candidate_id: Option<&str>,
) -> Result<Value, String> {
    validate(draft)?;
    let old = if draft.id.is_empty() {
        None
    } else {
        Some(
            get(db, NS, &format!("skill:{}", draft.id))
                .await?
                .ok_or("技能不存在")?,
        )
    };
    if old.as_ref().is_some_and(|v| v["deletedAt"].is_number()) {
        return Err("技能已删除".into());
    }
    let mut package = if let Some(record) = &old {
        if record["filePath"].as_str().is_none() {
            if draft.revision.as_deref() != Some(legacy_revision(record).as_str()) {
                return Err("技能记录已变化，请重新打开".into());
            }
            Package::new()
        } else {
            let dir = crate::agent_skill_runner::skill_dir(record)?;
            let package = read_package(&dir)?;
            if draft.revision.as_deref() != Some(fingerprint(&package, true).as_str()) {
                return Err("技能文件已被修改，请重新打开后再保存；本次编辑仍然保留".into());
            }
            package
        }
    } else {
        Package::new()
    };
    let is_copy = old
        .as_ref()
        .is_some_and(|v| v["fileSource"] == "builtin" || v["filePath"].as_str().is_none());
    let skill_id = if old.is_none() || is_copy {
        candidate_id.map(str::to_owned).unwrap_or_else(id)
    } else {
        draft.id.clone()
    };
    package.retain(|path, _| !editable(path));
    for file in &draft.files {
        package.insert(file.path.clone(), file.content.as_bytes().to_vec());
    }
    package.insert("SKILL.md".into(), document(draft)?);
    let target = user_dir.join(&skill_id);
    // 文件已提交但数据库事务未提交时，重试只复用完全相同的候选包，不覆盖其他内容。
    let recover_candidate = candidate_id.is_some() && (old.is_none() || is_copy) && target.exists();
    let dir = if recover_candidate {
        if fingerprint(&read_package(&target)?, true) != fingerprint(&package, true) {
            return Err("候选目标文件已变化，请重新核对".into());
        }
        target
    } else {
        write_package(
            user_dir,
            &skill_id,
            &package,
            if is_copy {
                None
            } else {
                draft.revision.as_deref()
            },
        )?
    };
    let recovered_record = if recover_candidate {
        get(db, NS, &format!("skill:{skill_id}")).await?
    } else {
        None
    };
    if recovered_record
        .as_ref()
        .is_some_and(|record| record["deletedAt"].is_number())
    {
        return Err("这份候选对应的技能已被删除，请另建候选".into());
    }
    let mut record = if let Some(record) = recovered_record {
        record
    } else if is_copy {
        json!({"enabled":true,"copiedFrom":draft.id})
    } else {
        old.unwrap_or_else(|| json!({"enabled":true}))
    };
    record["id"] = json!(skill_id);
    record["source"] = json!("custom");
    record["fileSource"] = json!("user");
    record["filePath"] = json!(dir.join("SKILL.md").to_string_lossy());
    hydrate(&mut record, &dir)?;
    put(db, NS, &format!("skill:{skill_id}"), &record).await?;
    Ok(record)
}

pub fn hydrate(record: &mut Value, dir: &Path) -> Result<(), String> {
    let preview = inspect(dir)?;
    for key in [
        "name",
        "description",
        "instructions",
        "kind",
        "replaces",
        "revision",
    ] {
        record[key] = preview[key].clone();
    }
    record["python"] = json!(!crate::agent_skill_runner::scripts(dir).is_empty());
    record["scripts"] = json!(crate::agent_skill_runner::scripts(dir));
    record["storageVersion"] = json!(2);
    record["updatedAt"] = json!(now());
    record["icon"] = json!(match record["kind"].as_str() {
        Some("method") => "fa-solid fa-pen-nib",
        Some("style") => "fa-solid fa-feather-pointed",
        Some("check") => "fa-solid fa-magnifying-glass",
        _ => "fa-solid fa-toolbox",
    });
    record["builtinLatest"] = Value::Null;
    if record["approvedHash"] != preview["scriptHash"] {
        record["approvedHash"] = Value::Null;
        record["approvedAt"] = Value::Null;
    }
    Ok(())
}

/// 旧版仅存在数据库的技能、旧版对文件说明的覆盖，原身份迁入作者目录。
pub async fn migrate(db: &mut SqliteConnection, user_dir: &Path) -> Result<(), String> {
    recover(user_dir)?;
    for mut record in crate::agent_store::list(db, NS, "skill:").await? {
        if record["storageVersion"] == 2
            || record["deletedAt"].is_number()
            || record["retiredAt"].is_number()
        {
            continue;
        }
        let file = record["filePath"].as_str().map(PathBuf::from);
        let file_body = file
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|t| crate::agent_skills::parse_skill_document(&t).1);
        let recorded_hash = record["builtinHash"].as_str();
        let old_hash = format!(
            "{:x}",
            Sha256::digest(record["instructions"].as_str().unwrap_or("").as_bytes())
        );
        let author_changed = recorded_hash.is_none_or(|value| value != &old_hash[..16]);
        let changed = file_body.as_deref() != record["instructions"].as_str()
            && (file.is_none() || author_changed);
        if !changed {
            continue;
        }
        if record["source"] == "builtin" && file.is_none() {
            continue;
        }
        let draft = Draft {
            id: record["id"].as_str().unwrap_or("").into(),
            name: record["name"].as_str().unwrap_or("").into(),
            description: record["description"]
                .as_str()
                .filter(|s| !s.is_empty())
                .unwrap_or("自定义创作技能")
                .into(),
            instructions: record["instructions"].as_str().unwrap_or("").into(),
            kind: record["kind"].as_str().map(str::to_owned),
            replaces: record["replaces"].as_str().map(str::to_owned),
            revision: None,
            files: vec![],
        };
        let mut package = match file.as_ref().and_then(|p| p.parent()) {
            Some(dir) if dir.exists() => read_package(dir)?,
            _ => Package::new(),
        };
        package.insert("SKILL.md".into(), document(&draft)?);
        let target = user_dir.join(&draft.id);
        let expected = if target.exists() {
            Some(fingerprint(&read_package(&target)?, true))
        } else {
            None
        };
        let dir = write_package(user_dir, &draft.id, &package, expected.as_deref())?;
        if record["source"] != "third-party" {
            record["source"] = json!("custom");
        }
        record["fileSource"] = json!("user");
        record["filePath"] = json!(dir.join("SKILL.md").to_string_lossy());
        hydrate(&mut record, &dir)?;
        put(db, NS, &format!("skill:{}", draft.id), &record).await?;
    }
    Ok(())
}

pub async fn install(
    db: &mut SqliteConnection,
    user_dir: &Path,
    from: &Path,
    expected: &str,
) -> Result<Value, String> {
    let package = read_package(from)?;
    if fingerprint(&package, true) != expected {
        return Err("导入文件在查看后发生变化，请重新导入".into());
    }
    let skill_id = from
        .file_name()
        .and_then(|p| p.to_str())
        .filter(|s| valid_id(s))
        .ok_or("技能文件夹名只能用英文字母、数字、- 和 _")?;
    if user_dir.join(skill_id).exists()
        || get(db, NS, &format!("skill:{skill_id}")).await?.is_some()
    {
        return Err("已有同名技能，请在编辑页面修改，或重命名导入文件夹".into());
    }
    inspect(from)?;
    let dir = write_package(user_dir, skill_id, &package, None)?;
    let mut record = json!({"id":skill_id,"source":"third-party","fileSource":"user","enabled":true,"filePath":dir.join("SKILL.md").to_string_lossy(),"approvedHash":fingerprint(&package,false),"approvedAt":now()});
    hydrate(&mut record, &dir)?;
    put(db, NS, &format!("skill:{skill_id}"), &record).await?;
    Ok(record)
}

fn legacy_revision(record: &Value) -> String {
    format!("{:x}", Sha256::digest(record.to_string().as_bytes()))
}

/// 导出使用文件内容，不把可能过期的数据库索引覆盖回文件。
pub fn export(record: &Value, destination: &Path) -> Result<PathBuf, String> {
    let skill_id = record["id"]
        .as_str()
        .filter(|id| valid_id(id))
        .ok_or("技能编号无效")?;
    let package = if record["filePath"].as_str().is_some() {
        let dir = crate::agent_skill_runner::skill_dir(record)?;
        std::fs::create_dir_all(destination).map_err(err)?;
        if std::fs::canonicalize(destination)
            .map_err(err)?
            .starts_with(std::fs::canonicalize(&dir).map_err(err)?)
        {
            return Err("导出位置不能在当前技能文件夹内".into());
        }
        read_package(&dir)?
    } else {
        let draft = Draft {
            id: skill_id.into(),
            name: record["name"].as_str().unwrap_or(skill_id).into(),
            description: record["description"]
                .as_str()
                .filter(|v| !v.is_empty())
                .unwrap_or("创作技能")
                .into(),
            instructions: record["instructions"].as_str().unwrap_or("").into(),
            kind: record["kind"].as_str().map(str::to_owned),
            replaces: record["replaces"].as_str().map(str::to_owned),
            revision: None,
            files: vec![],
        };
        BTreeMap::from([("SKILL.md".into(), document(&draft)?)])
    };
    write_package(destination, skill_id, &package, None)
}
