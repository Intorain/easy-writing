//! 网页桥：只能在应用网页层完成的调用（榜单抓取、画趋势图、发微信文件与图片、灵感便签、码字统计、按封面方案出图），
//! 由宿主发给主窗口的执行器并等它回传。只接受本地主窗口注册与回传；旧执行器、任务停止后的迟到结果一律拒绝。
//! 榜单工具的联网确认在 agent_rank_bridge；这里管传输、通道能力检查、审批门与回执。
use crate::{agent_channel, agent_store::*, DesktopAppHandle, DesktopWebviewWindow};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Mutex,
    },
    time::{Duration, Instant},
};
use tauri::{Emitter, Manager};

pub const REQUEST_EVENT: &str = "agent-webview-request";
pub const CANCEL_EVENT: &str = "agent-webview-cancel";

#[derive(Default)]
pub struct WebviewExecutor {
    state: Mutex<ExecutorState>,
}

#[derive(Default)]
pub(crate) struct ExecutorState {
    client_id: Option<String>,
    pending: HashMap<String, mpsc::Sender<ExecutorEvent>>,
}

#[derive(Debug, PartialEq)]
pub(crate) enum ExecutorEvent {
    Completed(Value),
    Progress { completed: usize, total: usize },
}

impl ExecutorState {
    pub(crate) fn register(&mut self, client_id: String) {
        for (_, sender) in self.pending.drain() {
            let _ = sender.send(ExecutorEvent::Completed(unavailable(
                "executor_unavailable",
                "应用执行环境已重新加载，请重试",
            )));
        }
        self.client_id = Some(client_id);
    }

    pub(crate) fn complete(&mut self, client_id: &str, id: &str, result: Value) -> bool {
        if self.client_id.as_deref() != Some(client_id) {
            return false;
        }
        self.pending
            .remove(id)
            .is_some_and(|sender| sender.send(ExecutorEvent::Completed(result)).is_ok())
    }

    pub(crate) fn progress(&self, client_id: &str, id: &str, completed: usize, total: usize) -> bool {
        self.client_id.as_deref() == Some(client_id)
            && total > 0
            && total <= 1000
            && completed <= total
            && self.pending.get(id).is_some_and(|sender| {
                sender
                    .send(ExecutorEvent::Progress { completed, total })
                    .is_ok()
            })
    }

    #[cfg(test)]
    pub(crate) fn insert(&mut self, id: &str, sender: mpsc::Sender<ExecutorEvent>) {
        self.pending.insert(id.to_owned(), sender);
    }

    #[cfg(test)]
    pub(crate) fn waiting(&self, id: &str) -> bool {
        self.pending.contains_key(id)
    }
}

pub fn unavailable(code: &str, message: &str) -> Value {
    json!({"status":code,"error":{"code":code,"message":message}})
}

pub struct Budget {
    pub deadline: Duration,
    pub idle: Duration,
}

