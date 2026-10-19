//! 技能里的 Python 脚本执行。
//!
//! 如实说明能做到的隔离：每个技能一个独立 venv（依赖隔离）、独立工作目录、清空环境变量、`-I` 隔离模式、
//! 超时终止整个进程组、输出上限。这些都**不限制文件访问**：脚本以当前用户的权限运行，能读到本机文件，
//! 包括应用数据库。所以第三方技能安装前向作者展示脚本全文，作者确认后记下脚本指纹，脚本之后被改动就要重新确认；
//! 内置技能的脚本随应用分发。界面上不称“沙箱”。
//!
//! 解释器按顺序找：应用数据目录里的便携 Python（固定版本、校验 SHA-256 后解压）、系统里 3.9 以上的 Python。
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const MAX_OUTPUT: usize = 2 * 1024 * 1024;
const MAX_STDERR: usize = 8 * 1024;
pub const MAX_TIMEOUT_SECS: u64 = 120;
const PBS_RELEASE: &str = "20260924";
const PBS_VERSION: &str = "3.12.14";

/// 当前平台可下载的便携 Python：（目标三元组，SHA-256）。数值取自该版本发布的 SHA256SUMS。
fn portable_asset() -> Option<(&'static str, &'static str)> {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some((
            "aarch64-apple-darwin",
            "9763f43db2481a6af36af82ec40302aab7a73632f880129d07a6e81aec846277",
        ))
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        Some((
            "x86_64-apple-darwin",
            "0d6a4a299908123f00bc844df737603f047ff9eba14fda6cad83f3cf3cb3a2af",
        ))
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        Some((
            "x86_64-pc-windows-msvc",
            "c5303174bc29f5205decf6721ac549d4eb41c448f9b8c46cbc562d00348865bb",
        ))
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some((
            "x86_64-unknown-linux-gnu",
            "5eae8cf79dd47fc2496a4fccc892936be831ce7a84d984b2299dfb1cdb592682",
        ))
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        Some((
            "aarch64-unknown-linux-gnu",
            "c0574c9c9fec561eb192ad510e4e79aaad039929b3531b3bb873e2f630fb14a9",
        ))
    } else {
        None
    }
}

pub fn agent_dir(root: &Path) -> PathBuf {
    root.join("agent")
}

fn portable_python(root: &Path) -> PathBuf {
    let dir = agent_dir(root).join("python");
    if cfg!(windows) {
        dir.join("python.exe")
    } else {
        dir.join("bin").join("python3")
    }
}

fn env_python(root: &Path, skill_id: &str) -> PathBuf {
    let dir = agent_dir(root).join("skill-env").join(skill_id);
    if cfg!(windows) {
        dir.join("Scripts").join("python.exe")
    } else {
        dir.join("bin").join("python")
    }
}

fn run_captured(cmd: &mut Command, timeout: Duration) -> Result<(bool, String, String), String> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let out = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stdout.read_to_string(&mut s);
        s
    });
    let err = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            break Some(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let (o, e) = (
        out.join().unwrap_or_default(),
        err.join().unwrap_or_default(),
    );
    match status {
        Some(s) => Ok((s.success(), o, e)),
        None => Err("命令超时".into()),
    }
}

fn python_version(python: &Path) -> Option<String> {
    let (ok, out, _) = run_captured(
        Command::new(python).args(["-c", "import sys;print('%d.%d.%d'%sys.version_info[:3])"]),
        Duration::from_secs(8),
    )
    .ok()?;
    let version = out.trim().to_owned();
    let parts: Vec<u32> = version.split('.').filter_map(|p| p.parse().ok()).collect();
    (ok && parts.len() == 3 && (parts[0], parts[1]) >= (3, 9)).then_some(version)
}

