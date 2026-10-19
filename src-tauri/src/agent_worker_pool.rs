//! 常驻 worker 池：Agent 进程按会话常驻，一轮结束不退出，等下一轮载荷；空闲超时回收，崩溃下次重启。
//!
//! 协议仍是 stdio 一行一条 JSON：宿主写一行载荷，worker 回若干事件与 RPC 请求，
//! 以 `done` 或 `failed` 事件结束本轮，然后阻塞等下一行。标准错误输出落日志文件，
//! 最后几行留在内存里，进程中断时随错误信息带出，不再只有一句"进程中断"。

use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// 空闲多久回收进程；一本书聊几轮通常间隔几分钟，十分钟内不重复付启动开销
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(600);
const REAP_TICK: Duration = Duration::from_secs(30);
const TAIL_LINES: usize = 30;
const TAIL_CHARS: usize = 300;
const LOG_LIMIT: u64 = 5 * 1024 * 1024;
const MAX_LINE: usize = 8_000_000;
pub const LOG_FILE: &str = "agent-worker.log";

pub struct Resident {
    pub key: String,
    pub pid: u32,
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    tail: Arc<Mutex<VecDeque<String>>>,
    /// 本进程已跑的轮数，排障时用
    pub runs: u32,
}

struct Idle {
    resident: Resident,
    since: Instant,
}

static POOL: OnceLock<Mutex<HashMap<String, Idle>>> = OnceLock::new();
static REAPER: OnceLock<()> = OnceLock::new();

fn pool() -> &'static Mutex<HashMap<String, Idle>> {
    POOL.get_or_init(|| Mutex::new(HashMap::new()))
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// 把子进程的标准错误输出逐行写进日志文件，并保留末尾几行。索引器等一次性进程也用它。
pub fn attach_stderr(child: &mut Child, key: &str, log_dir: &Path) -> Arc<Mutex<VecDeque<String>>> {
    let tail: Arc<Mutex<VecDeque<String>>> = Arc::new(Mutex::new(VecDeque::new()));
    let Some(stderr) = child.stderr.take() else {
        return tail;
    };
    let pid = child.id();
    let key = key.to_owned();
    let log_dir = log_dir.to_path_buf();
    let sink = tail.clone();
    std::thread::spawn(move || {
        let _ = fs::create_dir_all(&log_dir);
        let path = log_dir.join(LOG_FILE);
        for line in BufReader::new(stderr).lines() {
            let Ok(line) = line else { break };
            let clipped: String = line.chars().take(TAIL_CHARS).collect();
            {
                let mut tail = sink.lock().unwrap();
                if tail.len() >= TAIL_LINES {
                    tail.pop_front();
                }
                tail.push_back(clipped.clone());
            }
            rotate_if_large(&path);
            if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(&path) {
                let stamp = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%.3f");
                let _ = writeln!(file, "{stamp} [{key} {pid}] {clipped}");
            }
        }
    });
    tail
}

fn rotate_if_large(path: &Path) {
    if fs::metadata(path).map(|m| m.len() > LOG_LIMIT).unwrap_or(false) {
        let _ = fs::rename(path, path.with_extension("log.1"));
    }
}

fn spawn(key: &str, mut command: Command, log_dir: &Path) -> Result<Resident, String> {
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("LANGSMITH_TRACING", "false")
        .env("LANGCHAIN_TRACING_V2", "false");
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("无法启动 Agent 后台：{e}"))?;
    let stdin = child.stdin.take().ok_or("无法连接后台输入")?;
    let stdout = child.stdout.take().ok_or("无法连接后台输出")?;
    let tail = attach_stderr(&mut child, key, log_dir);
    Ok(Resident {
        key: key.to_owned(),
        pid: child.id(),
        child,
        stdin,
        stdout: BufReader::new(stdout),
        tail,
        runs: 0,
    })
}

/// 取某个键（会话）的常驻进程：池里有活的就复用，否则按给定命令起一个。取出后由调用方持有，跑完要 `release`。
pub fn acquire(
    key: &str,
    log_dir: &Path,
    command: impl FnOnce() -> Result<Command, String>,
) -> Result<Resident, String> {
    let existing = pool().lock().unwrap().remove(key);
    if let Some(mut idle) = existing {
        if idle.resident.child.try_wait().ok().flatten().is_none() {
            return Ok(idle.resident);
        }
        let _ = idle.resident.child.wait();
    }
    spawn(key, command()?, log_dir)
}