/// 向主窗口发一个请求并等它回传。`extra` 合并进事件（如榜单的 networkApproved）。
/// 进度只接受总数不变且前进的更新：重复或倒退的心跳不延长超时，失去响应的执行器挂不住任务。
pub fn request(
    app: &DesktopAppHandle,
    tool: &str,
    payload: Value,
    extra: Value,
    budget: Budget,
    cancelled: &AtomicBool,
    mut on_progress: impl FnMut(usize, usize),
) -> Result<Value, String> {
    let executor = app.state::<WebviewExecutor>();
    let request_id = id();
    let (sender, receiver) = mpsc::channel();
    let client_id = {
        let mut state = executor.state.lock().map_err(err)?;
        let Some(client_id) = state.client_id.clone() else {
            return Ok(unavailable(
                "executor_unavailable",
                "应用执行环境尚未就绪，请稍后重试",
            ));
        };
        state.pending.insert(request_id.clone(), sender);
        client_id
    };
    let mut event = json!({"id":request_id,"clientId":client_id,"tool":tool,"request":payload});
    for (key, value) in extra.as_object().into_iter().flatten() {
        event[key] = value.clone();
    }
    let emitted = app.emit_to("main", REQUEST_EVENT, event);
    let started = Instant::now();
    let mut last_activity = started;
    let mut progress: Option<(usize, usize)> = None;
    let result = if emitted.is_err() {
        unavailable("executor_unavailable", "无法连接应用执行环境")
    } else {
        loop {
            if cancelled.load(Ordering::SeqCst) {
                break unavailable("cancelled", "任务已取消");
            }
            if started.elapsed() >= budget.deadline || last_activity.elapsed() >= budget.idle {
                break unavailable("executor_timeout", "应用执行环境响应超时，本次没有拿到结果");
            }
            match receiver.recv_timeout(Duration::from_millis(100)) {
                Ok(ExecutorEvent::Completed(result)) => break result,
                Ok(ExecutorEvent::Progress { completed, total }) => {
                    if progress.is_none_or(|(previous, expected)| total == expected && completed > previous) {
                        progress = Some((completed, total));
                        last_activity = Instant::now();
                        on_progress(completed, total);
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(_) => break unavailable("executor_unavailable", "应用执行环境已断开"),
            }
        }
    };
    executor
        .state
        .lock()
        .map_err(err)?
        .pending
        .remove(&request_id);
    if matches!(result["status"].as_str(), Some("cancelled" | "executor_timeout")) {
        let _ = app.emit_to(
            "main",
            CANCEL_EVENT,
            json!({"id":request_id,"clientId":client_id}),
        );
    }
    Ok(result)
}

/// 工具开始与进度写进会话：开始事件由宿主在真正执行时发出，模型一次发出多个工具调用时界面也准确。
pub fn report(app: &DesktopAppHandle, root: &Path, sid: &str, run: &str, event: &Value) {
    if let Ok(session) =
        crate::blocking::run(crate::agent_host::apply_event(root, sid, run, event, ""))
    {
        crate::agent_events::emit(app, "agent-session", session);
    }
}

fn require_main(window: &DesktopWebviewWindow) -> Result<(), String> {
    if window.label() != "main" || !window.url().is_ok_and(|url| crate::is_app_shell_url(&url)) {
        return Err("执行器只接受本地主窗口".into());
    }
    Ok(())
}

#[tauri::command]
pub fn webview_executor_ready(
    window: DesktopWebviewWindow,
    app: DesktopAppHandle,
    client_id: String,
) -> Result<(), String> {
    require_main(&window)?;
    uuid::Uuid::parse_str(&client_id).map_err(|_| "执行器编号无效")?;
    let executor = app.state::<WebviewExecutor>();
    let mut state = executor.state.lock().map_err(err)?;
    state.register(client_id);
    Ok(())
}

#[tauri::command]
pub fn webview_executor_complete(
    window: DesktopWebviewWindow,
    app: DesktopAppHandle,
    client_id: String,
    id: String,
    result: Value,
) -> Result<bool, String> {
    require_main(&window)?;
    if !result.is_object() || result.to_string().len() > 2_000_000 {
        return Err("执行结果格式无效或超过大小限制".into());
    }
    let executor = app.state::<WebviewExecutor>();
    let mut state = executor.state.lock().map_err(err)?;
    Ok(state.complete(&client_id, &id, result))
}

#[tauri::command]
pub fn webview_executor_progress(
    window: DesktopWebviewWindow,
    app: DesktopAppHandle,
    client_id: String,
    id: String,
    completed: usize,
    total: usize,
) -> Result<bool, String> {
    require_main(&window)?;
    let executor = app.state::<WebviewExecutor>();
    let state = executor.state.lock().map_err(err)?;
    Ok(state.progress(&client_id, &id, completed, total))
}

// ---------------------------------------------------------------------------
// 经网页桥执行的工具：灵感便签、码字统计（数据在前端设置缓存里）、发文件、发图（微信媒体通道、echarts）
// ---------------------------------------------------------------------------

pub fn handles(name: &str) -> bool {
    matches!(
        name,
        "list_inspirations" | "add_inspiration" | "writing_stats" | "send_chapter_file" | "send_rank_chart" | "generate_cover"
    )
}

const TOOL_BUDGET: Budget = Budget {
    deadline: Duration::from_secs(180),
    idle: Duration::from_secs(90),
};
/// 出图是一次长等待，中途没有进度可报
const COVER_BUDGET: Budget = Budget {
    deadline: Duration::from_secs(300),
    idle: Duration::from_secs(300),
};

fn budget(name: &str) -> Budget {
    if name == "generate_cover" { COVER_BUDGET } else { TOOL_BUDGET }
}
/// iLink 单个媒体文件上限 25 MiB；文本导出远到不了，留作兜底
const MEDIA_MAX_BYTES: usize = 25 * 1024 * 1024;

enum Prepared {
    Done(Value),
    Send { payload: Value, channel: Value },
}

fn tool_error(code: &str, message: &str) -> Value {
    json!({"ok":false,"toolError":{"code":code,"retryable":false,"message":message}})
}

pub fn execute(
    app: &DesktopAppHandle,
    root: &Path,
    sid: &str,
    run: &str,
    name: &str,
    args: &Value,
    cancelled: &AtomicBool,
) -> Result<Value, String> {
    let mut args = crate::agent_contract::canonical_arguments(args);
    if let Some(error) = crate::agent_contract::tool_error(name, &args)? {
        return Ok(error);
    }
    let receipt_key = args["operation_id"]
        .as_str()
        .filter(|v| !v.is_empty())
        .map(|op| format!("webview:{sid}:{op}"));
    // 会话、通道、回执、审批与（发文件时）正文都在一次连接里备好；网页层只拿到要发的内容
    let prepared = crate::blocking::run(async {
        let mut db = crate::desktop_storage::connect_existing_at(root).await?;
        let s = session(&mut db, sid).await?;
        if s["runId"] != run || s["status"] != "running" {
            return Err("任务已停止或失效".to_string());
        }
        if let Some(key) = &receipt_key {
            if let Some(receipt) = get(&mut db, OP_NS, key).await? {
                if receipt["tool"] != name || receipt["request"] != args {
                    return Err("重复操作编号对应不同请求".into());
                }
                return Ok(Prepared::Done(receipt["result"].clone()));
            }
        }
        let channel = agent_channel::of_session(&s);
        let required = match name {
            "send_chapter_file" => Some(("canSendFile", "文件")),
            "send_rank_chart" => Some(("canSendImage", "图片")),
            _ => None,
        };
        if let Some((field, label)) = required {
            if channel[field] != true {
                return Ok(Prepared::Done(tool_error(
                    "channel_unsupported",
                    &format!("当前对话入口不能发{label}；告诉作者到桌面端查看或导出。"),
                )));
            }
        }
        if let Some(error) = crate::agent_tool_boundary::prepare(&mut db, &s, name, &mut args).await? {
            return Ok(Prepared::Done(error));
        }
        // 出图先预检（方案、作品、图片模型），不通过就不发确认卡
        let cover = match name {
            "generate_cover" => match crate::agent_packaging::cover_job(&mut db, &s, &args).await? {
                Ok(job) => Some(job),
                Err(error) => return Ok(Prepared::Done(error)),
            },
            _ => None,
        };
        if let Some(pending) = crate::agent_approval::gate(&mut db, &s, run, name, &args, &Value::Null).await? {
            return Ok(Prepared::Done(pending));
        }
        let payload = match (name, cover) {
            ("send_chapter_file", _) => match chapter_file(&mut db, &s, &args).await? {
                Ok(payload) => payload,
                Err(error) => return Ok(Prepared::Done(error)),
            },
            (_, Some(job)) => job,
            _ => args.clone(),
        };
        Ok(Prepared::Send {
            payload,
            channel: channel["kind"].clone(),
        })
    })?;
    let (payload, channel) = match prepared {
        Prepared::Done(value) => return Ok(value),
        Prepared::Send { payload, channel } => (payload, channel),
    };
    report(app, root, sid, run, &json!({"event":"tool_started","name":name}));
    let result = request(app, name, payload, json!({"channel":channel}), budget(name), cancelled, |_, _| {})?;
    let succeeded = result["toolError"].is_null() && result["error"].is_null() && result["ok"] != false;
    if let (Some(key), true) = (&receipt_key, succeeded) {
        crate::blocking::run(async {
            let mut db = crate::desktop_storage::connect_existing_at(root).await?;
            let s = session(&mut db, sid).await?;
            if s["runId"] != run || s["status"] != "running" {
                return Err("任务已停止或失效".to_string());
            }
            put(&mut db, OP_NS, key, &json!({"tool":name,"request":args,"result":result})).await
        })?;
    }
    Ok(result)
}

fn id_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

fn safe_file_name(name: &str) -> String {
    let cleaned: String = name
        .trim()
        .chars()
        .map(|c| if c.is_control() || "\\/:*?\"<>|".contains(c) { '_' } else { c })
        .collect();
    if cleaned.is_empty() {
        "export".to_owned()
    } else {
        cleaned
    }
}

fn word_count(text: &str) -> usize {
    text.chars().filter(|c| !c.is_whitespace() && *c != '\u{feff}').count()
}

/// 把一章或整本书拼成 txt 内容；发送由网页层完成，这里只准备文件
async fn chapter_file(
    db: &mut sqlx::SqliteConnection,
    s: &Value,
    args: &Value,
) -> Result<Result<Value, Value>, String> {
    let bid = str_field(args, "book_id")?;
    crate::agent_service::allowed_book(db, s, bid, false).await?;
    let book = book(db, bid).await?;
    let book_title = book["title"].as_str().unwrap_or("作品").to_owned();
    let entries = catalog(db, bid).await?;
    let cid = args["chapter_id"].as_str().unwrap_or("").trim();
    let (file_name, text, summary) = if cid.is_empty() {
        if entries.is_empty() {
            return Ok(Err(tool_error("no_content", &format!("《{book_title}》还没有章节，没有可导出的内容。"))));
        }
        let volumes = crate::agent_service::volumes(db, bid).await?;
        let mut out = format!("{book_title}\n");
        if let Some(intro) = book["intro"].as_str().filter(|v| !v.trim().is_empty()) {
            out.push_str(&format!("\n{}\n", intro.trim()));
        }
        for (index, volume) in volumes.iter().enumerate() {
            let vid = id_text(&volume["id"]);
            out.push_str(&format!("\n\n第{}卷 {}\n", index + 1, volume["title"].as_str().unwrap_or("")));
            for entry in entries.iter().filter(|e| id_text(&e["volumeId"]) == vid) {
                let body = chapter(db, bid, &id_text(&entry["id"])).await?;
                out.push_str(&format!(
                    "\n{}\n\n{}\n",
                    entry["title"].as_str().unwrap_or(""),
                    body["textContent"].as_str().unwrap_or("").trim()
                ));
            }
        }
        (
            format!("{}-{}.txt", safe_file_name(&book_title), chrono::Local::now().format("%Y%m%d")),
            out,
            format!("《{book_title}》整本 {} 章", entries.len()),
        )
    } else {
        let position = entries.iter().position(|e| id_text(&e["id"]) == cid);
        let body = chapter(db, bid, cid).await?;
        let title = position
            .and_then(|i| entries[i]["title"].as_str())
            .or(body["title"].as_str())
            .unwrap_or("")
            .to_owned();
        let content = body["textContent"].as_str().unwrap_or("").trim().to_owned();
        let label = match position {
            Some(i) => format!("第{}章《{title}》", i + 1),
            None => format!("《{title}》"),
        };
        if content.is_empty() {
            return Ok(Err(tool_error("no_content", &format!("{label}还没有正文，没有可导出的内容。"))));
        }
        (
            format!("{}-{}.txt", safe_file_name(&book_title), safe_file_name(&title)),
            format!("{title}\n\n{content}\n"),
            format!("《{book_title}》{label}"),
        )
    };
    if text.len() > MEDIA_MAX_BYTES {
        return Ok(Err(tool_error(
            "too_large",
            &format!("导出内容有 {} MB，超过微信文件上限，请作者到桌面端导出。", text.len() / 1024 / 1024),
        )));
    }
    let words = word_count(&text);
    Ok(Ok(json!({"kind":"file","fileName":file_name,"text":text,"summary":summary,"words":words})))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reloading_executor_releases_waiters_and_late_clients_cannot_complete_new_requests() {
        let mut state = ExecutorState::default();
        state.register("old".into());
        let (sender, receiver) = mpsc::channel();
        state.insert("first", sender);
        state.register("new".into());
        assert_eq!(
            receiver.recv().unwrap(),
            ExecutorEvent::Completed(unavailable("executor_unavailable", "应用执行环境已重新加载，请重试"))
        );
        let (sender, receiver) = mpsc::channel();
        state.insert("second", sender);
        assert!(!state.complete("old", "second", json!({"status":"available"})));
        assert!(state.waiting("second"));
        assert!(state.complete("new", "second", json!({"status":"available"})));
        assert_eq!(receiver.recv().unwrap(), ExecutorEvent::Completed(json!({"status":"available"})));
        assert!(!state.complete("new", "second", json!({"status":"available"})));
    }

    #[test]
    fn progress_preserves_waiter_and_rejects_stale_or_invalid_updates() {
        let mut state = ExecutorState::default();
        state.register("current".into());
        let (sender, receiver) = mpsc::channel();
        state.insert("range", sender);
        assert!(!state.progress("old", "range", 0, 37));
        assert!(!state.progress("current", "range", 38, 37));
        assert!(state.progress("current", "range", 1, 37));
        assert_eq!(receiver.recv().unwrap(), ExecutorEvent::Progress { completed: 1, total: 37 });
        assert!(state.waiting("range"));
        assert!(state.complete("current", "range", json!({"status":"partial"})));
        assert_eq!(receiver.recv().unwrap(), ExecutorEvent::Completed(json!({"status":"partial"})));
        assert!(!state.progress("current", "range", 2, 37));
        assert!(handles("send_chapter_file") && handles("generate_cover") && !handles("get_rank"));
        assert!(budget("generate_cover").idle > budget("send_rank_chart").idle);
    }

    #[test]
    fn file_names_drop_path_characters_and_word_count_ignores_whitespace() {
        assert_eq!(safe_file_name(" 第3章: 夜袭/反击? "), "第3章_ 夜袭_反击_");
        assert_eq!(safe_file_name("   "), "export");
        assert_eq!(word_count("夜色 压下来。\n\n他走了。"), 10);
    }
}
