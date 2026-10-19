//! 后台任务不依赖 WebView；每个会话同一时刻只允许一个宿主进程执行。
//! stdio 为私有双向协议：凭据只通过 stdin 传入，工具与检查点统一回到平台数据服务。
use crate::{agent_service as service, agent_store::*, DesktopAppHandle};
use serde_json::{json, Value};
use std::{
    collections::{BTreeSet, HashMap},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tauri::Manager;

#[derive(Default)]
pub struct AgentHost {
    pub(crate) gate: tokio::sync::Mutex<()>,
    pub(crate) storage_ready: AtomicBool,
    jobs: Mutex<HashMap<String, Arc<Job>>>,
    pub quitting: AtomicBool,
    maintenance: AtomicBool,
}
struct Job {
    pid: AtomicU32,
    cancelled: Arc<AtomicBool>,
}
/// 索引器在任务表里的固定键；它可以被关窗流程直接停止，不像会话任务那样让窗口转为隐藏。
const INDEXER_JOB: &str = "__indexer__";
/// 参考书（拆书）索引通道：与作者作品的索引并行，一轮只领几章，暂停能很快生效。
/// 和索引器一样可以被关窗直接停止，事件留在 outbox，下次启动接着拆。
const REFERENCE_JOB: &str = "__reference_indexer__";
const DISTILLATION_JOB: &str = "__distillation__";
const SEARCH_JOB: &str = "__reference_search__";
const REFERENCE_BATCH: usize = 4;

impl AgentHost {
    pub fn active(&self) -> bool {
        self.jobs.lock().unwrap().len() > 0
    }
    /// 不含索引器的活动会话任务数。
    pub fn active_sessions(&self) -> usize {
        self.jobs
            .lock()
            .unwrap()
            .keys()
            .filter(|k| k.as_str() != INDEXER_JOB && k.as_str() != REFERENCE_JOB && k.as_str() != SEARCH_JOB && k.as_str() != DISTILLATION_JOB)
            .count()
    }
    pub fn stop(&self, session_id: &str) {
        if let Some(job) = self.jobs.lock().unwrap().get(session_id) {
            stop_job(job);
        }
        crate::agent_worker_pool::kill(session_id);
    }
    pub fn stop_all(&self) {
        for job in self.jobs.lock().unwrap().values() {
            stop_job(job);
        }
        // 空闲中的常驻进程一起收掉，退出不留孤儿进程
        crate::agent_worker_pool::kill_all();
    }
}
fn stop_job(job: &Job) {
    job.cancelled.store(true, Ordering::SeqCst);
    let pid = job.pid.load(Ordering::SeqCst);
    if pid == 0 {
        return;
    }
    #[cfg(unix)]
    unsafe {
        libc::kill(-(pid as i32), libc::SIGTERM);
    }
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .output();
    }
}

