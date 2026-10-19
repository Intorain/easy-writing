//! 调度器（全平台方案 5.9，借 OpenClaw 的自动化模型在宿主内实现，决策 D7 / D13 / D14）。
//!
//! - 任务：定义、运行态、历史都是 SQLite 里的记录，重启不丢。一次性或周期（每天 / 每周 / 每隔若干分钟，按本机时区）。
//!   载荷两种：隔离的 Agent 回合（`agent`）、宿主命令（`command`）。
//!   投递目标：微信、桌面通知、不投递。
//! - 隔离会话：Agent 任务跑在 `job:<id>` 会话里，不污染作者正在聊的会话；无人值守，模型不提问。
//! - 由 Agent 自建：`create_automation` 工具，建立须经确认卡，卡上写明频率与估算用量（D13）。
//! - 失败语义：连续出错自动停用并通知作者（D14）；每次运行有墙钟上限；重启后错过的时间点合并成一次补跑，
//!   不回放已完成的载荷；同一任务在跑时不起第二个。
//! - 事件提醒：批量结束、拆书完成（订阅宿主事件），伏笔久未推进、字数达标、榜单过期、停笔整理记忆（每小时查一次数据，
//!   见 agent_heartbeat），每条可开关、改阈值、选送达方式；提醒时段是总设置。
use crate::agent_delivery_queue as delivery;
use crate::agent_store::*;
use chrono::{Datelike, Local, TimeZone, Timelike};
use serde_json::{json, Value};
use sqlx::SqliteConnection;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use tauri::Manager;

pub const NS: &str = delivery::NS;
const TASK: &str = "task:";
const RUN: &str = "run:";
const RULE: &str = "rule:";
/// 旧版内置的"助理巡检"任务编号：现在拆成了事件提醒里的规则，启动时清掉
const LEGACY_HEARTBEAT_ID: &str = "heartbeat";
const SETTINGS: &str = "settings";
/// 周期检查的间隔
const CHECK_INTERVAL_MS: i64 = 60 * 60 * 1000;
const TICK_SECS: u64 = 30;
/// 连续失败这么多次自动停用
pub const MAX_FAILURES: i64 = 3;
pub const DEFAULT_MAX_RUN_SECS: i64 = 600;
pub const MIN_INTERVAL_MINUTES: i64 = 15;
/// 建任务时没有可参考的用量，按一次普通回合估
const DEFAULT_RUN_TOKENS: i64 = 30_000;
const KEEP_RUNS: usize = 30;
const TITLE_MAX: usize = 40;
const INSTRUCTION_MAX: usize = 2000;
/// 事件提醒：事件名、说明（{参数名} 处由作者填数字）、默认送达方式、可调参数（名、默认值、下限、上限）
pub const EVENT_RULES: [(&str, &str, &str, Option<(&str, i64, i64, i64)>); 6] = [
    ("batch_finished", "批量连载跑完或中断后汇报", delivery::WEIXIN, None),
    ("breakdown_finished", "拆书任务完成后通知", delivery::WEIXIN, None),
    ("threads_overdue", "伏笔超过 {chapters} 章没推进时提醒", delivery::DESKTOP, Some(("chapters", 15, 3, 100))),
    ("words_target_reached", "今日码字达到目标时提醒", delivery::DESKTOP, None),
    ("rank_stale", "榜单超过 {days} 天没更新时提醒", delivery::DESKTOP, Some(("days", 3, 1, 30))),
    ("idle_consolidate", "停笔 {minutes} 分钟后把改动整理进记忆", delivery::NONE, Some(("minutes", 30, 10, 240))),
];

// ---------------------------------------------------------------------------
// 时间表
// ---------------------------------------------------------------------------

fn parse_time(text: &str) -> Result<(u32, u32), String> {
    let (h, m) = text.trim().split_once(':').ok_or("时间要写成 HH:MM，比如 08:00")?;
    let h: u32 = h.parse().map_err(|_| "时间要写成 HH:MM，比如 08:00")?;
    let m: u32 = m.parse().map_err(|_| "时间要写成 HH:MM，比如 08:00")?;
    if h > 23 || m > 59 {
        return Err("时间超出范围".into());
    }
    Ok((h, m))
}

/// 校验并整理时间表：{type: once|daily|weekly|interval, time, weekday, minutes, at}
pub fn validate_schedule(schedule: &Value) -> Result<Value, String> {
    let kind = schedule["type"].as_str().ok_or("时间表缺少 type")?;
    Ok(match kind {
        "daily" => {
            let (h, m) = parse_time(schedule["time"].as_str().ok_or("每天的任务要给 time")?)?;
            json!({"type":"daily","time":format!("{h:02}:{m:02}")})
        }
        "weekly" => {
            let (h, m) = parse_time(schedule["time"].as_str().ok_or("每周的任务要给 time")?)?;
            let weekday = schedule["weekday"].as_i64().ok_or("每周的任务要给 weekday（1 周一 … 7 周日）")?;
            if !(1..=7).contains(&weekday) {
                return Err("weekday 要在 1（周一）到 7（周日）之间".into());
            }
            json!({"type":"weekly","time":format!("{h:02}:{m:02}"),"weekday":weekday})
        }
        "interval" => {
            let minutes = schedule["minutes"].as_i64().ok_or("间隔任务要给 minutes")?;
            if minutes < MIN_INTERVAL_MINUTES {
                return Err(format!("间隔不能短于 {MIN_INTERVAL_MINUTES} 分钟"));
            }
            json!({"type":"interval","minutes":minutes})
        }
        "once" => {
            let at = schedule["at"].as_i64().ok_or("一次性任务要给 at（毫秒时间戳）")?;
            json!({"type":"once","at":at})
        }
        other => return Err(format!("未知的时间表类型：{other}")),
    })
}