/// 本轮跑完放回池里等下一轮；回收线程按空闲时间清理。
pub fn release(resident: Resident) {
    pool().lock().unwrap().insert(
        resident.key.clone(),
        Idle {
            resident,
            since: Instant::now(),
        },
    );
    REAPER.get_or_init(|| {
        std::thread::spawn(|| loop {
            std::thread::sleep(REAP_TICK);
            reap_idle(IDLE_TIMEOUT);
        });
    });
}

/// 进程不能再用（崩溃、协议错乱、作者停止）：杀掉，不放回池里。
pub fn discard(mut resident: Resident) {
    let _ = resident.child.kill();
    let _ = resident.child.wait();
}

/// 作者停止某个会话时把它空闲中的进程也收掉；正在跑的由 Job 的进程号处理。
pub fn kill(key: &str) {
    if let Some(idle) = pool().lock().unwrap().remove(key) {
        discard(idle.resident);
    }
}

/// 应用退出：不留孤儿进程。
pub fn kill_all() {
    let all: Vec<Idle> = pool().lock().unwrap().drain().map(|(_, v)| v).collect();
    for idle in all {
        discard(idle.resident);
    }
}

/// 回收空闲超过 `max_idle` 的进程；返回回收数量。
pub fn reap_idle(max_idle: Duration) -> usize {
    let expired: Vec<Idle> = {
        let mut guard = pool().lock().unwrap();
        let keys: Vec<String> = guard
            .iter()
            .filter(|(_, idle)| idle.since.elapsed() >= max_idle)
            .map(|(k, _)| k.clone())
            .collect();
        keys.into_iter().filter_map(|k| guard.remove(&k)).collect()
    };
    let count = expired.len();
    for idle in expired {
        discard(idle.resident);
    }
    count
}

#[cfg(test)]
pub fn idle_count() -> usize {
    pool().lock().unwrap().len()
}

/// 进程最近的错误输出，进程中断时随错误信息带出
pub fn diagnostics(resident: &Resident) -> String {
    let tail = resident.tail.lock().unwrap();
    tail.iter().cloned().collect::<Vec<_>>().join("\n")
}

pub enum TurnEnd {
    /// worker 以 done / failed 事件结束了本轮，进程还活着
    Finished,
    /// 没等到结束事件进程就断了；附最近的错误输出
    Crashed(String),
}

/// 跑一轮：写载荷，读事件直到本轮结束。RPC 请求交给 `on_rpc` 处理并把结果写回；其他事件交给 `on_event`。
pub fn run_turn(
    resident: &mut Resident,
    payload: &Value,
    mut on_rpc: impl FnMut(&Value) -> Result<Value, String>,
    mut on_event: impl FnMut(&Value) -> Result<(), String>,
) -> Result<TurnEnd, String> {
    resident.runs += 1;
    writeln!(resident.stdin, "{payload}").map_err(|e| format!("无法把任务交给 Agent 后台：{e}"))?;
    resident.stdin.flush().map_err(err)?;
    let mut line = String::new();
    loop {
        line.clear();
        let read = resident.stdout.read_line(&mut line).map_err(err)?;
        if read == 0 {
            let _ = resident.child.wait();
            return Ok(TurnEnd::Crashed(diagnostics(resident)));
        }
        if line.len() > MAX_LINE {
            return Err("后台输出超过协议限制".into());
        }
        let event: Value = serde_json::from_str(line.trim_end())
            .map_err(|_| "后台返回无效数据".to_string())?;
        if let Some(request) = event["rpc"].as_str() {
            let response = match on_rpc(&event) {
                Ok(v) => serde_json::json!({"rpc":request,"result":v}),
                Err(e) => serde_json::json!({"rpc":request,"error":e}),
            };
            writeln!(resident.stdin, "{response}").map_err(err)?;
            resident.stdin.flush().map_err(err)?;
            continue;
        }
        on_event(&event)?;
        if matches!(event["event"].as_str(), Some("done" | "failed")) {
            return Ok(TurnEnd::Finished);
        }
    }
}

/// 日志目录：数据目录下的 logs，与微信机器人、码字日志同处
pub fn log_dir(root: &Path) -> PathBuf {
    root.join("logs")
}