#[tauri::command]
pub async fn agent_workspace(
    app: DesktopAppHandle,
    action: String,
    args: Value,
) -> Result<Value, String> {
    let host = app.state::<AgentHost>();
    let root = crate::desktop_storage::root(&app)?;
    if action == "activity" {
        return Ok(json!({"active":host.active()}));
    }
    if action == "stop" {
        host.stop(str_field(&args, "id")?);
        return Ok(json!(true));
    }
    if action == "index_now" {
        schedule_index(app.clone(), Duration::from_millis(0));
        return Ok(json!(true));
    }
    // 恢复与一次性成果应用/记录变更共用入口锁，不能只检查后台进程数量。
    let _gate = host.gate.lock().await;
    if action == "maintenance" {
        let mut jobs = host.jobs.lock().unwrap();
        if !jobs.is_empty() {
            return Err("还有 Agent 任务在执行，请先停止再恢复数据".into());
        }
        host.maintenance.store(true, Ordering::SeqCst);
        jobs.clear();
        return Ok(json!(true));
    }
    if action == "end_maintenance" {
        host.maintenance.store(false, Ordering::SeqCst);
        return Ok(json!(true));
    }
    if action == "prepare_close" {
        if host.active_sessions() > 0 && !host.quitting.load(Ordering::SeqCst) {
            return Ok(json!("hide"));
        }
        host.stop_all();
        for _ in 0..100 {
            if !host.active() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        if host.active() {
            return Err("后台任务仍在保存进度，请稍后退出".into());
        }
        return Ok(json!("close"));
    }
    if host.maintenance.load(Ordering::SeqCst) {
        return Err("数据恢复期间不可操作 Agent，请重启后继续".into());
    }
    if action == "delete_skill" {
        if host.active_sessions() > 0 { return Err("请先停止 Agent 任务，再卸载技能".into()); }
        let _files = crate::agent_skill_lifecycle::gate().lock().await;
        return crate::agent_skill_lifecycle::uninstall(&root, &crate::ensure_prompt_dir()?.join("skills"), str_field(&args, "id")?, None).await;
    }
    if action == "reset_skill" {
        let dirs = skill_dirs(&app);
        let mut db = crate::desktop_storage::connect_existing_at(&root).await?;
        let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
        let record = crate::agent_skills::reset(&mut tx, &dirs.0, dirs.1.as_deref(), str_field(&args, "id")?).await?;
        tx.commit().await.map_err(err)?;
        return Ok(record);
    }
    if action == "load" {
        if !host.jobs.lock().unwrap().contains_key(DISTILLATION_JOB) {
            let mut db=crate::desktop_storage::connect_existing_at(&root).await?;
            let mut tx=crate::desktop_storage::begin_write(&mut db).await?;
            crate::agent_distillation::recover(&mut tx).await?;
            tx.commit().await.map_err(err)?;
        }
        // 先把随应用分发的技能文件与作者目录同步进记录，再读工作区；同步失败不阻塞加载
        let dirs = skill_dirs(&app);
        let sync_result = async {
            let _files = crate::agent_skill_lifecycle::gate().lock().await;
            let mut db = crate::desktop_storage::connect_existing_at(&root).await?;
            let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
            if let Some(dir) = dirs.1.as_deref() { crate::agent_skill_lifecycle::recover(&mut tx, &root, dir).await?; }
            let summary = crate::agent_skills::sync(&mut tx, &dirs.0, dirs.1.as_deref()).await?;
            tx.commit().await.map_err(err)?;
            Ok::<_, String>(summary)
        }
        .await;
        let mut data = service::workspace(&root).await?;
        data["skillSync"] = match sync_result {
            Ok(summary) => summary,
            Err(error) => json!({"error":error}),
        };
        {
            let mut db = crate::desktop_storage::connect_existing_at(&root).await?;
            data["jobs"] = json!(crate::agent_batch::list_all(&mut db).await?);
            data["breakdownJobs"] = json!(crate::agent_breakdown_job::list_views(&mut db).await?);
        }
        // 异常退出后仅修复孤立 running 状态；等待作者的任务仍保留原 requestId。
        let active: Vec<String> = host.jobs.lock().unwrap().keys().cloned().collect();
        let mut db = crate::desktop_storage::connect_existing_at(&root).await?;
        for s in data["conversations"].as_array_mut().unwrap() {
            if s["status"] == "running" && !active.iter().any(|id| s["id"] == *id) {
                s["status"] = json!("paused");
                s["error"] = json!("应用已中断，可继续上次任务");
                save_session(&mut db, s).await?;
            }
        }
        for job in data["jobs"].as_array_mut().into_iter().flatten() {
            let sid = job["sessionId"].as_str().unwrap_or("");
            let runner = format!("batch-runner:{}",job["id"].as_str().unwrap_or(""));
            if job["status"] == "running" && !active.iter().any(|id| id == sid || id == &runner) {
                job["status"] = json!("paused");
                for entry in job["queue"].as_array_mut().into_iter().flatten() {
                    if entry["status"] == "running" { entry["status"] = json!("paused"); }
                }
                job["revision"] = json!(job["revision"].as_u64().unwrap_or(0) + 1);
                job["updatedAt"] = json!(now());
                put(&mut db, NS, &format!("job:{}",job["id"].as_str().unwrap_or("")), job).await?;
            }
        }
        return Ok(data);
    }
    if action == "retry_memory" {
        let sid = str_field(&args, "sessionId")?;
        let mut db = crate::desktop_storage::connect_existing_at(&root).await?;
        let current = session(&mut db, sid).await?;
        if current["status"] != "completed" { return Err("请等当前创作任务结束后再重试记忆整理".into()); }
        start(app.clone(), json!({"sessionId":sid,"text":"重新整理当前作品记忆","modelCode":current["modelCode"],"memoryOnly":true})).await?;
        return Ok(json!(true));
    }
    if action == "run" {
        start(app.clone(), args).await?;
        return Ok(json!(true));
    }
    // 自动任务页：立即运行、重试投递；前端转来的通知（自动生文完成、榜单自动抓取）进宿主投递队列
    if action == "automation_run_now" {
        crate::agent_scheduler::run_now(&app, str_field(&args, "id")?)?;
        return Ok(json!(true));
    }
    if action == "automation_check_now" {
        return crate::agent_scheduler::run_checks(&app, true);
    }
    if action == "delivery_flush" {
        let sent = crate::agent_delivery_queue::flush(&app, args["channel"].as_str().unwrap_or(""))?;
        return Ok(json!({"sent":sent}));
    }
    if action == "automation_notify" {
        let queued = {
            let mut db = crate::desktop_storage::connect_existing_at(&root).await?;
            crate::agent_delivery_queue::enqueue(&mut db, args["channel"].as_str().unwrap_or(crate::agent_delivery_queue::WEIXIN),
                args["text"].as_str().unwrap_or(""), args["key"].as_str().unwrap_or(""), json!({"event":"frontend"})).await?
        };
        let Some(record) = queued else { return Ok(json!({"outcome":"skipped"})) };
        let sent = crate::agent_delivery_queue::flush(&app, record["channel"].as_str().unwrap_or("")).unwrap_or(0);
        return Ok(json!({"outcome":if sent > 0 { "sent" } else { "queued" },"id":record["id"]}));
    }
    if action == "breakdown_start" || action == "breakdown_control" {
        let pid = crate::agent_breakdown::project_id_arg(&args)?;
        let mut db = crate::desktop_storage::connect_existing_at(&root).await?;
        let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
        let view = if action == "breakdown_start" {
            crate::agent_breakdown_job::start(&mut tx, &pid, args["from"].as_i64().unwrap_or(1), args["to"].as_i64().unwrap_or(0),
                args["sessionId"].as_str().filter(|v| !v.is_empty()), false).await?
        } else {
            crate::agent_breakdown_job::control(&mut tx, &pid, str_field(&args, "control")?).await?
        };
        tx.commit().await.map_err(err)?;
        let control = args["control"].as_str().unwrap_or("");
        if control == "pause" || control == "cancel" {
            // 正在拆的进程停下，没写成的章节放回队列；通道随后按最新状态继续拆别的书
            interrupt_reference_lane(&app);
        }
        if view["status"] == "running" {
            schedule_reference_index(app.clone());
        }
        crate::agent_events::emit(&app, "agent-breakdown", &view);
        return Ok(view);
    }
    if action == "distillation_start" || action == "distillation_control" {
        let view=service::command(&root,&action,&args).await?;
        crate::agent_events::emit(&app,"agent-distillation",&view);
        let mut db=crate::desktop_storage::connect_existing_at(&root).await?;
        if let Some(sid)=view["sessionId"].as_str() {
            crate::agent_events::emit(&app,"agent-session",session(&mut db,sid).await?);
        }
        if view["status"]=="running" {schedule_distillation(app.clone());}
        return Ok(view);
    }
    if action == "index_rebuild" {
        let mut db = crate::desktop_storage::connect_existing_at(&root).await?;
        let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
        let chapter_ids: Vec<String> = args["chapterIds"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect())
            .unwrap_or_default();
        let queued = if chapter_ids.is_empty() {
            crate::agent_indexer::enqueue_book(&mut tx, str_field(&args, "bookId")?, args["onlyMissing"] == true).await?
        } else {
            // 写作台"重新整理本章"：只补这一章；已有章节卡的先作废再入队
            for cid in &chapter_ids {
                crate::agent_canon::remove_card(&mut tx, str_field(&args, "bookId")?, cid).await?;
            }
            crate::agent_indexer::enqueue_chapters(&mut tx, str_field(&args, "bookId")?, &chapter_ids).await?
        };
        tx.commit().await.map_err(err)?;
        if queued > 0 {
            schedule_index(app.clone(), Duration::from_millis(0));
        }
        return Ok(json!({"queued":queued}));
    }
    if let Some(result) = skill_action(&app, &root, &action, &args).await {
        return result;
    }
    if action == "batch_create" {
        let mut db = crate::desktop_storage::connect_existing_at(&root).await?;
        let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
        let job = crate::agent_batch::create(&mut tx, &args).await?;
        tx.commit().await.map_err(err)?;
        schedule_batch(app.clone(), str_field(&job, "id")?.to_owned());
        return Ok(job);
    }
    if action == "batch_control" {
        let job_id = str_field(&args, "id")?.to_owned();
        let control = str_field(&args, "control")?.to_owned();
        let mut db = crate::desktop_storage::connect_existing_at(&root).await?;
        let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
        let job = crate::agent_batch::control(&mut tx, &job_id, &control).await?;
        tx.commit().await.map_err(err)?;
        if control == "pause" || control == "cancel" {
            host.stop(str_field(&job, "sessionId")?);
        }
        if control == "resume" {
            schedule_batch(app.clone(), job_id);
        }
        crate::agent_events::emit(&app, "agent-batch", &job);
        return Ok(job);
    }
    if action == "apply_artifact" || action == "undo_artifact" {
        let artifact = service::apply_artifact_with(
            &root,
            str_field(&args, "id")?,
            action == "undo_artifact",
            args["revision"].as_u64(),
            args["overwrite"] == true,
        )
        .await?;
        crate::agent_events::emit(&app, "agent-book-changed", &artifact);
        return Ok(artifact);
    }
    let result = service::command(&root, &action, &args).await?;
    if action == "reference_commit" { schedule_reference_search(app.clone()); }
    if action == "adopt_book_plan" {
        crate::agent_events::emit(&app, "agent-book-changed", &result);
    }
    // 作者在记忆页改了规则、线索、疑点裁决或画像：写作台跟着刷新（删除规则不带作品编号，表示全部作品）
    if ["save_verdict", "override_thread", "save_rule", "delete_rule", "set_book_profile", "save_platform_profile", "style_lock", "confirm_samples", "apply_digest", "method_pack_import", "canon_mark", "add_note", "delete_note", "import_legacy_notes", "save_memory", "delete_memory"]
        .contains(&action.as_str())
    {
        let book_id = args["bookId"].as_str().filter(|v| !v.is_empty()).or(result["bookId"].as_str());
        crate::agent_events::emit(&app, "agent-canon-changed", json!({"bookId":book_id,"reason":action}));
    }
    Ok(result)
}

/// 技能脚本相关的动作：运行环境、依赖、导入前过目、安装、确认、导出。涉及下载与安装的放到阻塞线程。
async fn skill_action(_app: &DesktopAppHandle, root: &Path, action: &str, args: &Value) -> Option<Result<Value, String>> {
    use crate::agent_skill_runner as runner;
    if !action.starts_with("skill_") {
        return None;
    }
    let _files = crate::agent_skill_lifecycle::gate().lock().await;
    let root = root.to_path_buf();
    let skill = |id: &str| {
        let root = root.clone();
        let id = id.to_owned();
        async move {
            let mut db = crate::desktop_storage::connect_existing_at(&root).await?;
            get(&mut db, NS, &format!("skill:{id}")).await?.ok_or_else(|| "技能不存在".to_owned())
        }
    };
    let blocking = |task: Box<dyn FnOnce() -> Result<Value, String> + Send>| async move {
        tauri::async_runtime::spawn_blocking(task).await.map_err(|e| e.to_string())?
    };
    let result = match action {
        "skill_adopt" => async {
            let user_dir = crate::ensure_prompt_dir()?.join("skills");
            let mut db = crate::desktop_storage::connect_existing_at(&root).await?;
            let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
            let artifact = crate::agent_skill_authoring::adopt(&mut tx, &user_dir, str_field(args,"id")?,
                args["revision"].as_u64().ok_or("缺少技能候选版本")?).await?;
            tx.commit().await.map_err(err)?;
            Ok(artifact)
        }.await,
        "skill_read" => match skill(str_field(args, "id").unwrap_or("")).await {
            Ok(record) => crate::agent_skill_files::read_draft(&record),
            Err(e) => Err(e),
        },
        "skill_save" => async {
            let draft = serde_json::from_value::<crate::agent_skill_files::Draft>(args.clone()).map_err(err)?;
            let user_dir = crate::ensure_prompt_dir()?.join("skills");
            let mut db = crate::desktop_storage::connect_existing_at(&root).await?;
            let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
            let record = crate::agent_skill_files::save(&mut tx, &user_dir, &draft).await?;
            tx.commit().await.map_err(err)?;
            Ok(record)
        }.await,
        "skill_open_folder" => match skill(str_field(args, "id").unwrap_or("")).await {
            Ok(record) => (|| {
                let dir = runner::skill_dir(&record)?;
                if !dir.is_dir() { return Err("技能文件夹不存在".into()); }
                #[cfg(target_os = "macos")] let mut command = Command::new("open");
                #[cfg(target_os = "windows")] let mut command = Command::new("explorer");
                #[cfg(not(any(target_os = "macos",target_os = "windows")))] let mut command = Command::new("xdg-open");
                command.arg(dir).spawn().map_err(err)?;
                Ok(json!(true))
            })(),
            Err(e) => Err(e),
        },
        "skill_runtime" => {
            let root = root.clone();
            blocking(Box::new(move || Ok(runner::runtime(&root)))).await
        }
        "skill_runtime_install" => {
            let root = root.clone();
            blocking(Box::new(move || runner::install_portable(&root))).await
        }
        "skill_status" => match skill(str_field(args, "id").unwrap_or("")).await {
            Ok(record) => {
                let root = root.clone();
                blocking(Box::new(move || Ok(runner::status(&root, &record)))).await
            }
            Err(e) => Err(e),
        },
        "skill_deps_install" => match skill(str_field(args, "id").unwrap_or("")).await {
            Ok(record) => {
                let root = root.clone();
                blocking(Box::new(move || runner::install_deps(&root, &record))).await
            }
            Err(e) => Err(e),
        },
        // 安装前过目：传 path 是待导入的文件夹；传 id 是已安装技能（脚本改动后重新确认）
        "skill_inspect" => match args["path"].as_str().filter(|p| !p.is_empty()) {
            Some(path) => runner::inspect(Path::new(path)),
            None => match skill(str_field(args, "id").unwrap_or("")).await {
                Ok(record) => runner::skill_dir(&record).and_then(|dir| runner::inspect(&dir)),
                Err(e) => Err(e),
            },
        },
        "skill_install" => async {
            let user_dir = crate::ensure_prompt_dir()?.join("skills");
            let mut db = crate::desktop_storage::connect_existing_at(&root).await?;
            let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
            let record = crate::agent_skill_files::install(&mut tx, &user_dir,
                Path::new(str_field(args,"path")?), str_field(args,"revision")?).await?;
            tx.commit().await.map_err(err)?;
            Ok(record)
        }.await,
        "skill_approve" => {
            async {
                let hash = str_field(args, "scriptHash")?.to_owned();
                let id = {
                    let id = str_field(args, "id")?.to_owned();
                    let record = skill(&id).await?;
                    if crate::agent_skill_files::script_hash(&runner::skill_dir(&record)?)? != hash {
                        return Err("脚本在你查看之后又被改动过，请重新查看后再确认".to_owned());
                    }
                    id
                };
                let mut db = crate::desktop_storage::connect_existing_at(&root).await?;
                let mut record = get(&mut db, NS, &format!("skill:{id}")).await?.ok_or("技能不存在")?;
                record["approvedHash"] = json!(hash);
                record["approvedAt"] = json!(now());
                put(&mut db, NS, &format!("skill:{id}"), &record).await?;
                Ok(record)
            }
            .await
        }
        "skill_export" => match skill(str_field(args, "id").unwrap_or("")).await {
            Ok(record) => runner::export(&record, Path::new(str_field(args, "dest").unwrap_or(""))).map(|p| json!({"path":p.to_string_lossy()})),
            Err(e) => Err(e),
        },
        _ => return None,
    };
    Some(result)
}

/// 为会话登记一个执行槽；同一会话同时只能有一个进程。
fn claim_session(host: &AgentHost, sid: &str) -> Result<Arc<Job>, String> {
    let job = Arc::new(Job {
        pid: AtomicU32::new(0),
        cancelled: Arc::new(AtomicBool::new(false)),
    });
    let mut jobs = host.jobs.lock().unwrap();
    if host.maintenance.load(Ordering::SeqCst) {
        return Err("正在恢复数据".into());
    }
    if jobs.contains_key(sid) {
        return Err("该会话已有任务，请先停止或等待完成".into());
    }
    jobs.insert(sid.to_owned(), job.clone());
    Ok(job)
}

/// 调度器的无人值守一轮：占住会话、准备载荷、跑完返回会话最终状态。与作者亲手发起的一轮同一条路。
pub(crate) fn run_unattended(app: &DesktopAppHandle, root: &Path, sid: &str, args: Value) -> Result<Value, String> {
    let host = app.state::<AgentHost>();
    let job = claim_session(&host, sid)?;
    let payload = match crate::blocking::run(prepare_run(root, sid, &args)) {
        Ok(v) => v,
        Err(e) => {
            host.jobs.lock().unwrap().remove(sid);
            return Err(e);
        }
    };
    let session = run_prepared(app, root, sid, payload, job)?;
    schedule_index(app.clone(), Duration::from_secs(2));
    Ok(session)
}

/// 执行已准备好的 payload 并收尾：修正未返回完成状态的会话、释放执行槽；返回会话最终状态。
/// 普通对话与批量连载的每一步都走这里。
fn run_prepared(
    app: &DesktopAppHandle,
    root: &Path,
    sid: &str,
    payload: Value,
    job: Arc<Job>,
) -> Result<Value, String> {
    let run_id = str_field(&payload, "runId")?.to_owned();
    let executable = worker_command(app);
    let result = execute(app, root, sid, &run_id, payload, executable, job.clone());
    let outcome = crate::blocking::run(async {
        let mut db = crate::desktop_storage::connect_existing_at(root).await?;
        let mut s = session(&mut db, sid).await?;
        if s["status"] == "running" {
            let cancelled = job.cancelled.load(Ordering::SeqCst);
            s["status"] = json!(if cancelled { "paused" } else { "failed" });
            s["currentTool"] = Value::Null;
            s["currentToolProgress"] = Value::Null;
            s["error"] = json!(if cancelled {
                "任务已停止，已完成步骤已保存".to_string()
            } else {
                result.err().unwrap_or("后台未返回完成状态".into())
            });
            finish_reply(&mut s, &run_id);
            save_session(&mut db, &mut s).await?;
            crate::agent_events::emit(app, "agent-session", &s);
        }
        Ok::<_, String>(s)
    });
    app.state::<AgentHost>()
        .jobs
        .lock()
        .unwrap()
        .remove(sid);
    outcome
}

async fn start(app: DesktopAppHandle, args: Value) -> Result<(), String> {
    let sid = str_field(&args, "sessionId")?.to_owned();
    let root = crate::desktop_storage::root(&app)?;
    let job = match claim_session(&app.state::<AgentHost>(), &sid) {
        Ok(job) => job,
        Err(error)
            if error.contains("已有任务")
                && args["inputKind"] == "message"
                && args["text"].as_str().is_some_and(|text| !text.trim().is_empty()) =>
        {
            // 运行中到达的消息进收件箱，不拒绝：worker 在下一个落点读到它；本轮结束仍有剩余就自动续跑
            let session = service::command(
                &root,
                "queue_message",
                &json!({"id": sid, "text": args["text"], "references":args["references"], "selectedSkillId": args["selectedSkillId"]}),
            )
            .await?;
            crate::agent_events::emit(&app, "agent-session", &session);
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    let payload = match prepare_run(&root, &sid, &args).await {
        Ok(v) => v,
        Err(e) => {
            app.state::<AgentHost>().jobs.lock().unwrap().remove(&sid);
            return Err(e);
        }
    };
    if payload["deferred"] == true {
        // 只答了几张卡中的一张：界面刷新，等其余的卡
        app.state::<AgentHost>().jobs.lock().unwrap().remove(&sid);
        crate::agent_events::emit(&app, "agent-session", &payload["session"]);
        return Ok(());
    }
    let handle = app.clone();
    let cancelled = job.cancelled.clone();
    std::thread::spawn(move || {
        let outcome = run_prepared(&handle, &root, &sid, payload, job);
        // 批量连载的某一步在等作者回答时，作者的回答走的是普通对话；会话结束后交给批量模块决定是否继续。
        if let Ok(session) = &outcome {
            if session["batchJobId"].is_string() {
                crate::agent_batch::on_session_finished(&handle, &root, session);
            } else if session["status"] == "paused"
                && session["pendingInputs"].as_array().is_some_and(|v| !v.is_empty())
                && !cancelled.load(Ordering::SeqCst)
            {
                // 本轮收尾后收件箱里还有作者的话：自动续跑，不等作者再点"继续"
                let next = json!({"sessionId": sid, "inputKind": "continue", "text": "", "modelCode": session["modelCode"]});
                if let Err(error) = crate::blocking::run(start(handle.clone(), next)) {
                    eprintln!("[agent-host] 续跑收件箱消息失败：{error}");
                }
            }
        }
        // 会话结束后索引本轮改动过的章节，让下一轮对话能拿到最新的章节卡。
        schedule_index(handle, Duration::from_secs(2));
    });
    Ok(())
}

pub(crate) async fn prepare_run(root: &Path, sid: &str, args: &Value) -> Result<Value, String> {
    let mut db = crate::desktop_storage::connect_existing_at(root).await?;
    let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
    let mut s = session(&mut tx, sid).await?;
    if s["legacy"].is_string() {
        return Err("这是旧版妙笔的对话记录，只能查看；要接着聊请开新对话".into());
    }
    use crate::agent_prompt::RunInput;
    let input = crate::agent_prompt::run_input(&s, args)?;
    let action = if input == RunInput::Message { "start" } else { "resume" };
    if action == "resume"
        && !["paused", "choice", "failed"].contains(&s["status"].as_str().unwrap_or(""))
    {
        return Err("当前任务无需恢复".into());
    }
    let mut text = args["text"].as_str().unwrap_or("").to_owned();
    let mut selected_direction = Value::Null;
    if args["selection"].is_object() {
        if input != RunInput::Message { return Err("选择创作方向不能同时提交旧问题答案".into()); }
        let selection = &args["selection"];
        let aid = str_field(selection, "artifactId")?;
        let artifact = get(&mut tx, NS, &format!("artifact:{aid}")).await?.ok_or("选题成果不存在")?;
        if artifact["sessionId"] != sid || artifact["kind"] != "selection" {
            return Err("选题成果不属于当前对话".into());
        }
        let revision = selection["revision"].as_u64().ok_or("缺少选题版本")?;
        if artifact["revision"] != revision { return Err("选题成果已变化，请重新选择方向".into()); }
        let index = selection["index"].as_u64().ok_or("缺少选题序号")? as usize;
        let card = artifact["selectionCards"].as_array().and_then(|cards| cards.get(index)).ok_or("该方向已不存在")?;
        text = format!("我选择《{}》中的第 {} 个故事方案「{}」，请沿用这个故事完善开书方案。",
            artifact["title"].as_str().unwrap_or("选题卡"), index + 1, card["genre"].as_str().unwrap_or(""));
        selected_direction = json!({"artifactId":aid,"revision":revision,"index":index,"card":card});
    }
    // 回答待处理的引导卡：结构化答案校验后原样交还模型；旧界面只传文字与 choiceId 时自动升格。
    // 写进对话的作者消息是所选标题与填写内容，点选单选时就是选项标题本身。
    let mut answer = Value::Null;
    // 同一步里并行的工具可能各自弹出一张卡（如同时汇总两类榜单）：每张卡单独作答并记在卡上，
    // 全部答完才恢复运行，按中断编号一次交还（LangGraph 多个中断须带编号恢复）。
    let mut answers = serde_json::Map::new();
    if input == RunInput::Answer {
        let waiting_run = s["runId"].clone();
        let messages = s["messages"].as_array_mut().ok_or("会话消息格式无效")?;
        let pending: Vec<usize> = (0..messages.len()).filter(|&i| messages[i]["choice"]["status"] == "pending").collect();
        let target = if let Some(mid) = args["messageId"].as_str() {
            pending.iter().copied().find(|&i| messages[i]["id"] == mid).ok_or("这个问题已经结束，请根据当前对话继续")?
        } else {
            *pending.first().ok_or("当前没有待回答的问题")?
        };
        {
            let prompt = crate::agent_prompt::from_message(&messages[target]);
            let raw = if args["answer"].is_object() {
                args["answer"].clone()
            } else {
                crate::agent_prompt::legacy_answer(&prompt, args["choiceId"].as_str(), text.trim())
            };
            let (normalized, display) = crate::agent_prompt::check_answer(&prompt, &raw)?;
            let card = &mut messages[target];
            card["choice"]["status"] = json!("selected");
            card["choice"]["selectedId"] = normalized["selectedIds"].get(0).cloned().unwrap_or(Value::Null);
            card["choice"]["selectedIds"] = normalized["selectedIds"].clone();
            card["choice"]["answerText"] = json!(display);
            card["choice"]["delivered"] = json!(false);
            card["answer"] = normalized.clone();
            if pending.len() > 1 {
                // 还有卡没答：先记下这一张，不启动运行
                s["promptDraft"] = Value::Null;
                save_session(&mut tx, &mut s).await?;
                tx.commit().await.map_err(err)?;
                return Ok(json!({"deferred":true,"session":s}));
            }
            // 最后一张答完：这一轮所有卡的回答合成一条作者消息，按中断编号交还
            let mut lines = Vec::new();
            for m in messages.iter_mut().filter(|m| m["kind"] == "choice" && m["runId"] == waiting_run) {
                if m["choice"]["answerText"].is_string() && m["choice"]["delivered"] != true {
                    lines.push(m["choice"]["answerText"].as_str().unwrap_or("").to_owned());
                    if let Some(request) = m["choice"]["requestId"].as_str() {
                        answers.insert(request.to_owned(), m["answer"].clone());
                    }
                    m["choice"]["delivered"] = json!(true);
                }
            }
            answer = normalized;
            text = if lines.is_empty() { display } else { lines.join("\n") };
        }
    }
    let text = text.as_str();
    if input == RunInput::Continue && s["messages"].as_array().into_iter().flatten().any(|m| m["choice"]["status"] == "pending") {
        return Err("请提交卡片答案，或在输入框直接发送新的要求".into());
    }
    if input == RunInput::Continue && !text.trim().is_empty() {
        if !s["pendingInputs"].is_array() {
            s["pendingInputs"] = json!([]);
        }
        s["pendingInputs"].as_array_mut().unwrap().push(json!(text));
    }
    if action == "start" && (text.trim().is_empty() || text.len() > 160_000) {
        return Err("请输入有效创作指令".into());
    }
    if s["status"] == "choice" && text.trim().is_empty() {
        return Err("请先回答待处理的问题".into());
    }
    // 作者在输入框指定的模型只决定规划角色；写作跟随规划模型（正文质量一致）；
    // 评审与索引调用多、要结构化输出，未设置时先跟便宜的写作助手，再没有才跟规划模型。
    let model = service::model_config(
        &mut tx,
        args["modelCode"].as_str().unwrap_or(""),
        "agent_planner",
    )
    .await?;
    let mut models = json!({"planner": model.clone()});
    models["writer"] = service::role_model(&mut tx, "agent_writer", &model).await?;
    for (role, key) in [("agent_critic", "critic"), ("agent_indexer", "indexer")] {
        models[key] = service::support_model(&mut tx, role, &model).await?;
    }
    // worker 只拿网关引用：没有 Key、供应商地址和额外参数
    let gateway = crate::model_gateway::endpoint(root)?;
    let model_ref = crate::model_gateway::worker_model(&gateway, &model);
    let mut model_refs = json!({});
    for (key, value) in models.as_object().into_iter().flatten() {
        model_refs[key] = crate::model_gateway::worker_model(&gateway, value);
    }
    let project = service::session_project(&mut tx, &s).await?;
    // 任务是作者交代的一件事（见 agent_task）：同一任务里的补充、答卡、"继续"沿用作者要求、写入目标和技能选择。
    // 批量的每一步、自动任务各是独立的一件事；对话里换了一件事由模型用 begin_new_task 声明，不按措辞猜。
    // 记忆整理是维护操作，不属于任何任务。
    let maintenance = args["memoryOnly"] == true;
    let new_task = action == "start"
        && !maintenance
        && (args["batch"] == true
            || args["automation"].is_object()
            || s["taskId"].as_str().is_none_or(str::is_empty));
    let mut delivered = text.to_owned();
    if action == "start" {
        if new_task {
            // 换了一件事：上一件事选定的写入目标不再授权
            s["targetBookId"] = Value::Null;
            s["selectedSkillId"] = Value::Null;
        }
        // 上一轮停下或失败时还没送达的补充：按原顺序放在本条前面交给模型，不静默丢掉
        let residual: Vec<String> = s["pendingInputs"].as_array().into_iter().flatten()
            .filter_map(|v| v.as_str().map(str::to_owned)).collect();
        if !residual.is_empty() && args["batch"] != true && !maintenance {
            delivered = format!("此前补充、尚未处理的话（按发送顺序）：\n{}\n\n本条消息：\n{text}", residual.join("\n\n"));
        }
        if !maintenance {
            s["pendingInputs"] = json!([]);
        }
        crate::agent_prompt::supersede_questions(&mut s);
        if args["batch"] != true && !maintenance {
            crate::agent_batch::suspend_for_message(&mut tx, &mut s).await?;
        }
    } else if s["scopeVersion"].is_null() {
        // 仅兼容升级前已存在的待恢复任务，不把旧项目规则扩散到新消息。
        s["targetBookId"] = project["bookId"].clone();
    }
    s["scopeVersion"] = json!(2);
    if !s["draftReferences"].is_array() {s["draftReferences"]=crate::agent_prompt::draft_references(&s);}
    if args["references"].is_array() {
        if action=="start" {settle_references(&mut s);}
        service::validate_references(&mut tx, &args["references"]).await?;
        s["references"] = args["references"].clone();
    }
    s["contextBookId"] = json!(crate::agent_memory::context_book(&mut tx, &s).await?);
    // taskInstruction 记发起这件事的那条消息；之后的补充与答卡由 agent_task::instruction 按任务累积，不再逐条覆盖
    if new_task { s["taskInstruction"] = json!(text); }
    if args["batch"] == true {
        s["batchStep"] = args["batchStep"].clone();
        s["activeThreadId"] = args["thread"].clone();
    } else if action == "start" {
        s["activeThreadId"] = json!(sid);
    }
    // 作者选定的技能在这件事里一直有效：本条没选就沿用，选了别的就换，新任务从头算。
    let mut chosen_now = false;
    if let Some(selected) = args.get("selectedSkillId") {
        if !selected.is_null() && !selected.is_string() { return Err("技能编号格式无效".into()); }
        if selected.as_str().is_some_and(|value| !value.is_empty()) {
            s["selectedSkillId"] = selected.clone();
            chosen_now = true;
        }
    }
    // 消息上只标作者这一条亲手选的技能；答卡沿用当时生效的技能，与以前一致
    let selected_skill = match s["selectedSkillId"].as_str().filter(|value| !value.is_empty()) {
        Some(skill_id) if chosen_now || action == "resume" => {
            let record = get(&mut tx,NS,&format!("skill:{skill_id}")).await?;
            json!({"id":skill_id,"name":record.as_ref().and_then(|skill|skill["name"].as_str()).unwrap_or(skill_id)})
        }
        _ => Value::Null,
    };
    s["reviewablePlans"] = json!(list(&mut tx, NS, "artifact:").await?.into_iter()
        .filter(|a| a["sessionId"] == sid && a["kind"] == "book_plan" && a["applied"] != true && a["superseded"] != true)
        .map(|a| json!({"id":a["id"],"revision":a["revision"],"title":a["bookPlan"]["workingTitle"],"fingerprint":crate::agent_book_adoption::fingerprint(&a)})).collect::<Vec<_>>());
    let run_id = id();
    if new_task {
        s["taskId"] = json!(run_id);
        s["taskRunIds"] = json!([run_id]);
    } else if !maintenance {
        // 接着原来的事做；升级前开始的任务从这一轮起记轮次
        if !s["taskRunIds"].is_array() {
            s["taskRunIds"] = json!([]);
        }
        s["taskRunIds"].as_array_mut().unwrap().push(json!(run_id));
    }
    s["runId"] = json!(run_id);
    s["status"] = json!("running");
    s["error"] = Value::Null;
    // 上一轮停下或失败时可能还挂着工具名；新一轮从"正在生成"开始
    s["currentTool"] = Value::Null;
    s["currentToolProgress"] = Value::Null;
    s["interactionWarning"] = Value::Null;
    if args["memoryOnly"] != true && selected_direction.is_null() { s["draft"] = json!(""); }
    s["promptDraft"] = Value::Null;
    if let Some(messages) = s["messages"].as_array_mut() {
        if !text.is_empty() {
            let mut user = message("user", text);
            // 消息只记引用了什么；资料正文已存在会话引用（旧版）或资料库里，不再每条消息复制一份
            user["references"] = json!(args["references"].as_array().into_iter().flatten().map(|r| {
                let mut r = r.clone();
                if let Some(map) = r.as_object_mut() { map.remove("content"); }
                r
            }).collect::<Vec<_>>());
            user["runId"] = json!(run_id);
            user["selectedSkill"] = selected_skill.clone();
            if !selected_direction.is_null() { user["selectedDirection"] = selected_direction.clone(); }
            if args["batch"] == true {
                user["source"] = json!("batch");
            } else if args["source"] == "breakdown" {
                // 拆书拆完后宿主替作者接着发起的一轮，界面上和作者亲手发的区分开
                user["source"] = json!("breakdown");
            } else if args["automation"].is_object() {
                // 调度器替作者发起的无人值守一轮
                user["source"] = json!("automation");
            }
            messages.push(user);
        }
        messages.push(json!({"id":format!("{run_id}:reply"),"runId":run_id,"role":"assistant","text":"","createdAt":now()}));
    }
    if s["title"] == "新对话" {
        s["title"] = json!(text.chars().take(24).collect::<String>());
    }
    s["modelCode"] = args["modelCode"].clone();
    if input==RunInput::Message && args["batch"]!=true && args["memoryOnly"]!=true && args["source"]!="breakdown" && !args["automation"].is_object() {
        crate::agent_prompt::consume_draft_references(&mut s,&args["references"]);
    }
    save_session(&mut tx, &mut s).await?;
    let mut skills = list(&mut tx, NS, "skill:")
        .await?
        .into_iter()
        .filter(|s| s["enabled"] == true && !s["deletedAt"].is_number())
        .collect::<Vec<_>>();
    // 参考文件只在本轮载入虚拟文件系统，按需读取；数据库不复制其正文。
    for skill in &mut skills {
        if skill["filePath"].is_string() {
            let package = crate::agent_skill_files::read_draft(skill)?;
            skill["files"] = json!(package["files"].as_array().into_iter().flatten()
                .filter(|f| f["path"].as_str().is_some_and(|p| p.starts_with("references/")) && f["content"].is_string())
                .cloned().collect::<Vec<_>>());
        }
    }
    let mut references = service::expand_references(&mut tx, &s["references"]).await?;
    // 上传的资料只在第一次引用的那一轮给正文；之后的轮次模型历史里已经有了，只留标题与读取方式
    let injected: BTreeSet<String> = s["injectedMaterials"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_owned).collect();
    let mut newly = Vec::new();
    for r in references.as_array_mut().into_iter().flatten() {
        let Some(material_id) = r["materialId"].as_str().map(str::to_owned) else { continue };
        if injected.contains(&material_id) {
            if let Some(map) = r.as_object_mut() {
                map.remove("content");
                map.remove("truncated");
            }
            r["note"] = json!("这份资料前面的对话里已经提供过；需要原文时用 read_material 读取");
        } else if r["missing"].is_null() {
            newly.push(material_id);
        }
    }
    if action == "start" && !newly.is_empty() {
        let mut all: Vec<String> = injected.into_iter().collect();
        all.extend(newly);
        s["injectedMaterials"] = json!(all);
        save_session(&mut tx, &mut s).await?;
    }
    let payload = json!({"sessionId":sid,"runId":run_id,"action":action,"memoryOnly":args["memoryOnly"] == true,"text":delivered,"answer":answer,"answers":answers,"model":model_ref,"models":model_refs,"channel":crate::agent_channel::of_session(&s),
        "project":project,"targetBookId":s["targetBookId"],"references":references,"skills":skills,"selectedSkillId":s["selectedSkillId"],"selectedDirection":selected_direction,"thread":s["activeThreadId"].as_str().unwrap_or(sid),"memoryUsage":s["memoryUsage"],"auxUsage":s["auxUsage"],"usage":s["usage"],
        "automation":args["automation"]});
    tx.commit().await.map_err(err)?;
    Ok(payload)
}

/// 技能目录：打包资源里的内置技能、开发模式下的源码目录、提示词目录下的作者技能。
fn skill_dirs(app: &DesktopAppHandle) -> (Vec<PathBuf>, Option<PathBuf>) {
    let mut builtin = Vec::new();
    #[cfg(debug_assertions)]
    builtin.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/skills"));
    if let Ok(dir) = app.path().resource_dir() {
        builtin.push(dir.join("skills"));
    }
    let user = crate::prompt_dir_path().ok().map(|dir| dir.join("skills"));
    (builtin, user)
}

fn worker_command(app: &DesktopAppHandle) -> Result<Command, String> {
    // 开发版优先运行当前源码，避免资源目录中上一次冻结的后台遮蔽本轮修改。
    #[cfg(debug_assertions)]
    {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../agent-runtime");
        let python = root.join(if cfg!(windows) {
            ".venv/Scripts/python.exe"
        } else {
            ".venv/bin/python"
        });
        if python.is_file() {
            let mut command = Command::new(python);
            command.arg(root.join("worker.py"));
            return Ok(command);
        }
    }
    let executable = if cfg!(windows) {
        "ew-agent-runtime.exe"
    } else {
        "ew-agent-runtime"
    };
    let packaged = app
        .path()
        .resource_dir()
        .map_err(err)?
        .join("agent")
        .join(if cfg!(all(target_os = "macos", target_arch = "aarch64")) { "aarch64-apple-darwin" }
            else if cfg!(target_os = "macos") { "x86_64-apple-darwin" }
            else if cfg!(target_os = "windows") { "x86_64-pc-windows-msvc" }
            else { "x86_64-unknown-linux-gnu" })
        .join(executable);
    if packaged.is_file() {
        return Ok(Command::new(packaged));
    }
    Err("Agent 运行环境未准备，请运行 pnpm setup:agent 或重新安装完整桌面包".into())
}

fn execute(
    app: &DesktopAppHandle,
    root: &Path,
    sid: &str,
    run_id: &str,
    payload: Value,
    executable: Result<Command, String>,
    job: Arc<Job>,
) -> Result<(), String> {
    use crate::agent_worker_pool as pool;
    use std::cell::{Cell, RefCell};
    // 进程按会话常驻：池里有活的就复用，没有才按命令起一个
    let mut resident = pool::acquire(sid, &pool::log_dir(root), move || executable)?;
    job.pid.store(resident.pid, Ordering::SeqCst);
    if job.cancelled.load(Ordering::SeqCst) {
        stop_job(&job);
    }
    let reply = RefCell::new(String::new());
    let last_save = Cell::new(Instant::now());
    let turn = pool::run_turn(
        &mut resident,
        &payload,
        |event| {
            if job.cancelled.load(Ordering::SeqCst) {
                return Err("任务已停止".into());
            }
            let tool_name = event["params"]["name"].as_str().unwrap_or("");
            let v = if event["method"] == "tool" && crate::agent_rank_bridge::handles(tool_name) {
                crate::agent_rank_bridge::execute(app, root, sid, run_id,
                    str_field(&event["params"], "name")?, &event["params"]["args"], &job.cancelled)?
            } else if event["method"] == "tool" && crate::agent_webview_bridge::handles(tool_name) {
                // 灵感、码字统计、发文件发图：只能在网页层做，经网页桥交给主窗口
                crate::agent_webview_bridge::execute(app, root, sid, run_id,
                    str_field(&event["params"], "name")?, &event["params"]["args"], &job.cancelled)?
            } else if event["method"] == "tool" {
                crate::blocking::run(service::tool_with_cancellation(
                    root, sid, run_id, str_field(&event["params"], "name")?,
                    &event["params"]["args"], job.cancelled.clone(),
                ))?
            } else {
                crate::blocking::run(rpc(
                    root,
                    sid,
                    run_id,
                    str_field(&event, "method")?,
                    &event["params"],
                ))?
            };
            // 正文、目录、作品信息有变动就通知界面刷新（含改名与进回收站）
            if v["saved"] == true || v["created"] == true || v["updated"] == true || v["deleted"] == true {
                crate::agent_events::emit(app, "agent-book-changed", &v);
            }
            // Agent 发起的拆书立刻开始，不等本轮对话结束；进度卡随事件出现在对话里
            if tool_name == "prepare_distillation" && v["status"] == "awaiting_scope" {
                crate::agent_events::emit(app, "agent-distillation", &v);
            }
            if tool_name == "start_breakdown" && v["status"] == "started" {
                schedule_reference_index(app.clone());
                crate::agent_events::emit(app, "agent-breakdown", &v["job"]);
            }
            if ["update_thread", "record_suspect_verdict", "save_rule"].contains(&tool_name) && v["toolError"].is_null() {
                let book_id = event["params"]["args"]["book_id"].as_str().filter(|b| !b.is_empty());
                crate::agent_events::emit(app, "agent-canon-changed", json!({"bookId":book_id,"reason":tool_name}));
            }
            if [
                "save_artifact",
                "save_material",
                "apply_creation_artifact",
                "save_chapter",
                "prepare_book_plan",
                "prepare_skill",
                "save_selection_cards",
                "save_comparison",
                "save_breakdown_report",
                "plan_short_story",
                "delivery_begin",
                "delivery_progress",
                "delivery_restore",
                "artifact_replace",
                // 取名候选、作品简介、封面方案也是成果：不挂进对话作者就找不到它们
                "save_name_candidates",
                "save_book_intro",
                "request_cover",
            ]
            .contains(&tool_name)
                && (v["id"].is_string() || v["artifact_id"].is_string())
            {
                let artifact = if v["id"].is_string() { v.clone() } else { json!({"id":v["artifact_id"]}) };
                crate::blocking::run(persist_event(
                    app,
                    root,
                    sid,
                    run_id,
                    &json!({"event":"artifact","artifact":artifact}),
                    &reply.borrow(),
                ))?;
            }
            Ok(v)
        },
        |event| {
            if event["event"] == "delta" {
                reply.borrow_mut().push_str(event["text"].as_str().unwrap_or(""));
                crate::agent_events::emit(
                    app,
                    "agent-delta",
                    json!({"sessionId":sid,"runId":run_id,"text":reply.borrow().as_str()}),
                );
                if last_save.get().elapsed() < Duration::from_millis(500) {
                    return Ok(());
                }
            }
            if event["event"] == "draft" {
                crate::agent_events::emit(
                    app,
                    "agent-draft",
                    json!({"sessionId":sid,"runId":run_id,"artifactId":event["artifactId"],"text":event["text"]}),
                );
            }
            crate::blocking::run(persist_event(app, root, sid, run_id, event, &reply.borrow()))?;
            last_save.set(Instant::now());
            Ok(())
        },
    );
    // 最后一段流式文本即使停止也保存；不能把取消误报为完成。
    let flushed = crate::blocking::run(persist_event(
        app,
        root,
        sid,
        run_id,
        &json!({"event":"flush"}),
        &reply.borrow(),
    ));
    match turn {
        Ok(pool::TurnEnd::Finished) => {
            pool::release(resident);
            flushed
        }
        Ok(pool::TurnEnd::Crashed(tail)) => {
            pool::discard(resident);
            flushed?;
            if job.cancelled.load(Ordering::SeqCst) {
                return Err("任务已停止".into());
            }
            let last = tail.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").to_owned();
            Err(if last.is_empty() {
                "Agent 进程中断，可从已保存的检查点继续".to_string()
            } else {
                format!("Agent 进程中断，可从已保存的检查点继续。后台最后的输出：{last}")
            })
        }
        Err(error) => {
            pool::discard(resident);
            flushed?;
            Err(error)
        }
    }
}

async fn persist_event(
    app: &DesktopAppHandle,
    root: &Path,
    sid: &str,
    run: &str,
    event: &Value,
    reply: &str,
) -> Result<(), String> {
    let s = apply_event(root, sid, run, event, reply).await?;
    crate::agent_events::emit(app, "agent-session", s);
    Ok(())
}

/// 两次累计用量之差；旧值缺失或更大（线程被重建）时按本次累计计。
fn usage_delta(now: &Value, before: &Value) -> Value {
    let mut out = json!({});
    for key in ["input_tokens", "output_tokens", "total_tokens"] {
        let (n, b) = (now[key].as_u64().unwrap_or(0), before[key].as_u64().unwrap_or(0));
        out[key] = json!(if b <= n { n - b } else { n });
    }
    out
}

pub(crate) async fn apply_event(
    root: &Path,
    sid: &str,
    run: &str,
    event: &Value,
    reply: &str,
) -> Result<Value, String> {
    let mut db = crate::desktop_storage::connect_existing_at(root).await?;
    let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
    let mut s = session(&mut tx, sid).await?;
    if s["runId"] != run {
        return Ok(s);
    }
    let messages = s["messages"].as_array_mut().ok_or("会话消息格式无效")?;
    if let Some(m) = messages
        .iter_mut()
        .find(|m| m["id"] == format!("{run}:reply"))
    {
        // 宿主内部的工具状态事件不携带流式回复，不能覆盖已有文字
        if !matches!(event["event"].as_str(), Some("tool_progress" | "tool_started" | "tool_done")) {
            m["text"] = json!(reply);
        }
    }
    match event["event"].as_str().unwrap_or("") {
        "tool" => {
            s["currentTool"] = event["name"].clone();
            s["currentToolProgress"] = Value::Null;
        }
        // 宿主在工具真正开始执行时发出；模型一次发出多个工具调用时，界面也显示实际在跑的那个。
        "tool_started" => {
            if s["status"] == "running" {
                s["currentTool"] = event["name"].clone();
                s["currentToolProgress"] = Value::Null;
            }
        }
        // 工具执行完：界面不再显示"正在执行"，模型接着写回复时显示正在生成
        "tool_done" => {
            if s["currentTool"] == event["name"] {
                s["currentTool"] = Value::Null;
                s["currentToolProgress"] = Value::Null;
            }
        }
        // 进度只属于当前工具；工具已结束或换成别的工具后到达的进度丢弃。
        "tool_progress" => {
            if s["status"] == "running" && s["currentTool"] == event["name"] {
                s["currentToolProgress"] = json!({"completed":event["completed"],"total":event["total"]});
            }
        }
        "waiting" => {
            // 并排对比卡没写选项时，用成果标题生成“采用某成果”
            let mut titles = std::collections::BTreeMap::new();
            for aid in event["prompt"]["artifactIds"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                if let Some(a) = get(&mut tx, NS, &format!("artifact:{aid}")).await? {
                    titles.insert(aid.to_owned(), a["title"].as_str().unwrap_or("").to_owned());
                }
            }
            let prompt = crate::agent_prompt::normalize(event, &titles);
            let options = prompt["options"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|o| json!({"id":o["id"],"title":o["title"],"description":o["description"].as_str().unwrap_or("")}))
                .collect::<Vec<_>>();
            // 每张卡自己的默认选中项：同时有几张卡待答时互不串
            let mut choice = json!({"status":"pending","requestId":event["requestId"],"kind":prompt["kind"],"options":options,
                "defaultId":crate::agent_prompt::default_choice(&prompt)});
            // 工具发起的联网确认带宿主生成的编号，宿主据此读取作者点选的是哪一项
            if event["confirmation"].is_string() {
                choice["confirmation"] = event["confirmation"].clone();
                let token = event["confirmation"].as_str().unwrap_or("");
                if let Some(record) = get(&mut tx, OP_NS, &format!("skill-consent-token:{sid}:{token}")).await? {
                    choice["skillReview"] = json!({"id":record["skillId"],"hash":record["hash"],"action":record["action"]});
                }
            }
            // 同一轮可能先后弹出几张卡（并行的工具各弹一张）：第二张起编号加序号，界面与作答都按编号区分
            let same_run = messages.iter().filter(|m| m["kind"] == "choice" && m["runId"] == run).count();
            let card_id = if same_run == 0 { format!("{run}:choice") } else { format!("{run}:choice:{}", same_run + 1) };
            messages.push(json!({"id":card_id,"runId":run,"role":"assistant","text":prompt["question"],"kind":"choice",
                "choice":choice,"prompt":prompt}));
            s["status"] = json!("choice");
            s["choiceId"] = crate::agent_prompt::default_choice(&prompt);
            s["promptDraft"] = Value::Null;
        }
        "interaction_warning" => {
            s["interactionWarning"] = event["message"].clone();
        }
        // Internal diagnostics only; normal context maintenance is not a user warning.
        "compaction" => {
            if !event["summaryId"].is_string() || event["summaryId"] != s["compaction"]["summaryId"] {
                let count = s["compaction"]["count"].as_i64().unwrap_or(0) + 1;
                s["compaction"] = json!({"count":count,"at":now(),"before":event["before"],"after":event["after"],"summaryId":event["summaryId"]});
            }
        }
        "artifact" => {
            let aid = event["artifact"]["id"].as_str().unwrap_or("");
            // 开书草案：对话里只留最新一份，之前被接替的草案卡片撤掉（成果记录仍在，只是不再供采用）
            let mut superseded = Vec::new();
            for m in messages.iter() {
                let Some(other) = m["artifactId"].as_str().filter(|id| *id != aid) else { continue };
                if let Some(a) = get(&mut tx, NS, &format!("artifact:{other}")).await? {
                    if a["kind"] == "book_plan" && a["superseded"] == true {
                        superseded.push(other.to_owned());
                    }
                }
            }
            messages.retain(|m| !m["artifactId"].as_str().is_some_and(|id| superseded.iter().any(|s| s == id)));
            if !messages.iter().any(|m| m["artifactId"] == aid) {
                // 正文类成果（章节、短篇、试写）在对话里直接显示正文，消息只是指向它的记录；其他成果给一条可展开的链接
                let prose = get(&mut tx, NS, &format!("artifact:{aid}")).await?.is_some_and(|a| crate::agent_quality::is_prose(&a) || a["kind"] == "draft");
                messages.push(json!({"id":format!("{aid}:link"),"runId":run,"role":"assistant","text":if prose { "" } else { "成果已保存，可展开查看。" },"kind":"result","artifactId":aid}));
            }
        }
        "draft" => {
            // 写作模型的流式片段：只更新本会话尚在生成中的章节候选，正式内容由 draft_finish 一次落定
            if let (Some(aid), Some(text)) = (event["artifactId"].as_str(), event["text"].as_str()) {
                let key = format!("artifact:{aid}");
                if let Some(mut artifact) = get(&mut tx, NS, &key).await? {
                    if artifact["sessionId"] == sid && artifact["partial"] == true {
                        artifact["content"] = json!(text);
                        artifact["generationRunId"] = json!(run);
                        artifact["draftUpdatedAt"] = json!(now());
                        put(&mut tx, NS, &key, &artifact).await?;
                        if !messages.iter().any(|m| m["artifactId"] == aid) {
                            // 流式正文直接显示在对话里，消息本身不带文字
                            messages.push(json!({"id":format!("{aid}:link"),"runId":run,"role":"assistant","text":"","kind":"result","artifactId":aid}));
                        }
                    }
                }
            }
        }
        "done" => {
            s["taskOutcome"] = crate::agent_delivery::task_outcome(&mut tx,&s).await?;
            s["status"] = json!(if event["waiting"] == true {
                "choice"
            } else if s["pendingInputs"].as_array().is_some_and(|v| !v.is_empty()) {
                "paused"
            } else {
                "completed"
            });
            if s["status"] == "completed" {
                settle_references(&mut s);
            }
            // worker 报的是本线程累计用量；普通对话同一会话同一线程，本轮用量取差值，批量每步是独立线程直接用
            let run_usage = if s["batchJobId"].is_string() {
                event["usage"].clone()
            } else {
                usage_delta(&event["usage"], &s["usage"])
            };
            if let Some(m) = s["messages"].as_array_mut().and_then(|ms| ms.iter_mut().find(|m| m["id"] == format!("{run}:reply"))) {
                m["usage"] = run_usage;
            }
            s["usage"] = event["usage"].clone();
            if event["auxUsage"].is_object() { s["auxUsage"] = event["auxUsage"].clone(); }
            if event["memoryUsage"].is_object() {
                s["memoryUsage"] = event["memoryUsage"].clone();
            }
            s["currentTool"] = Value::Null;
            s["currentToolProgress"] = Value::Null;
        }
        "failed" => {
            s["taskOutcome"] = crate::agent_delivery::task_outcome(&mut tx,&s).await?;
            s["status"] = json!("failed");
            s["error"] = event["message"].clone();
            s["currentTool"] = Value::Null;
            s["currentToolProgress"] = Value::Null;
        }
        _ => {}
    }
    if matches!(event["event"].as_str(), Some("waiting" | "done")) {
        for job in list(&mut tx,NS,"distillation-job:").await? {
            crate::agent_distillation::reconcile_scope_session(&mut s,&job);
        }
    }
    if matches!(event["event"].as_str(), Some("waiting" | "done" | "failed")) {
        finish_reply(&mut s, run);
    }
    save_session(&mut tx, &mut s).await?;
    tx.commit().await.map_err(err)?;
    Ok(s)
}

/// 一轮任务做完，@ 带进来的资料、参考书、选段这类附件从会话引用里撤下：作者的直觉是"发完就走"，
/// 消息上的快照照旧保留。作品引用总是留着（选择作品、妙笔、微信当前作品都靠它），标了 pinned 的也留（妙笔的当前章锚点）。
/// 撤下的书与参考书记进 `readSources`：这条会话读过它们，后面几轮模型仍可只读它们的章节（见 `allowed_book / allowed_source`）。
/// 提问中途（choice / paused）不撤：作者答完模型还要用到。
pub(crate) fn settle_references(s: &mut Value) {
    let Some(refs) = s["references"].as_array() else { return };
    let sent: Vec<&Value> = s["messages"].as_array().into_iter().flatten()
        .filter(|m|m["role"]=="user" && m["runId"]==s["runId"]).collect();
    let (kept, dropped): (Vec<Value>, Vec<Value>) = refs.iter().cloned().partition(|r| r["kind"] == "book" || r["pinned"] == true
        || !sent.is_empty() && !sent.iter().any(|m|m["references"].as_array().into_iter().flatten().any(|used|used["id"]==r["id"])));
    if dropped.is_empty() {
        return;
    }
    let mut sources: Vec<Value> = s["readSources"].as_array().cloned().unwrap_or_default();
    for r in &dropped {
        if r["kind"] == "material" {
            continue;
        }
        if let Some(bid) = r["bookId"].as_str().filter(|v| !v.is_empty()) {
            // 还留着作品引用的书不用记：引用本身就放行
            let still_referenced = kept.iter().any(|k| k["kind"] == "book" && k["bookId"] == bid);
            if !still_referenced && !sources.iter().any(|v| v == bid) {
                sources.push(json!(bid));
            }
        }
    }
    const READ_SOURCES_MAX: usize = 50;
    if sources.len() > READ_SOURCES_MAX {
        sources.drain(..sources.len() - READ_SOURCES_MAX);
    }
    s["readSources"] = json!(sources);
    s["references"] = json!(kept);
}

pub(crate) async fn rpc(
    root: &Path,
    sid: &str,
    run: &str,
    method: &str,
    args: &Value,
) -> Result<Value, String> {
    if method == "memory_record" {
        return service::tool(root, sid, run, "record_book_memory", args).await;
    }
    if method == "tool" {
        return service::tool(root, sid, run, str_field(args, "name")?, &args["args"]).await;
    }
    let mut db = crate::desktop_storage::connect_existing_at(root).await?;
    let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
    let namespace = args["namespace"].as_str().unwrap_or("");
    // 检查点按线程隔离：主线程与会话同名时沿用旧键格式，升级前的会话无需迁移；
    // 批量步骤等子线程挂在会话之下，键始终以会话编号开头，进程读不到其他会话的检查点。
    let thread = args["thread"]
        .as_str()
        .filter(|t| !t.is_empty())
        .unwrap_or(sid);
    let scope = if thread == sid {
        sid.to_owned()
    } else {
        format!("{sid}|{thread}")
    };
    let prefix = format!("{scope}:{namespace}:");
    let key = format!("{prefix}{}", args["id"].as_str().unwrap_or(""));
    let value = match method {
        "memory_context" | "memory_sources" | "memory_warning" | "memory_progress" | "memory_indexed" => {
            let mut s = session(&mut tx, sid).await?;
            if s["runId"] != run || s["status"] != "running" {
                return Err("记忆任务已停止或失效".into());
            }
            if method == "memory_context" {
                let mut context = crate::agent_memory::context(&mut tx, &s, None).await?;
                // 逐条给出：对话里已经看得到的那几条由运行时去掉，只补回被历史整理挤掉的原话
                context["currentTaskInstruction"]=json!(crate::agent_task::author_texts(&s));
                context["selectedSkillId"]=s["selectedSkillId"].clone();
                crate::agent_memory::capture_context(&mut tx, &s, run, &mut context).await?;
                // 每次模型调用附带固定层（规则、画像、核心设定、主角状态）与前几章概要；不装正文，预算很小。
                if let Some(bid) = context["book"]["id"].as_str().map(str::to_owned) {
                    let model=service::model_config(&mut tx,s["modelCode"].as_str().unwrap_or(""),"agent_planner").await.ok();
                    let pack = crate::agent_context::assemble(
                        &mut tx,
                        Some(&s),
                        &crate::agent_context::ContextRequest {
                            book_id: &bid,
                            purpose: "chat",
                            chapter_id: None,
                            ruleset_id: None,
                            query_hint: None,
                            characters_hint: Vec::new(),
                            model_max_context: model.as_ref().and_then(|m|m["maxContext"].as_u64()).unwrap_or(32000) as usize,
                            output_reserve: model.as_ref().and_then(|m|m["maxOutputTokens"].as_u64()).unwrap_or(4096) as usize,
                            budget_tokens: Some(4000),
                        },
                    )
                    .await?;
                    if pack["ok"] == true {
                        context["writingContext"] = json!({"fixed":pack["layers"]["fixed"],"recent":pack["layers"]["adjacent"]
                            .as_array().map(|a| a.iter().filter(|b| b["source"]["type"] == "cards").cloned().collect::<Vec<_>>()).unwrap_or_default()});
                    } else {
                        context["writingContext"] = pack["budgetExceeded"].clone();
                    }
                }
                context
            } else if method == "memory_sources" {
                crate::agent_memory::extraction_sources(
                    &mut tx,
                    &s,
                    args["phase"].as_str().unwrap_or("all"),
                )
                .await?
            } else if method == "memory_indexed" {
                for entry in args["sources"].as_array().ok_or("缺少已整理来源")? {
                    let source = &entry["ref"];
                    if source["type"] == "author" && source["sessionId"] != sid {
                        return Err("作者记忆来源不匹配".into());
                    }
                    if !matches!(source["type"].as_str(), Some("author" | "chapter" | "material" | "artifact")) { return Err("记忆来源类型无效".into()); }
                    let bid = str_field(source, "bookId")?;
                    service::allowed_book(&mut tx, &s, bid, false).await?;
                    let (revision, text) = crate::agent_memory::source_value(&mut tx, source).await?;
                    if entry["revision"] != revision {
                        return Err("记忆来源已经变化".into());
                    }
                    let projected = source["type"] == "author" && entry["authorProjection"].is_object();
                    if projected {
                        crate::agent_memory::reconcile_author_projection(&mut tx,source,&text,entry).await?;
                    }
                    put(
                        &mut tx,
                        OP_NS,
                        &crate::agent_memory::source_receipt_key(source),
                        &json!({"revision":revision,"semanticVersion":if projected {2} else {1}}),
                    )
                    .await?;
                }
                json!(true)
            } else if method == "memory_progress" {
                let phase = str_field(args, "phase")?;
                let state = str_field(args, "state")?;
                if !["author", "all"].contains(&phase) || !["running", "complete", "partial", "failed"].contains(&state) { return Err("记忆进度无效".into()); }
                if !s["memoryStatus"].is_object() { s["memoryStatus"] = json!({}); }
                let mut progress = s["memoryStatus"][phase].as_object().cloned().unwrap_or_default();
                progress.extend(args.as_object().ok_or("记忆进度格式无效")?.clone());
                let mut progress = Value::Object(progress);
                progress["runId"] = json!(run);
                progress["updatedAt"] = json!(now());
                put(&mut tx, OP_NS, &format!("memory-status:{sid}:{run}:{phase}"), &progress).await?;
                s["memoryStatus"][phase] = progress;
                let failed = s["memoryStatus"].as_object().into_iter().flat_map(|m|m.values())
                    .any(|p| matches!(p["state"].as_str(), Some("failed" | "partial")));
                s["memoryWarning"] = if failed { json!("作品记忆更新暂未完成，原始对话和稿件已保留。可查看原因并重试。") } else { Value::Null };
                save_session(&mut tx, &mut s).await?;
                json!(true)
            } else {
                s["memoryWarning"] =
                    json!("部分记忆尚未整理完成，下次继续时会重新核对来源。");
                save_session(&mut tx, &mut s).await?;
                json!(true)
            }
        }
        "steering" => {
            let mut s = session(&mut tx, sid).await?;
            let pending = s["pendingInputs"].as_array().cloned().unwrap_or_default();
            if !pending.is_empty() {
                s["pendingInputs"] = json!([]);
                save_session(&mut tx, &mut s).await?;
            }
            json!(pending)
        }
        "checkpoint_get" => {
            if args["id"].is_string() {
                get(&mut tx, CHECKPOINT_NS, &key)
                    .await?
                    .unwrap_or(Value::Null)
            } else {
                let mut rows = list(&mut tx, CHECKPOINT_NS, &prefix).await?;
                rows.sort_by(|a, b| b["id"].as_str().cmp(&a["id"].as_str()));
                rows.into_iter().next().unwrap_or(Value::Null)
            }
        }
        "checkpoint_list" => {
            let mut ids = list(&mut tx, CHECKPOINT_NS, &prefix)
                .await?
                .into_iter()
                .map(|r| r["id"].clone())
                .collect::<Vec<_>>();
            ids.sort_by(|a, b| b.as_str().cmp(&a.as_str()));
            json!(ids)
        }
        "checkpoint_put" => {
            let mut v = args.clone();
            v["writes"] = get(&mut tx, CHECKPOINT_NS, &key)
                .await?
                .map(|v| v["writes"].clone())
                .unwrap_or(json!([]));
            put(&mut tx, CHECKPOINT_NS, &key, &v).await?;
            Value::Null
        }
        "checkpoint_writes" => {
            let mut v = get(&mut tx, CHECKPOINT_NS, &key)
                .await?
                .ok_or("检查点尚未保存")?;
            let writes = v["writes"].as_array_mut().ok_or("检查点写入格式错误")?;
            for w in args["writes"].as_array().ok_or("检查点写入无效")? {
                if let Some(i) = writes
                    .iter()
                    .position(|old| old["task"] == w["task"] && old["index"] == w["index"])
                {
                    if w["index"].as_i64().unwrap_or(0) < 0 {
                        writes[i] = w.clone();
                    }
                } else {
                    writes.push(w.clone());
                }
            }
            put(&mut tx, CHECKPOINT_NS, &key, &v).await?;
            Value::Null
        }
        _ => return Err("未知后台协议方法".into()),
    };
    tx.commit().await.map_err(err)?;
    Ok(value)
}

// ---------------------------------------------------------------------------
// 索引器：独立于会话的后台进程。按 outbox 领取事件，目录与资料事件直接完成，
// 章节事件交给索引模型提取章节卡；同一时刻只有一个索引器在跑。
// ---------------------------------------------------------------------------

/// 延迟后启动一轮索引。启动、会话结束、作者点“立即索引”都走这里；已有索引器在跑就交给它。
pub fn schedule_index(app: DesktopAppHandle, delay: Duration) {
    std::thread::spawn(move || {
        if !delay.is_zero() {
            std::thread::sleep(delay);
        }
        // 参考书通道各跑各的：启动恢复、运行结束触发时一并看看有没有没拆完的书
        schedule_reference_index(app.clone());
        schedule_reference_search(app.clone());
        let host = app.state::<AgentHost>();
        if !host.storage_ready.load(Ordering::SeqCst) || host.quitting.load(Ordering::SeqCst) || host.maintenance.load(Ordering::SeqCst) {
            return;
        }
        let job = match claim_session(&host, INDEXER_JOB) {
            Ok(job) => job,
            Err(_) => return,
        };
        let outcome = index_rounds(&app, &job, crate::agent_indexer::QUIET_MS, crate::agent_indexer::Lane::Author);
        host.jobs.lock().unwrap().remove(INDEXER_JOB);
        emit_index(&app, &outcome);
    });
}

/// 变更事件等了这么久还没处理就补跑索引。
const BACKLOG_AGE_MS: i64 = 60_000;
const WATCH_TICK: Duration = Duration::from_secs(30);
const WATCH_MIN: Duration = Duration::from_secs(60);
const WATCH_MAX: Duration = Duration::from_secs(600);

/// 后台巡检：作者一直在写、没触发 Agent 运行时，变更事件积压超过一分钟也会补跑索引。
/// 一轮下来一章都没写成（多半没配模型或供应商故障）就逐步放慢，最长十分钟一次；有进展恢复一分钟。
pub fn start_index_watch(app: DesktopAppHandle) {
    std::thread::spawn(move || {
        let Ok(root) = crate::desktop_storage::root(&app) else {
            return;
        };
        let mut wait = WATCH_MIN;
        let mut last = Instant::now();
        loop {
            std::thread::sleep(WATCH_TICK);
            let host = app.state::<AgentHost>();
            if host.quitting.load(Ordering::SeqCst) {
                return;
            }
            if !host.storage_ready.load(Ordering::SeqCst) || host.maintenance.load(Ordering::SeqCst) {
                continue;
            }
            schedule_reference_search(app.clone());
            schedule_distillation(app.clone());
            // 参考书通道不受作者通道的退避影响：有没拆完的书就接上（已在拆时什么都不做）
            let reference_due = crate::blocking::run(async {
                let mut db = crate::desktop_storage::connect_existing_at(&root).await?;
                crate::agent_indexer::backlog_due_in(&mut db, now(), BACKLOG_AGE_MS, Some(crate::agent_indexer::Lane::Reference)).await
            })
            .unwrap_or(false);
            if reference_due {
                schedule_reference_index(app.clone());
            }
            if last.elapsed() < wait {
                continue;
            }
            let due = crate::blocking::run(async {
                let mut db = crate::desktop_storage::connect_existing_at(&root).await?;
                crate::agent_indexer::backlog_due_in(&mut db, now(), BACKLOG_AGE_MS, Some(crate::agent_indexer::Lane::Author)).await
            })
            .unwrap_or(false);
            if !due {
                wait = WATCH_MIN;
                continue;
            }
            // 已有索引器在跑（运行结束触发的、作者手动的）就等下一轮
            let Ok(job) = claim_session(&host, INDEXER_JOB) else {
                continue;
            };
            last = Instant::now();
            let outcome = index_rounds(&app, &job, crate::agent_indexer::QUIET_MS, crate::agent_indexer::Lane::Author);
            host.jobs.lock().unwrap().remove(INDEXER_JOB);
            emit_index(&app, &outcome);
            let progressed = outcome.as_ref().is_ok_and(|v| v["completed"].as_u64().unwrap_or(0) > 0);
            wait = if progressed { WATCH_MIN } else { (wait * 2).min(WATCH_MAX) };
        }
    });
}

/// 同步跑完一轮索引：不等安静期，用于批量连载每章开始前把上一章的章节卡补齐。
/// 已有索引器在跑就等它结束再跑；等待超过两分钟则放弃，本章用现有上下文。
pub(crate) fn run_indexer(app: &DesktopAppHandle) -> Result<Value, String> {
    let host = app.state::<AgentHost>();
    let started = Instant::now();
    let job = loop {
        if !host.storage_ready.load(Ordering::SeqCst) || host.quitting.load(Ordering::SeqCst) || host.maintenance.load(Ordering::SeqCst) {
            return Ok(json!({"skipped":"unavailable"}));
        }
        match claim_session(&host, INDEXER_JOB) {
            Ok(job) => break job,
            Err(e) if started.elapsed() > Duration::from_secs(120) => {
                return Ok(json!({"skipped":e}))
            }
            Err(_) => std::thread::sleep(Duration::from_millis(200)),
        }
    };
    let outcome = index_rounds(app, &job, 0, crate::agent_indexer::Lane::Author);
    host.jobs.lock().unwrap().remove(INDEXER_JOB);
    emit_index(app, &outcome);
    outcome
}

fn emit_index(app: &DesktopAppHandle, outcome: &Result<Value, String>) {
    let payload = match outcome {
        Ok(v) => json!({"event":"index_finished","summary":v}),
        Err(e) => json!({"event":"index_failed","message":e}),
    };
    crate::agent_events::emit(app, "agent-index", payload);
}

/// 反复领取直到没有可领取的事件。安静期内的事件领不到时，若还有待处理事件就等一个安静期再试，
/// 最多等三次，避免作者连续编辑时索引器长期空转。
///
/// 作者作品通道一次最多 50 轮；参考书通道一轮只领几章、一直拆到没有事件，每轮把用量记进拆书任务。
fn index_rounds(app: &DesktopAppHandle, job: &Arc<Job>, quiet_ms: i64, lane: crate::agent_indexer::Lane) -> Result<Value, String> {
    use crate::agent_indexer as indexer;
    let reference = lane == indexer::Lane::Reference;
    let root = crate::desktop_storage::root(app)?;
    let model = crate::blocking::run(async {
        let mut db = crate::desktop_storage::connect_existing_at(&root).await?;
        service::model_config(&mut db, "", "agent_indexer").await
    });
    // 没有可用模型时不领取事件：事件留在 outbox，配置好模型后下一轮再处理。
    let Ok(model) = model else {
        if reference {
            fail_breakdowns(app, &root, "还没有可用的文本模型。请先在模型管理中配置，也可以为“Agent 索引”单独指定一个便宜的模型，然后点继续。");
        }
        return Ok(json!({"skipped":"no_model"}));
    };
    let (limit, max_rounds) = if reference { (REFERENCE_BATCH, usize::MAX) } else { (indexer::BATCH_LIMIT, 50) };
    let mut summary = json!({"rounds":0,"completed":0,"retried":0,"givenUp":0,"skippedPaused":0,"usage":{"input_tokens":0,"output_tokens":0,"total_tokens":0}});
    let mut waited = 0;
    let mut rounds = 0usize;
    // 参考书通道连续几轮都只是模型输出格式不对（不是服务故障）：单章失败照常计次重试，连续两轮才停
    let mut format_only_rounds = 0;
    while rounds < max_rounds {
        rounds += 1;
        if job.cancelled.load(Ordering::SeqCst)
            || app.state::<AgentHost>().quitting.load(Ordering::SeqCst)
        {
            summary["stopped"] = json!(true);
            break;
        }
        let run_id = format!("index:{}", id());
        let batch = crate::blocking::run(async {
            let mut db = crate::desktop_storage::connect_existing_at(&root).await?;
            let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
            let batch = indexer::prepare_batch_in(&mut tx, &run_id, limit, quiet_ms, Some(lane)).await?;
            tx.commit().await.map_err(err)?;
            Ok::<_, String>(batch)
        })?;
        summary["skippedPaused"] = json!(summary["skippedPaused"].as_u64().unwrap_or(0) + batch.skipped_paused as u64);
        if batch.jobs.is_empty() && batch.pending.is_empty() && batch.books.is_empty() {
            let pending = crate::blocking::run(async {
                let mut db = crate::desktop_storage::connect_existing_at(&root).await?;
                indexer::pending_in(&mut db, lane).await
            })?;
            if quiet_ms > 0 && pending > 0 && waited < 3 {
                waited += 1;
                std::thread::sleep(Duration::from_millis(quiet_ms as u64));
                continue;
            }
            break;
        }
        let (saved, failures, usage, book_usage) = if batch.jobs.is_empty() {
            (BTreeSet::new(), Vec::new(), Value::Null, HashMap::new())
        } else {
            match index_process(app, &root, &run_id, &model, &batch.jobs, job) {
                Ok(result) => result,
                Err(error) => {
                    crate::blocking::run(async {
                        let mut db = crate::desktop_storage::connect_existing_at(&root).await?;
                        crate::agent_indexer::release(&mut db, &run_id).await?;
                        Ok::<_, String>(())
                    })?;
                    if reference {
                        fail_breakdowns(app, &root, &error);
                    }
                    return Err(error);
                }
            }
        };
        // 作者暂停、取消或应用退出时停掉的进程：没写成的章节放回队列，不算失败
        let interrupted = job.cancelled.load(Ordering::SeqCst);
        let finished = crate::blocking::run(async {
            let mut db = crate::desktop_storage::connect_existing_at(&root).await?;
            let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
            let out = indexer::finish_batch_with(&mut tx, &run_id, &batch, &saved, &failures, interrupted).await?;
            tx.commit().await.map_err(err)?;
            Ok::<_, String>(out)
        })?;
        summary["rounds"] = json!(summary["rounds"].as_u64().unwrap_or(0) + 1);
        for key in ["completed", "retried", "givenUp"] {
            summary[key] = json!(summary[key].as_u64().unwrap_or(0) + finished[key].as_u64().unwrap_or(0));
        }
        if usage.is_object() {
            for key in ["input_tokens", "output_tokens", "total_tokens"] {
                summary["usage"][key] = json!(summary["usage"][key].as_u64().unwrap_or(0) + usage[key].as_u64().unwrap_or(0));
            }
        }
        for book_id in &batch.books {
            crate::agent_events::emit(app, "agent-index", json!({"event":"index_book","bookId":book_id}));
            // 写作台的体检、状态账本、编辑器上下文缓存据此刷新
            crate::agent_events::emit(app, "agent-canon-changed", json!({"bookId":book_id,"reason":"index"}));
        }
        if reference {
            let views = crate::blocking::run(async {
                let mut db = crate::desktop_storage::connect_existing_at(&root).await?;
                let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
                let mut views = Vec::new();
                for book_id in &batch.books {
                    let used = book_usage.get(book_id).cloned().unwrap_or(Value::Null);
                    if let Some(view) = crate::agent_breakdown_job::record_round(&mut tx, book_id, &used).await? {
                        views.push(view);
                    }
                }
                tx.commit().await.map_err(err)?;
                Ok::<_, String>(views)
            })?;
            for (view, finished) in views {
                crate::agent_events::emit(app, "agent-breakdown", &view);
                if finished && view["autoContinue"] == true {
                    continue_after_breakdown(app, &root, &view);
                }
            }
        }
        // 模型一章都没写成（多半是供应商故障）时不再继续领取，失败计数已在 finish_batch 记录。
        if !batch.jobs.is_empty() && saved.is_empty() && !interrupted {
            let format_only = !failures.is_empty()
                && failures.iter().all(|f| matches!(f["code"].as_str(), Some("invalid_output" | "output_limit")));
            if reference && format_only {
                format_only_rounds += 1;
                if format_only_rounds < 2 {
                    continue;
                }
            }
            summary["aborted"] = json!("all_failed");
            if reference {
                let reason = failures
                    .iter()
                    .find_map(|f| f["message"].as_str())
                    .unwrap_or("这一轮的章节全部提取失败");
                let advice = if format_only {
                    "当前模型连续几章都没按格式返回章节卡，可以点继续重试，或在模型管理里给“Agent 索引”换一个模型"
                } else {
                    "多半是模型服务暂时不可用，检查模型设置或网络后点继续"
                };
                fail_breakdowns(app, &root, &format!("章节卡提取失败：{reason}。{advice}。"));
            }
            break;
        }
        format_only_rounds = 0;
    }
    // 作者作品通道每轮结束顺手补向量：配置了向量模型才做；供应商出错只记进摘要，不影响索引结果
    if !reference {
        let filled = crate::blocking::run(async {
            let mut db = crate::desktop_storage::connect_existing_at(&root).await?;
            let Some(model) = crate::agent_vectors::embedding_model(&mut db).await? else { return Ok::<Value, String>(Value::Null) };
            let embedder = crate::agent_vectors::RemoteEmbedder::new(&model, Some(&root));
            let key = crate::agent_vectors::model_key(&model);
            match crate::agent_vectors::fill_missing(&mut db, &key, &embedder, crate::agent_vectors::MAX_PER_PASS).await {
                Ok(written) => Ok(json!({"written":written,"missing":crate::agent_vectors::missing_count(&mut db, &key).await?})),
                Err(error) => Ok(json!({"error":error})),
            }
        });
        match filled {
            Ok(Value::Null) => {}
            Ok(value) => summary["vectors"] = value,
            Err(error) => summary["vectors"] = json!({"error":error}),
        }
    }
    Ok(summary)
}

/// 拆完了：在发起拆书的会话里替作者接着发一轮，让 Agent 把作者原来的要求做完。
/// 会话正在跑别的任务、在等作者回答或被作者暂停时不打扰，进度卡上留“让 Agent 继续分析”给作者自己点。
fn continue_after_breakdown(app: &DesktopAppHandle, root: &Path, job: &Value) {
    let (Some(sid), Some(pid)) = (job["sessionId"].as_str(), job["projectId"].as_str()) else {
        return;
    };
    let outcome = (|| {
        let s = crate::blocking::run(async {
            let mut db = crate::desktop_storage::connect_existing_at(root).await?;
            session(&mut db, sid).await
        })?;
        match s["status"].as_str().unwrap_or("") {
            "running" => return Err("对话正在进行别的任务".to_owned()),
            "choice" => return Err("对话在等你回答问题".to_owned()),
            "paused" => return Err("对话已暂停".to_owned()),
            _ => {}
        }
        let failed = job["failed"].as_u64().unwrap_or(0);
        let text = format!(
            "《{}》第 {}–{} 章已经拆完{}，请接着完成刚才的要求，并用 write_breakdown_report 保存可重新打开的拆书报告。",
            job["title"].as_str().unwrap_or("参考书"),
            job["from"],
            job["to"],
            if failed > 0 { format!("（其中 {failed} 章没拆成，结论里说明）") } else { String::new() }
        );
        crate::blocking::run(start(
            app.clone(),
            json!({"sessionId":sid,"inputKind":"message","text":text,"modelCode":s["modelCode"],"source":"breakdown"}),
        ))
    })();
    let view = crate::blocking::run(async {
        let mut db = crate::desktop_storage::connect_existing_at(root).await?;
        let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
        let view = crate::agent_breakdown_job::mark_continue(&mut tx, pid, outcome).await?;
        tx.commit().await.map_err(err)?;
        Ok::<_, String>(view)
    });
    if let Ok(Some(view)) = view {
        crate::agent_events::emit(app, "agent-breakdown", &view);
    }
}

/// 参考书通道跑不下去：进行中的拆书任务标为失败并通知界面。
fn fail_breakdowns(app: &DesktopAppHandle, root: &Path, message: &str) {
    let views = crate::blocking::run(async {
        let mut db = crate::desktop_storage::connect_existing_at(root).await?;
        let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
        let views = crate::agent_breakdown_job::fail_running(&mut tx, message).await?;
        tx.commit().await.map_err(err)?;
        Ok::<_, String>(views)
    });
    for view in views.unwrap_or_default() {
        crate::agent_events::emit(app, "agent-breakdown", &view);
    }
}

/// 参考书通道：有排队的拆书事件就在后台线程里一直拆。被作者暂停某本书打断时（只停当前进程），
/// 还有别的书在排队就换一个新任务接着拆；应用退出或维护时直接结束。
pub fn schedule_reference_index(app: DesktopAppHandle) {
    std::thread::spawn(move || loop {
        let host = app.state::<AgentHost>();
        if !host.storage_ready.load(Ordering::SeqCst) || host.quitting.load(Ordering::SeqCst) || host.maintenance.load(Ordering::SeqCst) {
            return;
        }
        let Ok(root) = crate::desktop_storage::root(&app) else { return };
        let pending = crate::blocking::run(async {
            let mut db = crate::desktop_storage::connect_existing_at(&root).await?;
            crate::agent_indexer::pending_in(&mut db, crate::agent_indexer::Lane::Reference).await
        })
        .unwrap_or(0);
        if pending == 0 {
            return;
        }
        let Ok(job) = claim_session(&host, REFERENCE_JOB) else { return };
        let outcome = index_rounds(&app, &job, 0, crate::agent_indexer::Lane::Reference);
        host.jobs.lock().unwrap().remove(REFERENCE_JOB);
        crate::agent_events::emit(&app, "agent-index", json!({"event":"index_finished","lane":"reference","summary":outcome.as_ref().ok()}));
        // 作者暂停某本书打断了进程，或这一趟有进展（收尾时可能又有书排进来）：再看一眼队列。
        // 一章都没拆成（供应商故障等）就停下，等巡检或作者操作再来，不在这里空转。
        let again = outcome
            .as_ref()
            .is_ok_and(|v| v["stopped"] == true || v["completed"].as_u64().unwrap_or(0) > 0);
        if !again {
            return;
        }
    });
}

/// 停下参考书通道当前的进程（作者暂停或取消某本书）；通道线程随后换新任务处理其他书。
fn interrupt_reference_lane(app: &DesktopAppHandle) {
    let host = app.state::<AgentHost>();
    host.stop(REFERENCE_JOB);
}

/// 启动一次索引进程处理一批章节。进程只被允许一个协议方法：`canon_put` 写章节卡；
/// 其余方法一律拒绝，索引器拿不到会话工具与检查点。
fn index_process(
    app: &DesktopAppHandle,
    root: &Path,
    run_id: &str,
    model: &Value,
    jobs: &[Value],
    job: &Arc<Job>,
) -> Result<(BTreeSet<(String, String)>, Vec<Value>, Value, HashMap<String, Value>), String> {
    let mut command = worker_command(app)?;
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
        .map_err(|e| format!("无法启动索引进程：{e}"))?;
    let _stderr = crate::agent_worker_pool::attach_stderr(&mut child, "indexer", &crate::agent_worker_pool::log_dir(root));
    job.pid.store(child.id(), Ordering::SeqCst);
    if job.cancelled.load(Ordering::SeqCst) {
        stop_job(job);
    }
    let mut input = child.stdin.take().ok_or("无法连接索引进程输入")?;
    let gateway = crate::model_gateway::endpoint(root)?;
    let payload = json!({"action":"index","runId":run_id,"model":crate::model_gateway::worker_model(&gateway, model),"jobs":jobs});
    writeln!(input, "{}", payload).map_err(err)?;
    let output = child.stdout.take().ok_or("无法连接索引进程输出")?;
    let mut saved = BTreeSet::new();
    let mut failures = Vec::new();
    let mut usage = Value::Null;
    // 每本书各用了多少 token（逐章的提取用量），拆书任务据此累计
    let mut book_usage: HashMap<String, Value> = HashMap::new();
    let result: Result<(), String> = (|| {
        for line in BufReader::new(output).lines() {
            let line = line.map_err(err)?;
            if line.len() > 8_000_000 {
                return Err("索引进程输出超过协议限制".into());
            }
            let event: Value =
                serde_json::from_str(&line).map_err(|_| "索引进程返回无效数据".to_string())?;
            if let Some(request) = event["rpc"].as_str() {
                let params = &event["params"];
                let result: Result<Value, String> = if job.cancelled.load(Ordering::SeqCst) {
                    Err("索引已停止".into())
                } else if event["method"] == "canon_put" {
                    let book_id = str_field(params, "bookId")?.to_owned();
                    let chapter_id = str_field(params, "chapterId")?.to_owned();
                    let source_revision = str_field(params, "sourceRevision")?.to_owned();
                    let card = params["card"].clone();
                    let written = crate::blocking::run(async {
                        let mut db = crate::desktop_storage::connect_existing_at(root).await?;
                        let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
                        let out = crate::agent_canon::put_card(&mut tx, &book_id, &chapter_id, &source_revision, &card).await?;
                        tx.commit().await.map_err(err)?;
                        Ok::<_, String>(out)
                    });
                    if written.is_ok() {
                        saved.insert((book_id, chapter_id));
                    }
                    written
                } else {
                    Err("索引进程不允许该操作".into())
                };
                let response = match result {
                    Ok(v) => json!({"rpc":request,"result":v}),
                    Err(e) => json!({"rpc":request,"error":e}),
                };
                writeln!(input, "{response}").map_err(err)?;
                input.flush().map_err(err)?;
                continue;
            }
            match event["event"].as_str().unwrap_or("") {
                "index_progress" => {
                    if let (Some(book_id), Some(used)) = (event["bookId"].as_str(), event["usage"].as_object()) {
                        let entry = book_usage.entry(book_id.to_owned()).or_insert_with(|| json!({}));
                        for key in ["input_tokens", "output_tokens", "total_tokens"] {
                            entry[key] = json!(entry[key].as_u64().unwrap_or(0) + used.get(key).and_then(Value::as_u64).unwrap_or(0));
                        }
                    }
                    crate::agent_events::emit(app, "agent-index", &event);
                }
                "index_failed" => {
                    failures.push(json!({"bookId":event["bookId"],"chapterId":event["chapterId"],"message":event["message"],"code":event["code"]}));
                    crate::agent_events::emit(app, "agent-index", &event);
                }
                "index_done" => {
                    usage = event["usage"].clone();
                }
                _ => {}
            }
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = child.kill();
    }
    let status = child.wait().map_err(err)?;
    result?;
    if !status.success() && !job.cancelled.load(Ordering::SeqCst) {
        // 进程中途退出：已写入的章节卡按指纹提交，其余章节按失败处理并在下一轮重试。
        for j in jobs {
            let key = (
                j["bookId"].as_str().unwrap_or("").to_owned(),
                j["chapterId"].as_str().unwrap_or("").to_owned(),
            );
            if !saved.contains(&key) && !failures.iter().any(|f| f["bookId"] == key.0 && f["chapterId"] == key.1) {
                failures.push(json!({"bookId":key.0,"chapterId":key.1,"message":"索引进程中断"}));
            }
        }
    }
    Ok((saved, failures, usage, book_usage))
}

// ---------------------------------------------------------------------------
// 批量连载：逐章运行，每章一次独立的会话运行；索引在每章前同步完成。
// ---------------------------------------------------------------------------

pub fn schedule_batch(app: DesktopAppHandle, job_id: String) {
    std::thread::spawn(move || {
        let lease = format!("batch-runner:{job_id}");
        let host = app.state::<AgentHost>();
        if claim_session(&host, &lease).is_err() { return; }
        let result = run_batch(&app, &job_id);
        host.jobs.lock().unwrap().remove(&lease);
        crate::agent_events::emit(&app,
            "agent-batch",
            json!({"jobId":job_id,"event":"runner_finished","error":result.as_ref().err()}),
        );
    });
}

fn run_batch(app: &DesktopAppHandle, job_id: &str) -> Result<(), String> {
    use crate::agent_batch as batch;
    let root = crate::desktop_storage::root(app)?;
    loop {
        let host = app.state::<AgentHost>();
        if host.maintenance.load(Ordering::SeqCst) || host.quitting.load(Ordering::SeqCst) {
            return Ok(());
        }
        let next = crate::blocking::run(async {
            let mut db = crate::desktop_storage::connect_existing_at(&root).await?;
            let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
            let next = batch::next_step(&mut tx, job_id).await?;
            tx.commit().await.map_err(err)?;
            Ok::<_, String>(next)
        })?;
        let Some((job, index)) = next else {
            let job = crate::blocking::run(async {
                let mut db = crate::desktop_storage::connect_existing_at(&root).await?;
                batch::load(&mut db, job_id).await
            })?;
            crate::agent_events::emit(app, "agent-batch", &job);
            return Ok(());
        };
        let sid = str_field(&job, "sessionId")?.to_owned();
        // 每章开始前同步索引，让本章上下文包含上一章的章节卡
        let indexing = run_indexer(app);
        let readiness = crate::blocking::run(async {
            let mut db = crate::desktop_storage::connect_existing_at(&root).await?;
            let current = batch::load(&mut db, job_id).await?;
            if current["status"] != "running" { return Ok::<bool,String>(false); }
            batch::ensure_index_ready(&mut db, &job, index).await?;
            Ok(true)
        });
        match readiness {
            Ok(false) => return Ok(()),
            Ok(true) => {},
            Err(error) => {
                let error = match indexing { Err(cause) => format!("{error}；{cause}"), _ => error };
                record(app, &root, job_id, index, &json!({"sessionStatus":"completed","artifactId":null,"error":error}))?;
                return Ok(());
            }
        }
        let step_job = match claim_session(&host, &sid) {
            Ok(j) => j,
            Err(e) => {
                record(app, &root, job_id, index, &json!({"sessionStatus":"failed","error":e}))?;
                return Ok(());
            }
        };
        let thread = format!("{sid}:batch:{job_id}:{index}");
        let resume = crate::blocking::run(async {
            let mut db = crate::desktop_storage::connect_existing_at(&root).await?;
            let s = session(&mut db, &sid).await?;
            Ok::<bool,String>(s["activeThreadId"] == thread
                && ["paused","failed"].contains(&s["status"].as_str().unwrap_or("")))
        })?;
        let args = json!({"sessionId":sid,"text":batch::step_instruction(&job, index),"modelCode":job["modelCode"],
            "selectedSkillId":batch::step_skill(&job),"batch":true,"thread":thread,"batchStep":{"jobId":job_id,"index":index},"resume":resume});
        let prepared = crate::blocking::run(prepare_run(&root, &sid, &args));
        let mut payload = match prepared {
            Ok(v) if v["deferred"] == true => {
                host.jobs.lock().unwrap().remove(&sid);
                return Ok(());
            }
            Ok(v) => v,
            Err(e) => {
                host.jobs.lock().unwrap().remove(&sid);
                record(app, &root, job_id, index, &json!({"sessionStatus":"failed","error":e}))?;
                return Ok(());
            }
        };
        payload["thread"] = json!(format!("{sid}:batch:{job_id}:{index}"));
        payload["batchStep"] = json!({"jobId":job_id,"index":index});
        let run_id = str_field(&payload, "runId")?.to_owned();
        let session_value = match run_prepared(app, &root, &sid, payload, step_job) {
            Ok(value) => value,
            Err(error) => {
                record(app, &root, job_id, index, &json!({"sessionStatus":"failed","runId":run_id,"error":error}))?;
                return Ok(());
            }
        };
        let outcome = crate::blocking::run(async {
            let mut db = crate::desktop_storage::connect_existing_at(&root).await?;
            batch::outcome_from_session(&mut db, &session_value, &run_id).await
        })?;
        let job = record(app, &root, job_id, index, &outcome)?;
        if job["status"] != "running" {
            return Ok(());
        }
    }
}

fn record(app: &DesktopAppHandle, root: &Path, job_id: &str, index: usize, outcome: &Value) -> Result<Value, String> {
    let job = crate::blocking::run(async {
        let mut db = crate::desktop_storage::connect_existing_at(root).await?;
        let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
        let job = crate::agent_batch::record_step(&mut tx, job_id, index, outcome).await?;
        tx.commit().await.map_err(err)?;
        Ok::<_, String>(job)
    })?;
    crate::agent_events::emit(app, "agent-batch", &job);
    Ok(job)
}

/// 全文检索与模型拆书分离；每批最多四章，一个短事务，应用退出可由游标恢复。
fn schedule_reference_search(app: DesktopAppHandle) {
    std::thread::spawn(move || {
        let host=app.state::<AgentHost>();
        if !host.storage_ready.load(Ordering::SeqCst) || host.maintenance.load(Ordering::SeqCst) { return; }
        let Ok(job)=claim_session(&host,SEARCH_JOB) else { return; };
        let result=(|| -> Result<(),String> {
            let root=crate::desktop_storage::root(&app)?;
            while !job.cancelled.load(Ordering::SeqCst) && !host.quitting.load(Ordering::SeqCst) {
                if !crate::blocking::run(crate::agent_reference_import::index_next(&root))? { break; }
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(())
        })();
        host.jobs.lock().unwrap().remove(SEARCH_JOB);
        if let Err(error)=result { eprintln!("参考书检索待续建：{error}"); }
    });
}

/// Distillation shares the resident worker and persistent outbox with indexing.
/// Reading, summaries and evidence checks resume from committed node results.
fn schedule_distillation(app: DesktopAppHandle) {
    std::thread::spawn(move || {
        let host=app.state::<AgentHost>();
        if !host.storage_ready.load(Ordering::SeqCst)||host.quitting.load(Ordering::SeqCst)||host.maintenance.load(Ordering::SeqCst){return;}
        let Ok(root)=crate::desktop_storage::root(&app) else{return;};
        let active=crate::blocking::run(async{let mut db=crate::desktop_storage::connect_existing_at(&root).await?;Ok::<_,String>(list(&mut db,NS,"distillation-job:").await?.iter().any(|j|j["status"]=="running"))}).unwrap_or(false);
        if !active{return;}
        let Ok(job)=claim_session(&host,DISTILLATION_JOB) else{return;};
        loop {
            if job.cancelled.load(Ordering::SeqCst)||host.quitting.load(Ordering::SeqCst){break;}
            let result=crate::agent_distillation::run_batch(&root,||worker_command(&app),&job.cancelled,
                |view|crate::agent_events::emit(&app,"agent-distillation",view),
                |pid|job.pid.store(pid,Ordering::SeqCst));
            job.pid.store(0,Ordering::SeqCst);
            let views=crate::blocking::run(async{let mut db=crate::desktop_storage::connect_existing_at(&root).await?;crate::agent_distillation::list_views(&mut db).await}).unwrap_or_default();
            for view in views {crate::agent_events::emit(&app,"agent-distillation",&view);}
            if !matches!(result,Ok(true)){break;}
        }
        host.jobs.lock().unwrap().remove(DISTILLATION_JOB);
    });
}