fn local_at(date: chrono::NaiveDate, h: u32, m: u32) -> Option<i64> {
    Local.with_ymd_and_hms(date.year(), date.month(), date.day(), h, m, 0).earliest().map(|t| t.timestamp_millis())
}

/// `after_ms` 之后的下一个运行时间；一次性任务已过时返回 None
pub fn next_run_at(schedule: &Value, after_ms: i64) -> Option<i64> {
    let after = Local.timestamp_millis_opt(after_ms).earliest()?;
    match schedule["type"].as_str().unwrap_or("") {
        "interval" => Some(after_ms + schedule["minutes"].as_i64().unwrap_or(60) * 60_000),
        "once" => schedule["at"].as_i64().filter(|at| *at > after_ms),
        "daily" => {
            let (h, m) = parse_time(schedule["time"].as_str().unwrap_or("08:00")).ok()?;
            let today = local_at(after.date_naive(), h, m)?;
            if today > after_ms {
                Some(today)
            } else {
                local_at(after.date_naive().succ_opt()?, h, m)
            }
        }
        "weekly" => {
            let (h, m) = parse_time(schedule["time"].as_str().unwrap_or("08:00")).ok()?;
            let wanted = schedule["weekday"].as_i64().unwrap_or(1) as u32;
            let current = after.weekday().number_from_monday();
            let ahead = (wanted + 7 - current) % 7;
            let date = after.date_naive().checked_add_days(chrono::Days::new(ahead as u64))?;
            let at = local_at(date, h, m)?;
            if at > after_ms {
                Some(at)
            } else {
                local_at(date.checked_add_days(chrono::Days::new(7))?, h, m)
            }
        }
        _ => None,
    }
}

const WEEKDAYS: [&str; 7] = ["一", "二", "三", "四", "五", "六", "日"];

pub fn describe_schedule(schedule: &Value) -> String {
    match schedule["type"].as_str().unwrap_or("") {
        "daily" => format!("每天 {}", schedule["time"].as_str().unwrap_or("")),
        "weekly" => format!(
            "每周{} {}",
            WEEKDAYS[(schedule["weekday"].as_u64().unwrap_or(1).clamp(1, 7) - 1) as usize],
            schedule["time"].as_str().unwrap_or("")
        ),
        "interval" => {
            let minutes = schedule["minutes"].as_i64().unwrap_or(0);
            if minutes % 60 == 0 { format!("每 {} 小时", minutes / 60) } else { format!("每 {minutes} 分钟") }
        }
        "once" => Local
            .timestamp_millis_opt(schedule["at"].as_i64().unwrap_or(0))
            .earliest()
            .map(|t| format!("{} 一次", t.format("%Y-%m-%d %H:%M")))
            .unwrap_or_else(|| "一次".into()),
        _ => "未知".into(),
    }
}

pub fn runs_per_week(schedule: &Value) -> f64 {
    match schedule["type"].as_str().unwrap_or("") {
        "daily" => 7.0,
        "weekly" => 1.0,
        "interval" => 7.0 * 24.0 * 60.0 / schedule["minutes"].as_i64().unwrap_or(60).max(1) as f64,
        _ => 1.0,
    }
}

/// 用量估算：按发起会话里每轮回复的平均 token，乘每周次数；没有参考时按一次普通回合估
pub fn estimate(session: &Value, schedule: &Value) -> Value {
    let mut total = 0i64;
    let mut count = 0i64;
    for m in session["messages"].as_array().into_iter().flatten() {
        if m["role"] == "assistant" && m["id"].as_str().is_some_and(|id| id.ends_with(":reply")) {
            if let Some(t) = m["usage"]["total_tokens"].as_i64().filter(|t| *t > 0) {
                total += t;
                count += 1;
            }
        }
    }
    let per_run = if count > 0 { total / count } else { DEFAULT_RUN_TOKENS };
    let runs = runs_per_week(schedule);
    json!({"perRunTokens":per_run,"runsPerWeek":(runs * 10.0).round() / 10.0,"weeklyTokens":(per_run as f64 * runs).round() as i64,
        "basis":if count > 0 { format!("按本次对话 {count} 轮回复的平均用量") } else { "没有可参考的对话用量，按一次普通回合估".to_owned() }})
}

fn thousands(tokens: i64) -> String {
    if tokens >= 1000 { format!("{:.1} 千", tokens as f64 / 1000.0) } else { tokens.to_string() }
}