/// PATH 上的系统 Python。macOS 的 /usr/bin/python3 在没装命令行工具时会弹安装框，先确认工具已装。
fn system_python() -> Option<(PathBuf, String)> {
    let path = std::env::var_os("PATH")?;
    for name in if cfg!(windows) {
        ["python.exe", "python3.exe"]
    } else {
        ["python3", "python"]
    } {
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join(name);
            if !candidate.is_file() {
                continue;
            }
            if cfg!(target_os = "macos") && candidate == Path::new("/usr/bin/python3") {
                let tools = Command::new("/usr/bin/xcode-select")
                    .arg("-p")
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
                if !tools.is_ok_and(|s| s.success()) {
                    continue;
                }
            }
            if let Some(version) = python_version(&candidate) {
                return Some((candidate, version));
            }
        }
    }
    None
}

/// 可用的解释器：便携版优先，其次系统版。
pub fn runtime(root: &Path) -> Value {
    let portable = portable_python(root);
    if portable.is_file() {
        if let Some(version) = python_version(&portable) {
            return json!({"available":true,"source":"portable","python":portable.to_string_lossy(),"version":version,
                "portableDownloadable":portable_asset().is_some()});
        }
    }
    match system_python() {
        Some((python, version)) => {
            json!({"available":true,"source":"system","python":python.to_string_lossy(),"version":version,
            "portableDownloadable":portable_asset().is_some()})
        }
        None => json!({"available":false,"portableDownloadable":portable_asset().is_some(),
            "message":"本机没有可用的 Python 3.9 以上版本，可以下载便携 Python（只放在应用数据目录，不改动系统）"}),
    }
}

fn runtime_python(root: &Path) -> Result<PathBuf, String> {
    let rt = runtime(root);
    rt["python"].as_str().map(PathBuf::from).ok_or_else(|| {
        rt["message"]
            .as_str()
            .unwrap_or("没有可用的 Python")
            .to_owned()
    })
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = file.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// 下载固定版本的便携 Python 到应用数据目录：curl 下载、核对 SHA-256、tar 解压，都用系统自带命令。
pub fn install_portable(root: &Path) -> Result<Value, String> {
    let (triple, expected) = portable_asset()
        .ok_or("当前平台没有可下载的便携 Python，请安装系统 Python 3.9 以上版本")?;
    let dir = agent_dir(root);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let archive = dir.join("python-download.tar.gz");
    let url = format!(
        "https://github.com/astral-sh/python-build-standalone/releases/download/{PBS_RELEASE}/cpython-{PBS_VERSION}+{PBS_RELEASE}-{triple}-install_only.tar.gz"
    );
    let (ok, _, stderr) = run_captured(
        Command::new(if cfg!(windows) { "curl.exe" } else { "curl" })
            .args(["-fsSL", "--retry", "2", "-o"])
            .arg(&archive)
            .arg(&url),
        Duration::from_secs(900),
    )
    .map_err(|e| format!("下载便携 Python 失败：{e}"))?;
    if !ok {
        let _ = std::fs::remove_file(&archive);
        return Err(format!(
            "下载便携 Python 失败：{}",
            stderr.trim().chars().take(300).collect::<String>()
        ));
    }
    let actual = sha256_file(&archive)?;
    if actual != expected {
        let _ = std::fs::remove_file(&archive);
        return Err("下载的便携 Python 校验不一致，已丢弃；请检查网络后重试".into());
    }
    let staging = dir.join("python-staging");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| e.to_string())?;
    let (ok, _, stderr) = run_captured(
        Command::new(if cfg!(windows) { "tar.exe" } else { "tar" })
            .arg("-xzf")
            .arg(&archive)
            .arg("-C")
            .arg(&staging),
        Duration::from_secs(300),
    )?;
    let _ = std::fs::remove_file(&archive);
    if !ok || !staging.join("python").is_dir() {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(format!(
            "解压便携 Python 失败：{}",
            stderr.trim().chars().take(300).collect::<String>()
        ));
    }
    let target = dir.join("python");
    let _ = std::fs::remove_dir_all(&target);
    std::fs::rename(staging.join("python"), &target).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_dir_all(&staging);
    Ok(runtime(root))
}

