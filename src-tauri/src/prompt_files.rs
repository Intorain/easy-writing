//! 提示词与技能包的统一文件范围：普通提示词只带文档，skills/<id> 带完整资源。
//! 备份、恢复及恢复日志必须使用同一范围，避免回滚时留下新脚本。
use std::{
    fs,
    path::{Path, PathBuf},
};

pub fn files(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut out = Vec::new();
    if root.exists() {
        visit(root, root, &mut out)?;
    }
    Ok(out)
}

fn visit(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    if fs::symlink_metadata(dir)
        .map_err(|e| e.to_string())?
        .file_type()
        .is_symlink()
    {
        return Err("提示词或技能目录不能包含符号链接".into());
    }
    for entry in fs::read_dir(dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || name == "__pycache__" {
            continue;
        }
        let path = entry.path();
        let relative = path.strip_prefix(root).map_err(|e| e.to_string())?;
        let parts: Vec<_> = relative.iter().collect();
        if parts.first().is_some_and(|v| *v == "skills")
            && parts.len() >= 2
            && !crate::agent_skill_files::valid_id(&parts[1].to_string_lossy())
        {
            continue;
        }
        let kind = entry.file_type().map_err(|e| e.to_string())?;
        if kind.is_symlink() {
            return Err(format!(
                "提示词或技能目录含符号链接：{}",
                relative.display()
            ));
        }
        if kind.is_dir() {
            visit(root, &path, out)?;
        } else if kind.is_file() {
            let skill = parts.len() >= 3 && parts[0] == "skills";
            let document = path
                .extension()
                .and_then(|v| v.to_str())
                .is_some_and(|v| v.eq_ignore_ascii_case("md") || v.eq_ignore_ascii_case("txt"));
            if skill || document {
                out.push(path);
            }
        } else {
            return Err(format!("不支持的提示词文件：{}", relative.display()));
        }
    }
    out.sort();
    Ok(())
}

/// 不跟随已有的目录链接写到用户选择范围之外。
pub fn checked_target(root: &Path, relative: &Path) -> Result<PathBuf, String> {
    if fs::symlink_metadata(root).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err("恢复目标目录不能是符号链接".into());
    }
    let mut target = root.to_path_buf();
    for component in relative.components() {
        if !matches!(component, std::path::Component::Normal(_)) {
            return Err("无效的提示词相对路径".into());
        }
        target.push(component);
        if fs::symlink_metadata(&target).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err("恢复目标包含符号链接".into());
        }
    }
    Ok(target)
}