/// 确认卡上的用量一句话
pub fn describe_estimate(estimate: &Value) -> String {
    format!(
        "每次约 {} token，每周约 {} 次（约 {} token/周）",
        thousands(estimate["perRunTokens"].as_i64().unwrap_or(0)),
        estimate["runsPerWeek"].as_f64().unwrap_or(0.0),
        thousands(estimate["weeklyTokens"].as_i64().unwrap_or(0))
    )
}

pub fn describe_deliver(deliver: &str) -> &'static str {
    match deliver {
        delivery::WEIXIN => "结果发到微信",
        delivery::DESKTOP => "结果在桌面提醒",
        _ => "结果不投递，只留在任务记录里",
    }
}

// ---------------------------------------------------------------------------
// 任务记录
// ---------------------------------------------------------------------------

fn task_key(id: &str) -> String {
    format!("{TASK}{id}")
}

pub async fn get_task(db: &mut SqliteConnection, id: &str) -> Result<Option<Value>, String> {
    get(db, NS, &task_key(id)).await
}

pub async fn save_task(db: &mut SqliteConnection, task: &Value) -> Result<(), String> {
    put(db, NS, &task_key(str_field(task, "id")?), task).await
}

/// 全部任务，按建立时间
pub async fn list_tasks(db: &mut SqliteConnection) -> Result<Vec<Value>, String> {
    retire_heartbeat_task(db).await?;
    let mut tasks = list(db, NS, TASK).await?;
    tasks.sort_by_key(|t| t["createdAt"].as_i64().unwrap_or(0));
    Ok(tasks)
}

/// 给模型看的任务清单：少量字段，时间表写成人话
pub async fn tasks_view(db: &mut SqliteConnection) -> Result<Vec<Value>, String> {
    Ok(list_tasks(db)
        .await?
        .into_iter()
        .map(|t| {
            json!({"id":t["id"],"title":t["title"],"kind":t["kind"],"schedule":describe_schedule(&t["schedule"]),"deliver":t["deliver"],
                "enabled":t["enabled"],"instruction":t["instruction"],"nextRunAt":t["nextRunAt"],"lastStatus":t["lastStatus"],"lastRunAt":t["lastRunAt"]})
        })
        .collect())
}

/// 旧版内置的"助理巡检"任务拆成了事件提醒：删掉任务与它的运行记录，已提醒过的键搬进设置里不重复提醒
pub async fn retire_heartbeat_task(db: &mut SqliteConnection) -> Result<(), String> {
    let Some(task) = get_task(db, LEGACY_HEARTBEAT_ID).await? else { return Ok(()) };
    let mut settings = settings(db).await?;
    if task["state"]["notified"].is_object() && !settings["checks"]["notified"].is_object() {
        settings["checks"] = task["state"].clone();
        put(db, NS, SETTINGS, &settings).await?;
    }
    delete_by_prefix(db, NS, &task_key(LEGACY_HEARTBEAT_ID)).await?;
    delete_by_prefix(db, NS, &format!("{RUN}{LEGACY_HEARTBEAT_ID}:")).await?;
    Ok(())
}

/// 事件提醒的总设置：提醒时段、周期检查的状态
pub async fn settings(db: &mut SqliteConnection) -> Result<Value, String> {
    Ok(get(db, NS, SETTINGS).await?.unwrap_or_else(|| json!({"activeHours":[8,23],"checks":{}})))
}

pub async fn settings_update(db: &mut SqliteConnection, patch: &Value) -> Result<Value, String> {
    let mut current = settings(db).await?;
    if let Some(hours) = patch["activeHours"].as_array() {
        let (Some(start), Some(end)) = (hours.first().and_then(Value::as_u64), hours.get(1).and_then(Value::as_u64)) else {
            return Err("提醒时段要给起止小时".into());
        };
        if start > 23 || end > 24 {
            return Err("提醒时段的小时超出范围".into());
        }
        current["activeHours"] = json!([start, end]);
    }
    put(db, NS, SETTINGS, &current).await?;
    Ok(current)
}