/// 技能目录：记录里 filePath 指向 SKILL.md。
pub fn skill_dir(skill: &Value) -> Result<PathBuf, String> {
    let file = skill["filePath"]
        .as_str()
        .ok_or("这个技能没有技能文件夹，不能带脚本")?;
    Path::new(file)
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "技能路径无效".to_owned())
}

fn requirements(dir: &Path) -> Option<PathBuf> {
    [
        dir.join("requirements.txt"),
        dir.join("scripts").join("requirements.txt"),
    ]
    .into_iter()
    .find(|p| p.is_file())
}

/// 技能目录里的脚本清单（scripts/*.py），按名字排序。
pub fn scripts(dir: &Path) -> Vec<String> {
    fn collect(dir: &Path, prefix: &str, names: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || name == "__pycache__" {
                continue;
            }
            let relative = format!("{prefix}{name}");
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                collect(&entry.path(), &format!("{relative}/"), names);
            } else if kind.is_file() && relative.ends_with(".py") {
                names.push(relative);
            }
        }
    }
    let mut names = Vec::new();
    collect(&dir.join("scripts"), "", &mut names);
    names.sort();
    names
}

/// 脚本指纹：scripts/ 下全部文件与 requirements.txt 的内容；作者确认的是这一版。
pub fn script_hash(dir: &Path) -> String {
    crate::agent_skill_files::script_hash(dir).unwrap_or_default()
}

/// 第三方与作者目录里的技能要作者确认过当前这一版脚本；内置技能随应用分发。
pub fn needs_approval(skill: &Value) -> bool {
    skill["fileSource"] != "builtin"
}

fn deps_marker(root: &Path, skill_id: &str) -> PathBuf {
    agent_dir(root)
        .join("skill-env")
        .join(skill_id)
        .join(".ew-requirements")
}

fn requirements_hash(dir: &Path) -> Option<String> {
    let file = requirements(dir)?;
    Some(format!(
        "{:x}",
        Sha256::digest(std::fs::read(file).unwrap_or_default())
    ))
}

/// 技能的运行状态：解释器、依赖、作者确认，供技能页展示。
pub fn status(root: &Path, skill: &Value) -> Value {
    let mut result = json!({"scripts":[],"requirements":[],"depsReady":true,"approved":false,"scriptHash":"","runtime":Value::Null,
        "state":"missing_files","missingScripts":[],"lastRun":skill["lastRun"]});
    let dir = match skill_dir(skill) {
        Ok(dir) => dir,
        Err(error) => {
            result["error"] = json!(error);
            return result;
        }
    };
    let preview = match crate::agent_skill_files::inspect(&dir) {
        Ok(v) => v,
        Err(error) => {
            result["error"] = json!(error);
            return result;
        }
    };
    let names = scripts(&dir);
    let mut missing: Vec<String> = preview["instructions"]
        .as_str()
        .unwrap_or("")
        .split(|c: char| !c.is_ascii_alphanumeric() && !"_./-".contains(c))
        .filter_map(|s| s.strip_prefix("scripts/"))
        .filter(|s| s.ends_with(".py") && !dir.join("scripts").join(s).is_file())
        .map(str::to_owned)
        .collect();
    missing.sort();
    missing.dedup();
    let reqs: Vec<String> = preview["requirements"]
        .as_str()
        .unwrap_or("")
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_owned)
        .collect();
    let id = skill["id"].as_str().unwrap_or("");
    let deps_ready = reqs.is_empty()
        || (env_python(root, id).is_file()
            && std::fs::read_to_string(deps_marker(root, id)).ok() == requirements_hash(&dir));
    let approved = !needs_approval(skill) || skill["approvedHash"] == preview["scriptHash"];
    let rt = if names.is_empty() {
        Value::Null
    } else {
        runtime(root)
    };
    let state = if !missing.is_empty() {
        "missing_files"
    } else if names.is_empty() {
        "instructions_only"
    } else if skill["enabled"] != true {
        "disabled"
    } else if !approved {
        "pending_confirmation"
    } else if rt["available"] != true {
        "missing_runtime"
    } else if !deps_ready {
        "missing_dependencies"
    } else if skill["lastRun"]["ok"] == false
        && skill["lastRun"]["scriptHash"] == preview["scriptHash"]
    {
        "failed"
    } else {
        "ready"
    };
    result["scripts"] = json!(names);
    result["requirements"] = json!(reqs);
    result["depsReady"] = json!(deps_ready);
    result["approved"] = json!(approved);
    result["scriptHash"] = preview["scriptHash"].clone();
    result["runtime"] = rt;
    result["state"] = json!(state);
    result["missingScripts"] = json!(missing);
    result["directory"] = preview["directory"].clone();
    result
}

/// 为技能建独立 venv 并安装 requirements.txt。
pub fn install_deps(root: &Path, skill: &Value) -> Result<Value, String> {
    let id = str_id(skill)?;
    let dir = skill_dir(skill)?;
    let Some(reqs) = requirements(&dir) else {
        return Ok(status(root, skill));
    };
    let hash = crate::agent_skill_files::script_hash(&dir)?;
    if needs_approval(skill) && skill["approvedHash"].as_str() != Some(hash.as_str()) {
        return Err("请先查看并确认当前脚本与依赖，再安装依赖".into());
    }
    let req_hash = requirements_hash(&dir).ok_or("依赖文件不存在")?;
    let python = runtime_python(root)?;
    let env_dir = agent_dir(root).join("skill-env").join(id);
    let _ = std::fs::remove_dir_all(&env_dir);
    std::fs::create_dir_all(env_dir.parent().unwrap()).map_err(|e| e.to_string())?;
    let (ok, _, stderr) = run_captured(
        Command::new(&python).args(["-m", "venv"]).arg(&env_dir),
        Duration::from_secs(180),
    )?;
    if !ok {
        return Err(format!("建立依赖环境失败：{}", tail(&stderr)));
    }
    let install_source = env_dir.join("source");
    crate::agent_skill_files::snapshot(&dir, &install_source, &hash)?;
    let install_reqs = install_source.join(reqs.strip_prefix(&dir).map_err(|e| e.to_string())?);
    let (ok, _, stderr) = run_captured(
        Command::new(env_python(root, id))
            .args([
                "-m",
                "pip",
                "install",
                "--disable-pip-version-check",
                "--no-input",
                "-r",
            ])
            .arg(&install_reqs),
        Duration::from_secs(900),
    )?;
    if !ok {
        return Err(format!("安装依赖失败：{}", tail(&stderr)));
    }
    if requirements_hash(&dir).as_deref() != Some(req_hash.as_str()) {
        return Err("安装期间依赖清单被修改，请按新版本重新安装".into());
    }
    std::fs::write(deps_marker(root, id), req_hash).map_err(|e| e.to_string())?;
    Ok(status(root, skill))
}