/// 模型工具 create_automation：校验、查重、按操作编号留回执；确认卡已由审批门在此之前发出并核对
pub async fn create(db: &mut SqliteConnection, s: &Value, args: &Value) -> Result<Value, String> {
    let sid = str_field(s, "id")?;
    let op = args["operation_id"].as_str().unwrap_or("");
    let receipt_key = format!("automation-op:{sid}:{op}");
    if !op.is_empty() {
        if let Some(receipt) = get(db, OP_NS, &receipt_key).await? {
            return Ok(receipt);
        }
    }
    let title = args["title"].as_str().unwrap_or("").trim().to_owned();
    if title.is_empty() || title.chars().count() > TITLE_MAX {
        return Err(format!("任务名要有，且不超过 {TITLE_MAX} 字"));
    }
    let instruction = args["instruction"].as_str().unwrap_or("").trim().to_owned();
    if instruction.is_empty() || instruction.chars().count() > INSTRUCTION_MAX {
        return Err(format!("任务内容要有，且不超过 {INSTRUCTION_MAX} 字"));
    }
    if let Some(error) = preflight(db, s, args).await? {
        return Ok(error);
    }
    let schedule = validate_schedule(&args["schedule"])?;
    let channel_kind = s["channel"]["kind"].as_str().unwrap_or(delivery::DESKTOP).to_owned();
    let deliver = args["deliver"].as_str().filter(|v| !v.is_empty()).map(str::to_owned).unwrap_or(channel_kind);
    if !delivery::channel_valid(&deliver) {
        return Err(format!("投递目标只能是 weixin / desktop / none，收到：{deliver}"));
    }
    let now_ms = now();
    let Some(next) = next_run_at(&schedule, now_ms) else {
        return Err("一次性任务的时间已经过了".into());
    };
    let book_id = args["book_id"].as_str().filter(|v| !v.is_empty()).map(str::to_owned);
    let task = json!({"id":id(),"title":title,"kind":"agent","instruction":instruction,"schedule":schedule,"deliver":deliver,
        "bookId":book_id,"modelCode":s["modelCode"].as_str().unwrap_or(""),"enabled":true,"builtin":false,
        "createdAt":now_ms,"createdBy":{"sessionId":sid,"channel":s["channel"]["kind"]},
        "nextRunAt":next,"lastRunAt":Value::Null,"lastStatus":"","lastError":"","lastSummary":"","failures":0,"disabledReason":"",
        "maxRunSeconds":DEFAULT_MAX_RUN_SECS,"estimate":estimate(s, &schedule),"state":{}});
    save_task(db, &task).await?;
    let out = json!({"ok":true,"task":{"id":task["id"],"title":task["title"],"schedule":describe_schedule(&task["schedule"]),"deliver":task["deliver"],
        "nextRunAt":next,"estimate":task["estimate"]}});
    if !op.is_empty() {
        put(db, OP_NS, &receipt_key, &out).await?;
    }
    Ok(out)
}

/// 建任务的前置核对：时间表不合法直接报错，同名或同内容的任务已存在返回工具错误。
/// 审批门之前先走一遍，作者不会为一条建不成的任务点确认。
pub async fn preflight(db: &mut SqliteConnection, s: &Value, args: &Value) -> Result<Option<Value>, String> {
    // 同一操作编号的重放由 create 返回回执，不算重复
    let op = args["operation_id"].as_str().unwrap_or("");
    if !op.is_empty() && get(db, OP_NS, &format!("automation-op:{}:{op}", str_field(s, "id")?)).await?.is_some() {
        return Ok(None);
    }
    validate_schedule(&args["schedule"])?;
    let title = args["title"].as_str().unwrap_or("").trim();
    let instruction = args["instruction"].as_str().unwrap_or("").trim();
    for existing in list(db, NS, TASK).await? {
        if existing["title"] == title || existing["instruction"] == instruction {
            return Ok(Some(json!({"ok":false,"toolError":{"code":"duplicate_task","retryable":false,
                "message":format!("已有同名或同内容的自动任务「{}」（{}），不重复建立；要改就先删掉它", existing["title"].as_str().unwrap_or(""), describe_schedule(&existing["schedule"]))},
                "existing":{"id":existing["id"],"title":existing["title"],"schedule":describe_schedule(&existing["schedule"])}})));
        }
    }
    Ok(None)
}

/// 自动任务页：启用 / 停用、改投递通道、改活跃时段。重新启用清掉失败计数并从现在起算下一次
pub async fn update_task(db: &mut SqliteConnection, id: &str, patch: &Value) -> Result<Value, String> {
    let mut task = get_task(db, id).await?.ok_or("自动任务不存在")?;
    if let Some(enabled) = patch["enabled"].as_bool() {
        if enabled && task["enabled"] != true {
            task["failures"] = json!(0);
            task["disabledReason"] = json!("");
            task["nextRunAt"] = json!(next_run_at(&task["schedule"], now()));
        }
        task["enabled"] = json!(enabled);
    }
    if let Some(deliver) = patch["deliver"].as_str() {
        if !delivery::channel_valid(deliver) {
            return Err(format!("未知的投递通道：{deliver}"));
        }
        task["deliver"] = json!(deliver);
    }
    if patch["activeHours"].is_array() {
        let hours = patch["activeHours"].as_array().unwrap();
        let (Some(start), Some(end)) = (hours.first().and_then(Value::as_u64), hours.get(1).and_then(Value::as_u64)) else {
            return Err("活跃时段要给起止小时".into());
        };
        if start > 23 || end > 24 {
            return Err("活跃时段的小时超出范围".into());
        }
        task["activeHours"] = json!([start, end]);
    }
    save_task(db, &task).await?;
    Ok(task)
}

pub async fn delete_task(db: &mut SqliteConnection, id: &str) -> Result<bool, String> {
    let Some(task) = get_task(db, id).await? else { return Ok(false) };
    if task["builtin"] == true {
        return Err("助理巡检是内置任务，只能停用不能删除".into());
    }
    delete_by_prefix(db, NS, &task_key(id)).await?;
    delete_by_prefix(db, NS, &format!("{RUN}{id}:")).await?;
    Ok(true)
}