fn str_id(skill: &Value) -> Result<&str, String> {
    skill["id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "技能编号无效".to_owned())
}

fn tail(text: &str) -> String {
    let chars: Vec<char> = text.trim().chars().collect();
    chars[chars.len().saturating_sub(600)..].iter().collect()
}

fn tool_error(code: &str, message: String) -> Value {
    json!({"ok":false,"toolError":{"code":code,"retryable":false,"message":message}})
}

/// 运行技能 scripts/ 下的一个脚本：输入 JSON 走标准输入，脚本把结果 JSON 打到标准输出。
/// 缺解释器、缺依赖、脚本未经确认、超时、输出超限都以工具错误如实返回，不假装执行过。
#[cfg(test)]
pub fn run(
    root: &Path,
    skill: &Value,
    script: &str,
    input: &Value,
    timeout_secs: u64,
) -> Result<Value, String> {
    run_with_cancellation(root, skill, script, input, timeout_secs,
        &std::sync::atomic::AtomicBool::new(false))
}

pub fn run_with_cancellation(
    root: &Path, skill: &Value, script: &str, input: &Value, timeout_secs: u64,
    cancelled: &std::sync::atomic::AtomicBool,
) -> Result<Value, String> {
    if cancelled.load(std::sync::atomic::Ordering::SeqCst) {
        return Ok(tool_error("cancelled", "任务已停止，脚本未运行".into()));
    }
    let id = str_id(skill)?;
    if skill["enabled"] != true || skill["deletedAt"].is_number() {
        return Ok(tool_error(
            "skill_disabled",
            "技能未启用，不能运行它的脚本".into(),
        ));
    }
    let valid_name =
        script.ends_with(".py") && crate::agent_skill_files::editable(&format!("scripts/{script}"));
    if !valid_name {
        return Ok(tool_error(
            "invalid_script",
            "脚本名只能是 scripts/ 目录下的 .py 文件名".into(),
        ));
    }
    let dir = match skill_dir(skill) {
        Ok(dir) => dir,
        Err(message) => return Ok(tool_error("no_scripts", message)),
    };
    let path = dir.join("scripts").join(script);
    if !path.is_file() {
        return Ok(tool_error(
            "missing_script",
            format!(
                "技能里没有脚本 {script}，可用的有：{}",
                scripts(&dir).join("、")
            ),
        ));
    }
    let current_hash = crate::agent_skill_files::script_hash(&dir)?;
    if needs_approval(skill) && skill["approvedHash"].as_str() != Some(current_hash.as_str()) {
        return Ok(tool_error(
            "script_not_approved",
            "这个技能的脚本还没经作者确认，或确认后被改动过；请作者在 Skills 页查看全文并确认"
                .into(),
        ));
    }
    let st = status(root, skill);
    let python = if st["requirements"].as_array().is_some_and(|r| !r.is_empty()) {
        if st["depsReady"] != true {
            return Ok(tool_error(
                "dependencies_missing",
                format!(
                    "脚本依赖还没安装：{}；请作者在 Skills 页安装依赖",
                    st["requirements"]
                        .as_array()
                        .map(|r| r
                            .iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join("、"))
                        .unwrap_or_default()
                ),
            ));
        }
        env_python(root, id)
    } else {
        match runtime_python(root) {
            Ok(p) => p,
            Err(message) => return Ok(tool_error("python_unavailable", message)),
        }
    };
    let work = agent_dir(root)
        .join("skill-work")
        .join(id)
        .join(crate::agent_store::id());
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let snapshot = work.join("source");
    if let Err(error) = crate::agent_skill_files::snapshot(&dir, &snapshot, &current_hash) {
        let _ = std::fs::remove_dir_all(&work);
        return Ok(tool_error("script_changed", error));
    }
    let mut cmd = Command::new(&python);
    cmd.arg("-I")
        .arg("-B")
        .arg(snapshot.join("scripts").join(script))
        .current_dir(&work)
        .env_clear();
    cmd.env("PYTHONIOENCODING", "utf-8")
        .env("PYTHONUTF8", "1")
        .env("HOME", &work)
        .env("TMPDIR", &work);
    if cfg!(windows) {
        for key in ["SYSTEMROOT", "WINDIR"] {
            if let Some(v) = std::env::var_os(key) {
                cmd.env(key, v);
            }
        }
        cmd.env("TEMP", &work).env("TMP", &work);
    } else {
        cmd.env("PATH", "/usr/bin:/bin").env("LANG", "C.UTF-8");
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let outcome = execute(
        cmd,
        input,
        Duration::from_secs(timeout_secs.clamp(1, MAX_TIMEOUT_SECS)),
        cancelled,
    );
    let _ = std::fs::remove_dir_all(&work);
    let (code, stdout, stderr, timed_out, overflow) = outcome?;
    if cancelled.load(std::sync::atomic::Ordering::SeqCst) {
        return Ok(tool_error("cancelled", "任务已停止，脚本及其子进程已终止".into()));
    }
    if timed_out {
        return Ok(tool_error(
            "timeout",
            format!(
                "脚本运行超过 {} 秒，已终止",
                timeout_secs.clamp(1, MAX_TIMEOUT_SECS)
            ),
        ));
    }
    if overflow {
        return Ok(tool_error(
            "output_too_large",
            "脚本输出超过 2MB 上限，已丢弃".into(),
        ));
    }
    if code != Some(0) {
        return Ok(
            json!({"ok":false,"toolError":{"code":"script_failed","retryable":false,"exitCode":code,
            "message":format!("脚本出错退出：{}", tail(&stderr))}}),
        );
    }
    let text = String::from_utf8_lossy(&stdout).trim().to_owned();
    Ok(match serde_json::from_str::<Value>(&text) {
        Ok(output) => json!({"ok":true,"output":output}),
        Err(_) => json!({"ok":true,"text":text.chars().take(20_000).collect::<String>()}),
    })
}

type Outcome = (Option<i32>, Vec<u8>, String, bool, bool);

fn execute(mut cmd: Command, input: &Value, timeout: Duration, cancelled: &std::sync::atomic::AtomicBool) -> Result<Outcome, String> {
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("无法启动 Python：{e}"))?;
    let payload = input.to_string();
    let mut stdin = child.stdin.take().unwrap();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(payload.as_bytes());
    });
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let out = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.take(MAX_OUTPUT as u64 + 1).read_to_end(&mut buf);
        buf
    });
    let err = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr.take(MAX_STDERR as u64).read_to_end(&mut buf);
        String::from_utf8_lossy(&buf).into_owned()
    });
    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            break Some(status);
        }
        if cancelled.load(std::sync::atomic::Ordering::SeqCst) || Instant::now() >= deadline {
            timed_out = !cancelled.load(std::sync::atomic::Ordering::SeqCst);
            kill_tree(&mut child);
            break child.wait().ok();
        }
        std::thread::sleep(Duration::from_millis(30));
    };
    // A script may exit while descendants still hold its pipes. They belong to this run too.
    kill_tree(&mut child);
    let _ = writer.join();
    let stdout = out.join().unwrap_or_default();
    let overflow = stdout.len() > MAX_OUTPUT;
    if overflow && !timed_out {
        kill_tree(&mut child);
    }
    Ok((
        status.and_then(|s| s.code()),
        stdout,
        err.join().unwrap_or_default(),
        timed_out,
        overflow,
    ))
}

/// 超时后连同脚本拉起的子进程一起终止。
fn kill_tree(child: &mut std::process::Child) {
    #[cfg(unix)]
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(["/PID", &child.id().to_string(), "/T", "/F"])
            .output();
    }
    let _ = child.kill();
}

/// 待安装的技能文件夹：SKILL.md 与全部脚本的全文、依赖清单与指纹，给作者逐个过目。不写入任何东西。
pub fn inspect(folder: &Path) -> Result<Value, String> {
    let preview = crate::agent_skill_files::inspect(folder)?;
    if !crate::agent_skill_files::valid_id(preview["id"].as_str().unwrap_or("")) {
        return Err("技能文件夹名只能用英文字母、数字、- 和 _".into());
    }
    Ok(preview)
}

/// 导出与编辑、导入共用技能文件服务。
pub fn export(skill: &Value, dest: &Path) -> Result<PathBuf, String> {
    crate::agent_skill_files::export(skill, dest)
}