pub async fn runs(db: &mut SqliteConnection, id: &str, limit: usize) -> Result<Vec<Value>, String> {
    let mut out = list(db, NS, &format!("{RUN}{id}:")).await?;
    out.sort_by_key(|r| -r["startedAt"].as_i64().unwrap_or(0));
    out.truncate(limit);
    Ok(out)
}

async fn record_run(db: &mut SqliteConnection, run: &Value) -> Result<(), String> {
    let task_id = str_field(run, "taskId")?;
    put(db, NS, &format!("{RUN}{task_id}:{}", run["startedAt"].as_i64().unwrap_or(0)), run).await?;
    let mut history = list(db, NS, &format!("{RUN}{task_id}:")).await?;
    history.sort_by_key(|r| r["startedAt"].as_i64().unwrap_or(0));
    while history.len() > KEEP_RUNS {
        let old = history.remove(0);
        delete_by_prefix(db, NS, &format!("{RUN}{task_id}:{}", old["startedAt"].as_i64().unwrap_or(0))).await?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 事件规则
// ---------------------------------------------------------------------------

/// 全部规则：没有记录的按默认值建立；旧记录缺的参数按默认值补上，说明文字以代码里的为准
pub async fn rules(db: &mut SqliteConnection) -> Result<Vec<Value>, String> {
    let mut out = Vec::new();
    for (event, label, deliver, param) in EVENT_RULES {
        let key = format!("{RULE}{event}");
        let mut rule = get(db, NS, &key).await?.unwrap_or_else(|| json!({"event":event,"enabled":true,"deliver":deliver}));
        let mut changed = rule["label"] != label;
        rule["label"] = json!(label);
        if let Some((name, default, min, max)) = param {
            if rule["params"][name].as_i64().is_none() {
                rule["params"][name] = json!(default);
                changed = true;
            }
            rule["paramRange"] = json!({"name":name,"min":min,"max":max,"default":default});
        }
        if changed || get(db, NS, &key).await?.is_none() {
            put(db, NS, &key, &rule).await?;
        }
        out.push(rule);
    }
    Ok(out)
}

pub async fn rule_update(db: &mut SqliteConnection, event: &str, patch: &Value) -> Result<Value, String> {
    let mut rule = rules(db).await?.into_iter().find(|r| r["event"] == event).ok_or("没有这条事件规则")?;
    if let Some(enabled) = patch["enabled"].as_bool() {
        rule["enabled"] = json!(enabled);
    }
    if let Some(deliver) = patch["deliver"].as_str() {
        if !delivery::channel_valid(deliver) {
            return Err(format!("未知的投递通道：{deliver}"));
        }
        rule["deliver"] = json!(deliver);
    }
    if patch["params"].is_object() {
        let range = EVENT_RULES.iter().find(|(e, ..)| *e == event).and_then(|(_, _, _, p)| *p).ok_or("这条规则没有可调的数字")?;
        let (name, _, min, max) = range;
        let value = patch["params"][name].as_i64().ok_or_else(|| format!("缺少 {name}"))?;
        if value < min || value > max {
            return Err(format!("{name} 要在 {min} 到 {max} 之间"));
        }
        rule["params"][name] = json!(value);
    }
    put(db, NS, &format!("{RULE}{event}"), &rule).await?;
    Ok(rule)
}

/// 周期检查：每小时一次、只在提醒时段内；`force` 时立刻查、不看时段。告警按各自规则的送达方式进投递队列。
pub fn run_checks(app: &crate::DesktopAppHandle, force: bool) -> Result<Value, String> {
    let root = crate::desktop_storage::root(app)?;
    let hour = Local::now().hour();
    let outcome = crate::blocking::run(async {
        let mut db = crate::desktop_storage::connect_existing_at(&root).await?;
        let mut settings = settings(&mut db).await?;
        let now_ms = now();
        if !force {
            let last = settings["checks"]["checkedAt"].as_i64().unwrap_or(0);
            if now_ms - last < CHECK_INTERVAL_MS || !crate::agent_heartbeat::within_active_hours(&settings, hour) {
                return Ok::<Option<(crate::agent_heartbeat::Report, Vec<Value>)>, String>(None);
            }
        }
        let rules = rules(&mut db).await?;
        let report = crate::agent_heartbeat::run(&mut db, &rules, &settings["checks"], now_ms).await?;
        settings["checks"] = report.state.clone();
        put(&mut db, NS, SETTINGS, &settings).await?;
        for alert in &report.alerts {
            let channel = rules.iter().find(|r| r["event"] == alert.event).and_then(|r| r["deliver"].as_str()).unwrap_or(delivery::DESKTOP);
            delivery::enqueue(&mut db, channel, &alert.text, &alert.key, json!({"event":alert.event})).await?;
        }
        Ok(Some((report, rules)))
    })?;
    let Some((report, _)) = outcome else { return Ok(json!({"skipped":true})) };
    for action in &report.actions {
        if *action == "index_now" {
            crate::agent_host::schedule_index(app.clone(), std::time::Duration::from_millis(0));
        }
    }
    if !report.alerts.is_empty() {
        delivery::flush_soon(app, "");
    }
    Ok(json!({"alerts":report.alerts.len(),"actions":report.actions}))
}

async fn rule(db: &mut SqliteConnection, event: &str) -> Result<Option<Value>, String> {
    Ok(rules(db).await?.into_iter().find(|r| r["event"] == event && r["enabled"] == true))
}

/// 宿主事件 → 投递：批量连载与拆书任务的结束状态，按规则表决定发不发、发到哪
pub fn on_event(app: &crate::DesktopAppHandle, name: &str, payload: &Value) -> Result<(), String> {
    let (event, key, text) = match name {
        "agent-batch" if payload["id"].is_string() && matches!(payload["status"].as_str(), Some("completed") | Some("failed")) => {
            let title = payload["bookTitle"].as_str().unwrap_or("批量连载");
            let total = payload["queue"].as_array().map_or(0, Vec::len);
            let done = payload["queue"].as_array().into_iter().flatten().filter(|q| q["status"] == "done" || q["status"] == "completed").count();
            let text = if payload["status"] == "completed" {
                format!("批量连载「{title}」跑完了：{done}/{total} 章，候选在作品面板里等你审阅。")
            } else {
                format!("批量连载「{title}」中断在第 {} 步：{}。", payload["cursor"].as_u64().unwrap_or(0) + 1, payload["error"].as_str().unwrap_or("未知原因"))
            };
            ("batch_finished", format!("batch:{}:{}", payload["id"].as_str().unwrap_or(""), payload["status"].as_str().unwrap_or("")), text)
        }
        "agent-breakdown" if payload["projectId"].is_string() && payload["status"] == "completed" => (
            "breakdown_finished",
            format!("breakdown:{}:{}", payload["projectId"].as_str().unwrap_or(""), payload["updatedAt"].as_i64().unwrap_or(0)),
            format!("拆书任务「{}」已完成，报告可以看了。", payload["title"].as_str().unwrap_or("")),
        ),
        _ => return Ok(()),
    };
    let root = crate::desktop_storage::root(app)?;
    let queued = crate::blocking::run(async {
        let mut db = crate::desktop_storage::connect_existing_at(&root).await?;
        let Some(rule) = rule(&mut db, event).await? else { return Ok::<Option<Value>, String>(None) };
        delivery::enqueue(&mut db, rule["deliver"].as_str().unwrap_or(delivery::NONE), &text, &key, json!({"event":event})).await
    })?;
    if queued.is_some() {
        delivery::flush_soon(app, "");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 运行
// ---------------------------------------------------------------------------

fn running() -> &'static Mutex<HashSet<String>> {
    static RUNNING: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    RUNNING.get_or_init(|| Mutex::new(HashSet::new()))
}

fn subscription() -> &'static Mutex<Option<crate::agent_events::Subscription>> {
    static SUB: OnceLock<Mutex<Option<crate::agent_events::Subscription>>> = OnceLock::new();
    SUB.get_or_init(|| Mutex::new(None))
}

/// 随应用启动：订阅宿主事件，起一条后台线程每半分钟看一次到点的任务与待发的投递
pub fn start(app: crate::DesktopAppHandle) {
    let handle = app.clone();
    let sub = crate::agent_events::subscribe(Arc::new(move |name: &str, payload: &Value| {
        if name != "agent-batch" && name != "agent-breakdown" {
            return;
        }
        let app = handle.clone();
        let name = name.to_owned();
        let payload = payload.clone();
        std::thread::spawn(move || {
            if let Err(error) = on_event(&app, &name, &payload) {
                eprintln!("[scheduler] 事件规则处理失败：{error}");
            }
        });
    }));
    *subscription().lock().unwrap() = Some(sub);
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(20));
        loop {
            let host = app.state::<crate::agent_host::AgentHost>();
            if host.quitting.load(Ordering::SeqCst) {
                return;
            }
            if host.storage_ready.load(Ordering::SeqCst) {
                if let Err(error) = tick(&app) {
                    eprintln!("[scheduler] 调度失败：{error}");
                }
            }
            std::thread::sleep(std::time::Duration::from_secs(TICK_SECS));
        }
    });
}

/// 到点的任务（按预定时间先后）
async fn due_tasks(db: &mut SqliteConnection, now_ms: i64) -> Result<Vec<Value>, String> {
    let mut due: Vec<Value> = list_tasks(db)
        .await?
        .into_iter()
        .filter(|t| t["enabled"] == true && t["nextRunAt"].as_i64().is_some_and(|at| at <= now_ms))
        .collect();
    due.sort_by_key(|t| t["nextRunAt"].as_i64().unwrap_or(0));
    Ok(due)
}

fn tick(app: &crate::DesktopAppHandle) -> Result<(), String> {
    let root = crate::desktop_storage::root(app)?;
    let _ = delivery::flush(app, "");
    if let Err(error) = run_checks(app, false) {
        eprintln!("[scheduler] 周期检查失败：{error}");
    }
    let due = crate::blocking::run(async {
        let mut db = crate::desktop_storage::connect_existing_at(&root).await?;
        due_tasks(&mut db, now()).await
    })?;
    for task in due {
        let id = str_field(&task, "id")?;
        if let Err(error) = run_task(app, id) {
            eprintln!("[scheduler] 任务 {id} 运行失败：{error}");
        }
    }
    Ok(())
}

/// 自动任务页的"立即运行"：后台跑，不等；在跑的不重复起
pub fn run_now(app: &crate::DesktopAppHandle, id: &str) -> Result<(), String> {
    if running().lock().unwrap().contains(id) {
        return Err("这个任务正在运行".into());
    }
    let app = app.clone();
    let id = id.to_owned();
    std::thread::spawn(move || {
        if let Err(error) = run_task(&app, &id) {
            eprintln!("[scheduler] 任务 {id} 运行失败：{error}");
        }
    });
    Ok(())
}

/// 错过的时间点合并成一次补跑：从预定时间数到现在跳过了几个点
pub(crate) fn missed_slots(schedule: &Value, scheduled_at: i64, now_ms: i64) -> i64 {
    let mut at = scheduled_at;
    let mut missed = 0;
    while let Some(next) = next_run_at(schedule, at) {
        if next > now_ms || missed >= 1000 {
            break;
        }
        missed += 1;
        at = next;
    }
    missed
}

/// 跑一次任务：记录运行、投递结果、更新任务状态（下一次时间、失败计数、自动停用）
pub fn run_task(app: &crate::DesktopAppHandle, id: &str) -> Result<Value, String> {
    if !running().lock().unwrap().insert(id.to_owned()) {
        return Err("这个任务正在运行".into());
    }
    let outcome = run_task_inner(app, id);
    running().lock().unwrap().remove(id);
    outcome
}

fn run_task_inner(app: &crate::DesktopAppHandle, id: &str) -> Result<Value, String> {
    let root = crate::desktop_storage::root(app)?;
    let task = crate::blocking::run(async {
        let mut db = crate::desktop_storage::connect_existing_at(&root).await?;
        get_task(&mut db, id).await?.ok_or_else(|| "自动任务不存在".to_owned())
    })?;
    let started = now();
    let merged = task["nextRunAt"].as_i64().map(|at| missed_slots(&task["schedule"], at, started)).unwrap_or(0);
    let run_id = id_short();
    let result: Result<(&'static str, String, Value), String> = match task["kind"].as_str().unwrap_or("") {
        "agent" => run_agent(app, &root, &task, &run_id),
        "command" => run_command(app, &task),
        other => Err(format!("未知的任务类型：{other}")),
    };
    let finished = now();
    let (status, summary, error, session_id) = match &result {
        Ok((status, summary, extra)) => (*status, summary.clone(), String::new(), extra["sessionId"].clone()),
        Err(error) => (if error.contains("超过运行时限") { "timeout" } else { "failed" }, String::new(), error.clone(), Value::Null),
    };
    crate::blocking::run(async {
        let mut db = crate::desktop_storage::connect_existing_at(&root).await?;
        let mut task = get_task(&mut db, id).await?.ok_or("自动任务不存在")?;
        record_run(&mut db, &json!({"taskId":id,"runId":run_id,"startedAt":started,"finishedAt":finished,"status":status,
            "error":error,"summary":summary,"merged":merged,"sessionId":session_id})).await?;
        if let Ok((_, _, extra)) = &result {
            if extra["state"].is_object() {
                task["state"] = extra["state"].clone();
            }
        }
        task["lastRunAt"] = json!(finished);
        task["lastStatus"] = json!(status);
        task["lastError"] = json!(error);
        task["lastSummary"] = json!(summary.chars().take(200).collect::<String>());
        let failed = matches!(status, "failed" | "timeout");
        task["failures"] = json!(if failed { task["failures"].as_i64().unwrap_or(0) + 1 } else { 0 });
        // 投递：结果、等待作者的问题、连续失败的停用通知
        let deliver = task["deliver"].as_str().unwrap_or(delivery::NONE).to_owned();
        let title = task["title"].as_str().unwrap_or("").to_owned();
        if !failed && !summary.is_empty() && status != "silent" {
            let text = format!("【{title}】\n{summary}");
            delivery::enqueue(&mut db, &deliver, &text, &format!("task:{id}:{started}"), json!({"taskId":id,"runId":run_id,"sessionId":session_id})).await?;
        }
        if task["failures"].as_i64().unwrap_or(0) >= MAX_FAILURES {
            task["enabled"] = json!(false);
            task["disabledReason"] = json!(format!("连续 {MAX_FAILURES} 次失败，已自动停用：{error}"));
            let channel = if deliver == delivery::NONE { delivery::DESKTOP } else { deliver.as_str() };
            delivery::enqueue(&mut db, channel, &format!("自动任务「{title}」连续 {MAX_FAILURES} 次失败，已停用：{error}。到 Agent 板块的自动任务页可以重新启用。"),
                &format!("task-disabled:{id}:{finished}"), json!({"taskId":id})).await?;
        }
        if task["schedule"]["type"] == "once" {
            task["enabled"] = json!(false);
            task["nextRunAt"] = Value::Null;
        } else {
            task["nextRunAt"] = json!(next_run_at(&task["schedule"], finished));
        }
        save_task(&mut db, &task).await?;
        crate::agent_events::emit(app, "agent-automation", &task);
        Ok::<_, String>(task)
    })?;
    delivery::flush_soon(app, "");
    result.map(|(status, summary, _)| json!({"status":status,"summary":summary}))
}

fn id_short() -> String {
    id().chars().take(8).collect()
}

/// 隔离的 Agent 回合：job:<id> 会话里跑一轮；最终回复就是要投递的内容；停在问题上算等待；超时由看门狗停止
fn run_agent(app: &crate::DesktopAppHandle, root: &std::path::Path, task: &Value, run_id: &str) -> Result<(&'static str, String, Value), String> {
    let task_id = str_field(task, "id")?;
    let title = task["title"].as_str().unwrap_or("").to_owned();
    let channel = if task["deliver"] == delivery::WEIXIN { delivery::WEIXIN } else { delivery::DESKTOP };
    let references = match task["bookId"].as_str().filter(|v| !v.is_empty()) {
        Some(bid) => {
            let book = crate::blocking::run(async {
                let mut db = crate::desktop_storage::connect_existing_at(root).await?;
                book(&mut db, bid).await
            })?;
            json!([{"id":format!("book:{bid}"),"bookId":bid,"kind":"book","title":book["title"]}])
        }
        None => json!([]),
    };
    let session = crate::blocking::run(crate::agent_service::command(
        root,
        "create_session",
        &json!({"requestId":format!("job:{task_id}"),"title":format!("自动任务 · {title}"),"channel":channel,"references":references}),
    ))?;
    let sid = str_field(&session, "id")?.to_owned();
    if session["status"] == "running" {
        return Err("上一次运行还没结束".into());
    }
    let args = json!({"sessionId":sid,"inputKind":"message","text":task["instruction"],"modelCode":task["modelCode"],
        "references":references,"automation":{"taskId":task_id,"title":title,"runId":run_id}});
    // 看门狗：超过墙钟上限就停掉这轮
    let finished = Arc::new(AtomicBool::new(false));
    let timed_out = Arc::new(AtomicBool::new(false));
    {
        let (finished, timed_out, app, sid) = (finished.clone(), timed_out.clone(), app.clone(), sid.clone());
        let limit = task["maxRunSeconds"].as_u64().unwrap_or(DEFAULT_MAX_RUN_SECS as u64);
        std::thread::spawn(move || {
            let step = std::time::Duration::from_secs(1);
            for _ in 0..limit {
                std::thread::sleep(step);
                if finished.load(Ordering::SeqCst) {
                    return;
                }
            }
            timed_out.store(true, Ordering::SeqCst);
            app.state::<crate::agent_host::AgentHost>().stop(&sid);
        });
    }
    let outcome = crate::agent_host::run_unattended(app, root, &sid, args);
    finished.store(true, Ordering::SeqCst);
    let session = outcome?;
    if timed_out.load(Ordering::SeqCst) {
        return Err(format!("超过运行时限 {} 秒，已停止", task["maxRunSeconds"].as_u64().unwrap_or(DEFAULT_MAX_RUN_SECS as u64)));
    }
    let run = session["runId"].as_str().unwrap_or("").to_owned();
    let reply = session["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|m| m["runId"] == run && m["role"] == "assistant" && m["kind"] != "choice")
        .filter_map(|m| m["text"].as_str().map(str::trim).filter(|t| !t.is_empty()))
        .collect::<Vec<_>>()
        .join("\n\n");
    let extra = json!({"sessionId":sid});
    match session["status"].as_str().unwrap_or("") {
        "completed" => Ok(("ok", reply, extra)),
        "choice" => {
            let question = session["messages"]
                .as_array()
                .into_iter()
                .flatten()
                .rev()
                .find(|m| m["kind"] == "choice" && m["choice"]["status"] == "pending")
                .and_then(|m| m["text"].as_str())
                .unwrap_or("")
                .to_owned();
            Ok(("waiting", format!("任务停在一个需要你回答的问题上：{question}\n到 Agent 板块的「自动任务 · {title}」会话里回答后它才会继续。"), extra))
        }
        "paused" => Err("运行被停止".into()),
        _ => Err(session["error"].as_str().unwrap_or("运行失败").to_owned()),
    }
}

/// 宿主命令：现在只有"立刻整理索引"
fn run_command(app: &crate::DesktopAppHandle, task: &Value) -> Result<(&'static str, String, Value), String> {
    match task["command"]["name"].as_str().unwrap_or("") {
        "index_now" => {
            crate::agent_host::schedule_index(app.clone(), std::time::Duration::from_millis(0));
            Ok(("ok", String::new(), json!({})))
        }
        other => Err(format!("未知的宿主命令：{other}")),
    }
}
