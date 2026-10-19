//! 拆书 2.0：参考书按章产出与作者作品同一套章节卡，聚合成书级画像，供节奏对比、案例检索与仿书使用。
//!
//! 参考书原文与拆书报告在拆书库（`ew-local-breakdown/kv`）里；章节卡、事实、线索这些派生数据沿用状态层，
//! 作品编号用 `breakdown:{项目}` 前缀，与作者作品完全隔开。参考书的人物名在作者作品的草案、资料、
//! 对照方案里出现时由宿主拦下：参考书只能借方法与结构，不能借人物和事件。
//!
//! Agent 自己就能完成一次拆书：输入框导入整本书（`import_book`），按区间建拆书任务逐章提取章节卡
//! （`agent_breakdown_job`），人名单从章节卡汇总，不依赖拆书工作台。
use crate::agent_canon as canon;
use crate::agent_store::{err, get, id, list, now, put, str_field, NS};
use serde_json::{json, Value};
use sqlx::{Row, SqliteConnection};
use std::collections::{BTreeMap, BTreeSet};

pub const BREAKDOWN_NS: &str = "ew-local-breakdown/kv";
pub const PREFIX: &str = "breakdown:";

/// 常见称谓：出现在人物关系里也不当作参考书的专有人名，避免误拦作者自己的“师父”“长老”。
const COMMON_TITLES: &[&str] = &[
    "师父", "师傅", "师尊", "师兄", "师姐", "师弟", "师妹", "长老", "掌门", "宗主", "父亲", "母亲", "爸爸", "妈妈",
    "爷爷", "奶奶", "哥哥", "姐姐", "弟弟", "妹妹", "皇帝", "公主", "王爷", "少爷", "小姐", "老板", "主角", "反派",
    "众人", "路人", "管家", "侍卫", "老者", "少年", "少女", "男主", "女主", "系统", "黑衣人", "老人", "中年人",
    "青年", "女子", "男子", "少妇", "孩子", "小孩", "众弟子", "弟子", "掌柜", "伙计", "护卫", "将军", "大人",
    "陛下", "殿下", "夫人", "老爷", "先生", "医生", "老师", "同学", "队长", "敌人", "对手", "神秘人",
];

fn is_proper_name(name: &str) -> bool {
    name.chars().count() >= 2 && !COMMON_TITLES.contains(&name)
}

pub fn project_of(book_id: &str) -> Option<&str> {
    book_id.strip_prefix(PREFIX)
}

pub fn book_id(project_id: &str) -> String {
    format!("{PREFIX}{project_id}")
}

pub async fn project(db: &mut SqliteConnection, pid: &str) -> Result<Value, String> {
    get(db, BREAKDOWN_NS, &format!("project:{pid}"))
        .await?
        .ok_or_else(|| "拆书项目不存在".to_owned())
}

/// 参考书目录，按原书顺序；形状与作者作品目录一致（有 id、title）。
pub async fn catalog(db: &mut SqliteConnection, pid: &str) -> Result<Vec<Value>, String> {
    let mut chapters = project(db, pid).await?["chapters"].as_array().cloned().unwrap_or_default();
    chapters.sort_by_key(|c| c["sortNo"].as_i64().unwrap_or(0));
    Ok(chapters)
}

/// 作者作品或参考书的目录。
pub async fn any_catalog(db: &mut SqliteConnection, book_id: &str) -> Result<Vec<Value>, String> {
    match project_of(book_id) {
        Some(pid) => catalog(db, pid).await,
        None => crate::agent_store::catalog(db, book_id).await,
    }
}

/// 参考书一章：标题与正文（段落以换行连接）。
pub async fn chapter(db: &mut SqliteConnection, pid: &str, cid: &str) -> Result<Value, String> {
    let item = catalog(db, pid)
        .await?
        .into_iter()
        .find(|c| c["id"].to_string() == cid)
        .ok_or("参考书章节不存在")?;
    let content = get(db, BREAKDOWN_NS, &format!("content:{cid}"))
        .await?
        .ok_or("参考书章节正文不存在")?;
    let text = content["paragraphs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join("\n");
    Ok(json!({"id":item["id"],"title":item["title"],"sortNo":item["sortNo"],"textContent":text}))
}

/// 交给模型的参考书目录一次最多列这么多章；更长的书按区间翻。
const MODEL_CATALOG_LIMIT: usize = 300;

/// 参考书目录（给模型）：编号、标题、字数、已拆与否；`from`/`to` 按目录位置计，`to` 为 0 表示到末尾，最多列 300 章。
pub async fn catalog_for_model(db: &mut SqliteConnection, pid: &str, from: i64, to: i64) -> Result<Value, String> {
    let p = project(db, pid).await?;
    let chapters = catalog(db, pid).await?;
    let total = chapters.len();
    let indexed = active_card_chapters(db, &book_id(pid)).await?;
    let start = (from.max(1) as usize).min(total.max(1));
    let requested_end = if to <= 0 { total } else { (to as usize).min(total) };
    let end = requested_end.min(start + MODEL_CATALOG_LIMIT - 1).max(start.min(total));
    let items = chapters
        .iter()
        .enumerate()
        .skip(start - 1)
        .take(end + 1 - start)
        .map(|(i, c)| json!({"no":i + 1,"chapterId":c["id"].to_string(),"title":c["title"],"wordCount":c["wordCount"],
            "indexed":indexed.contains(&c["id"].to_string())}))
        .collect::<Vec<_>>();
    let note = if end < requested_end {
        format!("共 {total} 章，这里列第 {start}–{end} 章；后面的用 from_chapter 接着列，或用 search_book 按内容找")
    } else {
        format!("共 {total} 章")
    };
    Ok(json!({"bookId":book_id(pid),"title":p["title"],"reference":true,"total":total,"from":start,"to":end,"chapters":items,
        "note":format!("{note}。参考书只读：可以读原文、检索、拆解分析，不能写入或当作作者作品的事实")}))
}

/// 参考书章节的来源指纹：标题与正文。
pub fn version_of(chapter: &Value) -> String {
    crate::agent_creation::version(&json!([chapter["title"], chapter["textContent"]]))
}

/// 参考书项目记录里的人物名（拆书工作台从人物关系汇总），去掉常见称谓与单字。
pub fn character_names(project: &Value) -> BTreeSet<String> {
    project["characterNames"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|n| is_proper_name(n))
        .map(str::to_owned)
        .collect()
}

/// 章节卡里出现过的参考书人物：名字 → 出现的章数，按章数从多到少。
async fn card_names(db: &mut SqliteConnection, pid: &str) -> Result<Vec<(String, i64)>, String> {
    let rows = sqlx::query(
        "SELECT name,COUNT(DISTINCT chapterId) AS n FROM canon_entities WHERE bookId=? AND entityType='character' GROUP BY name ORDER BY n DESC,name",
    )
    .bind(book_id(pid))
    .fetch_all(&mut *db)
    .await
    .map_err(err)?;
    Ok(rows
        .into_iter()
        .map(|r| (r.get::<String, _>("name").trim().to_owned(), r.get::<i64, _>("n")))
        .filter(|(name, _)| is_proper_name(name))
        .collect())
}

/// 提取下一章章节卡时给模型的人物表：项目记录里的人名在前，章节卡里出现过的按章数排在后面。
pub async fn known_names(db: &mut SqliteConnection, pid: &str, limit: usize) -> Result<Vec<String>, String> {
    let mut names: Vec<String> = match project(db, pid).await {
        Ok(p) => p["characterNames"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|n| is_proper_name(n))
            .map(str::to_owned)
            .collect(),
        Err(_) => Vec::new(),
    };
    for (name, _) in card_names(db, pid).await? {
        if names.len() >= limit {
            break;
        }
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names.truncate(limit);
    Ok(names)
}

/// 防“借人物”拦截用的参考书人名单：项目记录里的人名，加上章节卡里至少两章出现的人名
/// （只拆了一两章时出现一次也算）。只出场一次的路人名不拦，免得误伤作者自己的取名。
pub async fn reference_names(db: &mut SqliteConnection, pid: &str) -> Result<BTreeSet<String>, String> {
    let mut names = match project(db, pid).await {
        Ok(p) => character_names(&p),
        Err(_) => BTreeSet::new(),
    };
    let indexed = active_card_chapters(db, &book_id(pid)).await?.len() as i64;
    let threshold = if indexed < 3 { 1 } else { 2 };
    names.extend(card_names(db, pid).await?.into_iter().filter(|(_, n)| *n >= threshold).map(|(name, _)| name));
    Ok(names)
}

/// 已有有效章节卡的章节编号。只读状态字段，不解析整张卡。
pub async fn active_card_chapters(db: &mut SqliteConnection, book: &str) -> Result<BTreeSet<String>, String> {
    let prefix = crate::desktop_storage::key_prefix(&canon::card_key(book, ""));
    let rows: Vec<Option<String>> = sqlx::query_scalar(
        "SELECT json_extract(value,'$.chapterId') FROM desktop_records WHERE namespace=? AND substr(key,1,length(?))=? AND json_extract(value,'$.status')='active'",
    )
    .bind(canon::CANON_NS)
    .bind(&prefix)
    .bind(&prefix)
    .fetch_all(&mut *db)
    .await
    .map_err(err)?;
    Ok(rows.into_iter().flatten().collect())
}

/// 文本里出现了哪些参考书人物名。
pub fn leaked_names(text: &str, names: &BTreeSet<String>) -> Vec<String> {
    names.iter().filter(|n| text.contains(n.as_str())).cloned().collect()
}

/// 本会话接触过的参考书人物名：会话引用的拆书项目 + 引用撤下后仍记着读过的（readSources）+ 本会话对照方案用过的项目。
pub async fn session_reference_names(db: &mut SqliteConnection, session: &Value) -> Result<BTreeSet<String>, String> {
    let mut projects: BTreeSet<String> = session["references"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| r["kind"] == "breakdown")
        .filter_map(|r| r["bookId"].as_str().and_then(project_of).map(str::to_owned))
        .chain(session["readSources"].as_array().into_iter().flatten().filter_map(|v| v.as_str().and_then(project_of).map(str::to_owned)))
        .collect();
    for artifact in list(db, crate::agent_store::NS, "artifact:").await? {
        if artifact["sessionId"] == session["id"] && artifact["kind"] == "comparison" {
            for pid in artifact["projectIds"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                projects.insert(pid.to_owned());
            }
        }
    }
    let mut names = BTreeSet::new();
    for pid in projects {
        if project(db, &pid).await.is_ok() {
            names.extend(reference_names(db, &pid).await?);
        }
    }
    Ok(names)
}

/// 参考书人物名混进作者作品时的工具错误；没有混入返回 None。
pub fn leak_error(text: &str, names: &BTreeSet<String>) -> Option<Value> {
    let leaked = leaked_names(text, names);
    (!leaked.is_empty()).then(|| {
        json!({"ok":false,"toolError":{"code":"reference_leak","retryable":true,"names":leaked,
            "message":format!("对标书的人物出现在作者作品的内容里：{}。参考书只能借方法和结构，换成作者自己的人物与设定后再保存", leaked.join("、"))}})
    })
}

// ---------------------------------------------------------------------------
// 节奏曲线：作者作品与参考书用同一套章节卡统计
// ---------------------------------------------------------------------------

/// 每章一个点：爽点、章末钩子、新信息、事件数、线索开合。没有有效章节卡的章节标 indexed=false。
pub async fn curve(db: &mut SqliteConnection, book_id: &str) -> Result<Vec<Value>, String> {
    let cards = canon::list_cards(db, book_id).await?;
    let mut points = Vec::new();
    for (index, chapter) in any_catalog(db, book_id).await?.iter().enumerate() {
        let cid = chapter["id"].to_string();
        let no = index as i64 + 1;
        let Some(card) = cards.iter().find(|c| c["chapterId"] == cid.as_str() && c["status"] == "active") else {
            points.push(json!({"no":no,"chapterId":cid,"title":chapter["title"],"indexed":false}));
            continue;
        };
        let c = &card["card"];
        let payoffs = c["payoffs"].as_array().cloned().unwrap_or_default();
        let hooks = c["hooks"].as_array().cloned().unwrap_or_default();
        let end_hook = hooks.iter().find(|h| h["position"] == "end");
        let threads = c["threads"].as_array().cloned().unwrap_or_default();
        points.push(json!({"no":no,"chapterId":cid,"title":chapter["title"],"indexed":true,"oneLine":c["oneLine"],
            "payoffCount":payoffs.len(),"payoffPeak":payoffs.iter().filter_map(|p| p["strength"].as_i64()).max().unwrap_or(0),
            "payoffTypes":payoffs.iter().filter_map(|p| p["type"].as_str()).collect::<Vec<_>>(),
            "endHook":end_hook.map(|h| json!({"type":h["type"],"strength":h["strength"]})),
            "hookTypes":hooks.iter().filter_map(|h| h["type"].as_str()).collect::<Vec<_>>(),
            "info":c["facts"].as_array().map_or(0, |a| a.len()),"events":c["events"].as_array().map_or(0, |a| a.len()),
            "planted":threads.iter().filter(|t| t["action"] == "plant").count(),
            "paid":threads.iter().filter(|t| t["action"] == "payoff").count(),
            "emotion":c["emotion"]}));
    }
    Ok(points)
}

fn count_types<'a>(items: impl Iterator<Item = &'a str>) -> Vec<Value> {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for item in items {
        *counts.entry(item).or_insert(0) += 1;
    }
    let mut out: Vec<(&str, usize)> = counts.into_iter().collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    out.into_iter().map(|(t, n)| json!({"type":t,"count":n})).collect()
}

/// 曲线的统计口径：只算已索引的章节。
pub fn summarize(points: &[Value]) -> Value {
    let indexed: Vec<&Value> = points.iter().filter(|p| p["indexed"] == true).collect();
    let n = indexed.len();
    let mut longest_gap = (0usize, 0i64, 0i64);
    let mut run = 0usize;
    let mut run_start = 0i64;
    let mut previous_no: Option<i64> = None;
    // 密度可以只统计已知章节；连续区间必须沿完整目录走，未知章节与编号断档都会打断区间。
    for p in points {
        let no = p["no"].as_i64();
        if previous_no.and_then(|n| n.checked_add(1)) != no { run = 0; }
        if p["indexed"] == true && no.is_some() && p["payoffCount"].as_u64() == Some(0) {
            let no = no.unwrap();
            if run == 0 { run_start = no; }
            run += 1;
            if run > longest_gap.0 { longest_gap = (run, run_start, no); }
        } else {
            run = 0;
        }
        previous_no = no;
    }
    let end_hooks: Vec<&str> = indexed.iter().filter_map(|p| p["endHook"]["type"].as_str()).collect();
    let payoffs: usize = indexed.iter().map(|p| p["payoffCount"].as_u64().unwrap_or(0) as usize).sum();
    let info: usize = indexed.iter().map(|p| p["info"].as_u64().unwrap_or(0) as usize).sum();
    let ratio = |a: usize| if n == 0 { 0.0 } else { (a as f64 / n as f64 * 100.0).round() / 100.0 };
    json!({"indexedChapters":n,"totalChapters":points.len(),"payoffsPerChapter":ratio(payoffs),"infoPerChapter":ratio(info),
        "endHookRate":ratio(end_hooks.len()),"longestPayoffGap":{"length":longest_gap.0,"from":longest_gap.1,"to":longest_gap.2},
        "endHookTypes":count_types(end_hooks.iter().copied()),
        "payoffTypes":count_types(indexed.iter().flat_map(|p| p["payoffTypes"].as_array().into_iter().flatten().filter_map(Value::as_str)))})
}

/// Source linkage comes from durable jobs and adopted artifacts, never from
/// matching a book/skill title or guessing a skill ID from an artifact key.
async fn distillation_styles(db: &mut SqliteConnection, pid: &str) -> Result<Vec<Value>,String> {
    let rows:Vec<String>=sqlx::query_scalar("SELECT value FROM desktop_records WHERE namespace=? AND key LIKE '%distillation-job:%' AND json_extract(value,'$.projectId')=? AND json_extract(value,'$.status')='completed' ORDER BY json_extract(value,'$.updatedAt') DESC")
        .bind(NS).bind(pid).fetch_all(&mut *db).await.map_err(err)?;
    let mut styles=Vec::new();
    for row in rows {
        let job:Value=serde_json::from_str(&row).map_err(err)?;
        let Some(aid)=job["artifactId"].as_str() else {continue;};
        let Some(artifact)=get(db,NS,&format!("artifact:{aid}")).await? else {continue;};
        if artifact["skillDraft"]["kind"]!="style" {continue;}
        let mut entry=json!({"jobId":job["id"],"artifactId":aid,"title":artifact["title"],"scope":job["scope"],"state":"candidate"});
        if artifact["applied"]==true {
            let Some(skill_id)=artifact["installedSkillId"].as_str() else {continue;};
            let Some(skill)=get(db,NS,&format!("skill:{skill_id}")).await? else {continue;};
            if skill["deletedAt"].is_number() || skill["kind"]!="style" {continue;}
            entry["state"]=json!(if skill["enabled"]==true{"installed"}else{"disabled"});
            entry["skillId"]=json!(skill_id);
            entry["title"]=skill["name"].clone();
        }
        styles.push(entry);
    }
    Ok(styles)
}

/// 书级画像：节奏曲线、类型分布、开篇模式、线索开合、文风数据、拆书报告里的阶段与技巧、已存的配方。
pub async fn profile(db: &mut SqliteConnection, pid: &str) -> Result<Value, String> {
    let p = project(db, pid).await?;
    let bid = book_id(pid);
    let points = curve(db, &bid).await?;
    let opening = points
        .iter()
        .filter(|pt| pt["indexed"] == true)
        .take(3)
        .map(|pt| json!({"no":pt["no"],"title":pt["title"],"oneLine":pt["oneLine"],"hookTypes":pt["hookTypes"],"payoffTypes":pt["payoffTypes"],"endHook":pt["endHook"]}))
        .collect::<Vec<_>>();
    let threads = canon::list_threads(db, &bid, None).await?;
    let order: BTreeMap<String, i64> = points
        .iter()
        .filter_map(|pt| Some((pt["chapterId"].as_str()?.to_owned(), pt["no"].as_i64()?)))
        .collect();
    let distances: Vec<i64> = threads
        .iter()
        .filter(|t| t["status"] == "paid")
        .filter_map(|t| Some(order.get(t["paidChapter"].as_str()?)? - order.get(t["plantedChapter"].as_str()?)?))
        .collect();
    // 文风统计与已索引范围一致，并单独声明其来源；统计不代表本轮模型阅读。
    let mut sample = String::new();
    let indexed: std::collections::BTreeSet<String> = points.iter().filter(|p|p["indexed"]==true).filter_map(|p|p["chapterId"].as_str().map(str::to_owned)).collect();
    for chapter in catalog(db, pid).await?.iter().filter(|c|indexed.contains(&c["id"].to_string())) {
        if let Ok(c) = self::chapter(db, pid, &chapter["id"].to_string()).await {
            sample.push_str(c["textContent"].as_str().unwrap_or(""));
            sample.push('\n');
        }
    }
    // 阶段、技巧、点评：Agent 写过拆书报告就用最新一份，否则沿用拆书工作台的全书报告
    let agent_report = latest_report(db, pid).await?;
    let report = if agent_report.is_object() {
        json!({"outlineRecovery":agent_report["stages"],"reusableTechniques":agent_report["techniques"],"editorNotes":agent_report["overview"]})
    } else {
        p["report"].clone()
    };
    Ok(json!({"projectId":pid,"title":p["title"],"summary":summarize(&points),"curve":points,"opening":opening,
        "threads":{"planted":threads.len(),"paid":threads.iter().filter(|t| t["status"] == "paid").count(),
            "averagePayoffDistance":if distances.is_empty() { Value::Null } else { json!(distances.iter().sum::<i64>() as f64 / distances.len() as f64) }},
        "style":crate::agent_style::metrics(&sample),
        "styleCoverage":{"basis":"raw_text_statistics","chapterIds":indexed,"chapters":indexed.len(),"note":"程序对这些章节原文计算的统计，不代表本轮模型读过原文，也不等于报告范围"},
        "stages":report["outlineRecovery"],"techniques":report["reusableTechniques"],"editorNotes":report["editorNotes"],
        "reportRange":if agent_report.is_object() { json!({"from":agent_report["from"],"to":agent_report["to"],"artifactId":agent_report["artifactId"]}) } else { Value::Null },
        "recipes":recipes(db, pid).await?,
        "distillationStyles":distillation_styles(db,pid).await?,
        "job":crate::agent_breakdown_job::view(db, pid).await?,
        "note":"节奏、钩子与爽点来自已索引章节卡；文风统计的原文范围见 styleCoverage，程序统计不代表本轮模型阅读。画像不完整时如实说明。stages、techniques、editorNotes 来自最近一份拆书报告（reportRange 是它覆盖的章节），没写过报告时为空。参考书的人物和事件只能借方法，不能进入作者作品"}))
}

/// 交给模型时逐章曲线最多保留的点数；更长的书按区间汇总，避免几百章的曲线把上下文撑爆。
const MODEL_CURVE_POINTS: usize = 40;

/// 把逐章曲线压成区间汇总：每段的已索引章数、爽点数与峰值、章末钩子数、线索开合。
pub fn compact_curve(points: &[Value]) -> Value {
    if points.len() <= MODEL_CURVE_POINTS {
        return json!(points);
    }
    let size = points.len().div_ceil(MODEL_CURVE_POINTS - 10);
    let mut segments = Vec::new();
    for chunk in points.chunks(size) {
        let indexed: Vec<&Value> = chunk.iter().filter(|p| p["indexed"] == true).collect();
        let sum = |key: &str| indexed.iter().map(|p| p[key].as_u64().unwrap_or(0)).sum::<u64>();
        segments.push(json!({"from":chunk[0]["no"],"to":chunk[chunk.len() - 1]["no"],"indexed":indexed.len(),
            "payoffs":sum("payoffCount"),"payoffPeak":indexed.iter().filter_map(|p| p["payoffPeak"].as_i64()).max().unwrap_or(0),
            "endHooks":indexed.iter().filter(|p| p["endHook"].is_object()).count(),"planted":sum("planted"),"paid":sum("paid")}));
    }
    json!({"note":format!("共 {} 章，按每 {size} 章汇总；单章详情用 search_breakdown_cases 检索", points.len()),
        "firstChapters":points.iter().take(10).collect::<Vec<_>>(),"segments":segments})
}

/// 画像交给模型的版本：逐章曲线压成区间汇总，其余不变。
pub fn profile_for_model(mut profile: Value) -> Value {
    if let Some(points) = profile["curve"].as_array().cloned() {
        profile["curve"] = compact_curve(&points);
    }
    profile
}

/// 节奏对比交给模型的版本：两条曲线都压成区间汇总。
pub fn compare_for_model(mut result: Value) -> Value {
    for side in ["mine", "benchmark"] {
        if let Some(points) = result[side]["curve"].as_array().cloned() {
            result[side]["curve"] = compact_curve(&points);
        }
    }
    result
}

/// 作者作品与参考书的节奏对比，附可直接说给作者听的诊断。
pub async fn compare(db: &mut SqliteConnection, book_id_value: &str, pid: &str) -> Result<Value, String> {
    let mine = curve(db, book_id_value).await?;
    let theirs = curve(db, &book_id(pid)).await?;
    let (a, b) = (summarize(&mine), summarize(&theirs));
    let mut findings = Vec::new();
    if a["indexedChapters"] == 0 {
        findings.push("你的作品还没有章节卡，先在记忆页“作品状态”补建索引再对比。".to_owned());
    }
    if b["indexedChapters"] == 0 {
        findings.push("参考书还没有章节卡，先用拆书任务（start_breakdown）拆出要对比的章节再对比。".to_owned());
    }
    if findings.is_empty() {
        let gap_a = &a["longestPayoffGap"];
        let gap_b = &b["longestPayoffGap"];
        if gap_a["length"].as_u64().unwrap_or(0) > gap_b["length"].as_u64().unwrap_or(0) + 1 {
            findings.push(format!("第 {}–{} 章连续 {} 章没有爽点，对标书最长只空 {} 章。",
                gap_a["from"], gap_a["to"], gap_a["length"], gap_b["length"]));
        }
        let density = |v: &Value| v["payoffsPerChapter"].as_f64().unwrap_or(0.0);
        if density(&a) + 0.3 < density(&b) {
            findings.push(format!("爽点密度每章 {:.2} 个，对标书 {:.2} 个。", density(&a), density(&b)));
        }
        let hook_rate = |v: &Value| v["endHookRate"].as_f64().unwrap_or(0.0);
        if hook_rate(&a) + 0.15 < hook_rate(&b) {
            findings.push(format!("章末留钩子的章节占 {:.0}%，对标书 {:.0}%。", hook_rate(&a) * 100.0, hook_rate(&b) * 100.0));
        }
        if let Some(top) = a["endHookTypes"].as_array().and_then(|t| t.first()) {
            let total: u64 = a["endHookTypes"].as_array().unwrap().iter().map(|t| t["count"].as_u64().unwrap_or(0)).sum();
            let share = top["count"].as_u64().unwrap_or(0) as f64 / total.max(1) as f64;
            if total >= 5 && share >= 0.6 {
                findings.push(format!("章末钩子 {:.0}% 都是{}钩，类型单一。", share * 100.0, top["type"].as_str().unwrap_or("")));
            }
        }
        let info = |v: &Value| v["infoPerChapter"].as_f64().unwrap_or(0.0);
        if info(&a) > info(&b) * 1.8 && info(&a) >= 3.0 {
            findings.push(format!("每章新信息 {:.1} 条，对标书 {:.1} 条，可能在倾泻设定。", info(&a), info(&b)));
        }
        if findings.is_empty() {
            findings.push("爽点密度、章末钩子与信息释放和对标书接近，没有明显差距。".to_owned());
        }
    }
    Ok(json!({"mine":{"summary":a,"curve":mine},"benchmark":{"projectId":pid,"title":project(db, pid).await?["title"],"summary":b,"curve":theirs},
        "findings":findings,"note":"基于章节卡统计；未索引的章节不计入，诊断是参考不是定论"}))
}

/// 跨参考书检索写法案例：钩子、爽点、事件与章节概要里包含关键词的条目，附章节与原文摘录。
pub async fn search_cases(db: &mut SqliteConnection, query: &str, only: Option<&str>, limit: usize) -> Result<Vec<Value>, String> {
    let terms: Vec<&str> = query.split_whitespace().filter(|t| !t.is_empty()).collect();
    if terms.is_empty() {
        return Err("请给出检索关键词".into());
    }
    let matches = |text: &str| terms.iter().all(|t| text.contains(t));
    let mut out = Vec::new();
    for p in list(db, BREAKDOWN_NS, "project:").await? {
        let pid = p["id"].to_string();
        if only.is_some_and(|o| o != pid) {
            continue;
        }
        let bid = book_id(&pid);
        let titles: BTreeMap<String, (i64, String)> = catalog(db, &pid)
            .await?
            .iter()
            .enumerate()
            .map(|(i, c)| (c["id"].to_string(), (i as i64 + 1, c["title"].as_str().unwrap_or("").to_owned())))
            .collect();
        for card in canon::list_cards(db, &bid).await?.into_iter().filter(|c| c["status"] == "active") {
            let cid = card["chapterId"].as_str().unwrap_or("").to_owned();
            let (no, title) = titles.get(&cid).cloned().unwrap_or((0, String::new()));
            let c = &card["card"];
            let mut push = |kind: &str, label: String, quote: &Value| {
                if out.len() < limit && matches(&label) {
                    out.push(json!({"project":p["title"],"projectId":pid,"chapterNo":no,"chapterTitle":title,"kind":kind,"text":label,"quote":quote}));
                }
            };
            for h in c["hooks"].as_array().into_iter().flatten() {
                push("钩子", format!("{}钩（{}）：{}", h["type"].as_str().unwrap_or(""), h["position"].as_str().unwrap_or(""), h["text"].as_str().unwrap_or("")), &Value::Null);
            }
            for pay in c["payoffs"].as_array().into_iter().flatten() {
                push("爽点", format!("{}，强度 {}，铺垫：{}{}", pay["type"].as_str().unwrap_or(""), pay["strength"],
                    pay["setupRef"].as_str().unwrap_or("未注明"), if pay["amplified"] == true { "，写了旁观者反应" } else { "" }), &pay["quote"]);
            }
            for e in c["events"].as_array().into_iter().flatten() {
                push("事件", e["summary"].as_str().unwrap_or("").to_owned(), &Value::Null);
            }
            push("本章", c["oneLine"].as_str().unwrap_or("").to_owned(), &Value::Null);
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// 拆书报告：章节卡按段归纳再合并成全书结论，保存为对话里的成果
// ---------------------------------------------------------------------------

/// 一段最多这么多章，或摘要超过这么多字就切段；千章的书也能分段交给模型。
const REPORT_SEGMENT_CHAPTERS: usize = 40;
const REPORT_SEGMENT_CHARS: usize = 16_000;

fn clip(value: &Value, limit: usize) -> String {
    value.as_str().unwrap_or("").chars().take(limit).collect()
}

/// 一章章节卡的摘要：一句话、关键事件、钩子、爽点、伏笔开合、情绪。
fn chapter_digest(no: usize, title: &Value, card: &Value) -> Value {
    let list = |key: &str| card[key].as_array().cloned().unwrap_or_default();
    json!({"no":no,"title":title,"oneLine":card["oneLine"],
        "events":list("events").iter().take(6).map(|e| clip(&e["summary"], 60)).collect::<Vec<_>>(),
        "hooks":list("hooks").iter().map(|h| json!({"position":h["position"],"type":h["type"],"strength":h["strength"],"text":clip(&h["text"], 40)})).collect::<Vec<_>>(),
        "payoffs":list("payoffs").iter().map(|p| json!({"type":p["type"],"strength":p["strength"],"amplified":p["amplified"],"quote":clip(&p["quote"], 40)})).collect::<Vec<_>>(),
        "planted":list("threads").iter().filter(|t| t["action"] == "plant").map(|t| clip(&t["text"], 40)).collect::<Vec<_>>(),
        "paid":list("threads").iter().filter(|t| t["action"] == "payoff").map(|t| clip(&t["text"], 40)).collect::<Vec<_>>(),
        "emotion":format!("{} → {} → {}", clip(&card["emotion"]["open"], 20), clip(&card["emotion"]["peak"], 20), clip(&card["emotion"]["close"], 20))})
}

/// 写报告要用的数据：区间内已拆章节的摘要（分好段）、覆盖情况、节奏统计、伏笔与文风数据。
/// 只给模型章节卡里的事实，不给整章正文；没拆的章节如实列出，报告里写明覆盖范围。
pub async fn report_data(db: &mut SqliteConnection, pid: &str, from: i64, to: i64) -> Result<Value, String> {
    let p = project(db, pid).await?;
    let bid = book_id(pid);
    let points = curve(db, &bid).await?;
    let total = points.len();
    if total == 0 {
        return Err("参考书没有章节".into());
    }
    let start = (from.max(1) as usize).min(total);
    let end = if to <= 0 { total } else { (to as usize).clamp(start, total) };
    let range = &points[start - 1..end];
    let cards: BTreeMap<String, Value> = canon::list_cards(db, &bid)
        .await?
        .into_iter()
        .filter(|c| c["status"] == "active")
        .filter_map(|c| Some((c["chapterId"].as_str()?.to_owned(), c["card"].clone())))
        .collect();
    let mut segments: Vec<Value> = Vec::new();
    let mut current: Vec<Value> = Vec::new();
    let mut current_chars = 0;
    let mut missing: Vec<(i64, i64)> = Vec::new();
    for point in range {
        let no = point["no"].as_i64().unwrap_or(0);
        let Some(card) = point["chapterId"].as_str().and_then(|cid| cards.get(cid)) else {
            match missing.last_mut() {
                Some(last) if last.1 + 1 == no => last.1 = no,
                _ => missing.push((no, no)),
            }
            continue;
        };
        let digest = chapter_digest(no as usize, &point["title"], card);
        let chars = digest.to_string().chars().count();
        if !current.is_empty() && (current.len() >= REPORT_SEGMENT_CHAPTERS || current_chars + chars > REPORT_SEGMENT_CHARS) {
            segments.push(json!({"from":current[0]["no"],"to":current[current.len() - 1]["no"],"chapters":std::mem::take(&mut current)}));
            current_chars = 0;
        }
        current_chars += chars;
        current.push(digest);
    }
    if !current.is_empty() {
        segments.push(json!({"from":current[0]["no"],"to":current[current.len() - 1]["no"],"chapters":current}));
    }
    let indexed = range.iter().filter(|p| p["indexed"] == true).count();
    if indexed == 0 {
        return Ok(json!({"ok":false,"toolError":{"code":"not_indexed","retryable":false,
            "message":format!("《{}》第 {start}–{end} 章还没拆，先用 start_breakdown 拆出要写进报告的章节", p["title"].as_str().unwrap_or(""))}}));
    }
    let chapter_ids: BTreeSet<&str> = range.iter().filter_map(|p| p["chapterId"].as_str()).collect();
    let order: BTreeMap<&str, i64> = range.iter().filter_map(|p| Some((p["chapterId"].as_str()?, p["no"].as_i64()?))).collect();
    let threads = canon::list_threads(db, &bid, None).await?;
    let in_range: Vec<&Value> = threads
        .iter()
        .filter(|t| t["plantedChapter"].as_str().is_some_and(|c| chapter_ids.contains(c)))
        .collect();
    let distances: Vec<i64> = in_range
        .iter()
        .filter(|t| t["status"] == "paid")
        .filter_map(|t| Some(order.get(t["paidChapter"].as_str()?)? - order.get(t["plantedChapter"].as_str()?)?))
        .collect();
    let mut sample = String::new();
    for point in range.iter().take(30) {
        if let Ok(c) = chapter(db, pid, point["chapterId"].as_str().unwrap_or("")).await {
            sample.push_str(c["textContent"].as_str().unwrap_or(""));
            sample.push('\n');
        }
    }
    Ok(json!({"projectId":pid,"title":p["title"],"from":start,"to":end,"totalChapters":total,"indexedChapters":indexed,
        "missing":missing.iter().map(|(a, b)| if a == b { format!("第{a}章") } else { format!("第{a}–{b}章") }).collect::<Vec<_>>(),
        "stats":summarize(range),
        "threads":{"planted":in_range.len(),"paid":distances.len(),
            "averagePayoffDistance":if distances.is_empty() { Value::Null } else { json!(distances.iter().sum::<i64>() as f64 / distances.len() as f64) }},
        "style":crate::agent_style::metrics(&sample),"segments":segments}))
}

fn report_key(pid: &str) -> String {
    format!("breakdown-report:{pid}")
}

/// 保存拆书报告：对话里的成果（可编辑的 Markdown，附结构化内容），并记下最新一份的要点供书级画像引用。
/// 报告分析的是参考书本身，可以写参考书人名；配方部分不能带人名（之后会被作者拿去用）。
pub async fn save_report(db: &mut SqliteConnection, session_id: &str, run_id: &str, args: &Value) -> Result<Value, String> {
    let pid = project_id_arg(args)?;
    let p = project(db, &pid).await?;
    let report = &args["report"];
    if !report.is_object() || !report["stages"].is_array() || !report["techniques"].is_array() {
        return Err("拆书报告格式无效".into());
    }
    let recipe = report["recipe"].to_string();
    if let Some(error) = leak_error(&recipe, &reference_names(db, &pid).await?) {
        return Ok(json!({"ok":false,"toolError":{"code":"recipe_has_names","retryable":true,"names":error["toolError"]["names"],
            "message":"报告里的写法配方要抽象成角色位置，不能带参考书人名；其他部分可以写"}}));
    }
    let (from, to) = (args["from"].as_i64().unwrap_or(1), args["to"].as_i64().unwrap_or(0));
    let artifact_id = format!("{session_id}:{}", str_field(args, "operation_id")?);
    let title = p["title"].as_str().unwrap_or("参考书");
    let record = json!({"id":artifact_id,"sessionId":session_id,"runId":run_id,"kind":"breakdown_report",
        "title":format!("《{title}》拆书报告（第 {from}–{to} 章）"),"bookId":"","bookTitle":title,"projectId":pid,
        "range":{"from":from,"to":to},"report":report,"content":args["content"].as_str().ok_or("缺少报告正文")?,
        "original":"","applied":false,"revision":0,"createdAt":now()});
    put(db, crate::agent_store::NS, &format!("artifact:{artifact_id}"), &record).await?;
    put(db, crate::agent_store::NS, &report_key(&pid), &json!({"id":report_key(&pid),"projectId":pid,"artifactId":artifact_id,
        "from":from,"to":to,"overview":report["overview"],"stages":report["stages"],"techniques":report["techniques"],"createdAt":now()})).await?;
    Ok(record)
}

/// 最新一份拆书报告的要点（没有时为 null）。
pub async fn latest_report(db: &mut SqliteConnection, pid: &str) -> Result<Value, String> {
    Ok(get(db, crate::agent_store::NS, &report_key(pid)).await?.filter(|r|r["stale"]!=true).unwrap_or(Value::Null))
}

pub async fn recipes(db: &mut SqliteConnection, pid: &str) -> Result<Vec<Value>, String> {
    let mut items = list(db, canon::AUTHOR_NS, &format!("recipe:{}:", book_id(pid))).await?;
    items.retain(|r| !r["deletedAt"].is_number());
    items.sort_by_key(|r| r["createdAt"].as_i64().unwrap_or(0));
    Ok(items)
}

/// 保存可复用配方：按章位置抽象的写法模板，不能带参考书的人物名与具体情节。
pub async fn save_recipe(db: &mut SqliteConnection, pid: &str, title: &str, steps: &Value, source: &str) -> Result<Value, String> {
    let p = project(db, pid).await?;
    let text = format!("{title}\n{steps}");
    if let Some(error) = leak_error(&text, &reference_names(db, pid).await?) {
        return Ok(json!({"ok":false,"toolError":{"code":"recipe_has_names","retryable":true,"names":error["toolError"]["names"],
            "message":"配方要抽象成写法模板，不能出现参考书的人物名；换成“主角”“对手”这类角色位置再保存"}}));
    }
    let key = format!("recipe:{}:{}", book_id(pid), id());
    let record = json!({"id":key,"projectId":pid,"projectTitle":p["title"],"title":title,"steps":steps,"source":source,"createdAt":now()});
    put(db, canon::AUTHOR_NS, &key, &record).await?;
    Ok(record)
}

/// 拆书库里的全部项目，附章节卡覆盖情况。
/// 工作区每次加载都会列一遍，章节卡只按状态计数，不读整张卡。
pub async fn list_projects(db: &mut SqliteConnection) -> Result<Vec<Value>, String> {
    let mut out = Vec::new();
    for p in list(db, BREAKDOWN_NS, "project:").await? {
        let pid = p["id"].to_string();
        let cards = active_card_chapters(db, &book_id(&pid)).await?.len();
        let chapters = p["chapters"].as_array().map_or(0, |a| a.len());
        let job = crate::agent_breakdown_job::view(db, &pid).await?;
        out.push(json!({"projectId":pid,"title":p["title"],"chapters":chapters,"words":p["wordCount"],"cards":cards,
            "hasReport":p["report"].is_object() || latest_report(db, &pid).await?["artifactId"].is_string(),"updateTime":p["updateTime"],
            "job":job.map(|j| json!({"status":j["status"],"from":j["from"],"to":j["to"],"done":j["done"],"total":j["total"],"failed":j["failed"]}))}));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// 导入参考书：输入框里拖进整本书，分章后一次写进拆书库
// ---------------------------------------------------------------------------

/// 单本参考书的章节上限与单章字数上限（前端分章时已把超长章拆成续章）。
pub const IMPORT_MAX_CHAPTERS: usize = 20_000;
pub const IMPORT_MAX_CHAPTER_CHARS: usize = 120_000;

/// 把分好章的整本书写进拆书库，格式与拆书工作台一致（两边都能打开）。一个事务内写完，失败不留半本。
/// 导入不等于拆书：同时建好正文检索（本地切块，不调模型），不拆也能按章读原文、搜原文；
/// 拆解（逐章提取章节卡）由作者或 Agent 另行按范围发起。
pub async fn import_book(db: &mut SqliteConnection, title: &str, chapters: &Value, source_hash: &str, notes: &Value) -> Result<Value, String> {
    let title = title.trim();
    if title.is_empty() {
        return Err("缺少书名".into());
    }
    let items = chapters.as_array().filter(|a| !a.is_empty()).ok_or("没有可导入的章节")?;
    // 同一份文本再拖进来：复用已存的参考书（含已拆的章节卡），不重复存放
    if !source_hash.is_empty() {
        for p in list(db, BREAKDOWN_NS, "project:").await? {
            if p["sourceHash"] == source_hash {
                let pid = p["id"].to_string();
                return Ok(json!({"projectId":pid,"bookId":book_id(&pid),"title":p["title"],"chapters":p["chapters"].as_array().map_or(0, |a| a.len()),
                    "words":p["wordCount"],"existing":true}));
            }
        }
    }
    if items.len() > IMPORT_MAX_CHAPTERS {
        return Err(format!("章节超过 {IMPORT_MAX_CHAPTERS} 章，请先确认分章是否正确"));
    }
    // 与前端本地编号同一口径（毫秒 × 1000），撞号就往后挪
    let mut base = now() * 1000;
    while get(db, BREAKDOWN_NS, &format!("project:{base}")).await?.is_some()
        || get(db, BREAKDOWN_NS, &format!("content:{}", base + 1)).await?.is_some()
    {
        base += items.len() as i64 + 1;
    }
    let stamp = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let mut catalog = Vec::new();
    let mut total_words = 0usize;
    for (index, item) in items.iter().enumerate() {
        let text = item["text"].as_str().unwrap_or("");
        if text.chars().count() > IMPORT_MAX_CHAPTER_CHARS {
            return Err(format!("第 {} 章超过 {IMPORT_MAX_CHAPTER_CHARS} 字，请先拆分", index + 1));
        }
        let paragraphs: Vec<&str> = text.split('\n').map(str::trim).filter(|p| !p.is_empty()).collect();
        let words = text.chars().filter(|c| !c.is_whitespace()).count();
        total_words += words;
        let chapter_id = base + index as i64 + 1;
        let heading = item["title"].as_str().map(str::trim).filter(|t| !t.is_empty());
        let chapter_title = heading.map(str::to_owned).unwrap_or_else(|| format!("第{}章", index + 1));
        catalog.push(json!({"id":chapter_id,"title":chapter_title,"status":"wait","wordCount":words,"sortNo":index + 1}));
        put(db, BREAKDOWN_NS, &format!("content:{chapter_id}"), &json!({"projectId":base,"chapterId":chapter_id,
            "paragraphs":paragraphs,"analysis":null,"paragraphInsightIds":vec![Value::Null; paragraphs.len()]})).await?;
        // 与 chapter() 读出的正文同一口径（段落以换行连接），指纹一致，之后拆解时检索块按同一版本覆盖
        let body = paragraphs.join("\n");
        let fingerprint = version_of(&json!({"title":chapter_title,"textContent":body}));
        crate::agent_indexer::index_chunks(db, &book_id(&base.to_string()), &chapter_id.to_string(), &fingerprint, &body, "unknown").await?;
    }
    let record = json!({"id":base,"title":title,"author":"","tags":[],"mood":"","characterCount":0,"wordCount":total_words,
        "chapterCount":catalog.len(),"progress":0,"status":"wait","createTime":stamp,"updateTime":stamp,"chapters":catalog,
        "report":null,"characterNames":[],"source":"agent","visibility":"conversation","sourceHash":source_hash,
        "importNotes":notes.as_array().cloned().unwrap_or_default()});
    put(db, BREAKDOWN_NS, &format!("project:{base}"), &record).await?;
    Ok(json!({"projectId":base.to_string(),"bookId":book_id(&base.to_string()),"title":title,"chapters":items.len(),"words":total_words}))
}

/// 对照方案：对标做法 → 我的做法 → 差异。“我的做法”与“差异”不能出现参考书人物名。
pub async fn check_comparison_rows(db: &mut SqliteConnection, project_ids: &[String], rows: &Value) -> Result<Option<Value>, String> {
    let mut names = BTreeSet::new();
    for pid in project_ids {
        project(db, pid).await?;
        names.extend(reference_names(db, pid).await?);
    }
    let mine = rows
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| format!("{}\n{}", r["mine"].as_str().unwrap_or(""), r["difference"].as_str().unwrap_or("")))
        .collect::<Vec<_>>()
        .join("\n");
    Ok(leak_error(&mine, &names))
}

pub fn render_comparison(title: &str, rows: &Value) -> String {
    let mut lines = vec![format!("# {title}"), String::new(), "| 维度 | 对标做法 | 我的做法 | 差异说明 |".to_owned(), "| --- | --- | --- | --- |".to_owned()];
    let cell = |v: &Value| v.as_str().unwrap_or("").replace('|', "／").replace('\n', " ");
    for row in rows.as_array().into_iter().flatten() {
        lines.push(format!("| {} | {} | {} | {} |", cell(&row["dimension"]), cell(&row["benchmark"]), cell(&row["mine"]), cell(&row["difference"])));
    }
    lines.join("\n")
}

pub fn project_id_arg(args: &Value) -> Result<String, String> {
    match &args["project_id"] {
        Value::String(s) if !s.is_empty() => Ok(s.clone()),
        Value::Number(n) => Ok(n.to_string()),
        _ => str_field(args, "projectId").map(str::to_owned),
    }
}

/// 可直接阅读的拆书成果，不要求模型再跑一轮。按已拆章节分页，报告沿用原成果记录；to=0 表示到末章。
pub async fn results_in_range(db: &mut SqliteConnection, pid: &str, offset: usize, from: usize, to: usize) -> Result<Value,String> {
    let p=project(db,pid).await?;
    let bid=book_id(pid);
    let indexed=active_card_chapters(db,&bid).await?;
    let catalog=catalog(db,pid).await?;
    let from=from.max(1).min(catalog.len().max(1));
    let to=if to==0 { catalog.len() } else { to.min(catalog.len()).max(from) };
    let covered:Vec<_>=catalog.iter().enumerate().filter(|(no,c)|no+1>=from && no+1<=to && indexed.contains(&c["id"].to_string())).collect();
    let mut cards=Vec::new();
    for (no,c) in covered.iter().skip(offset).take(20) {
        if let Some(card)=canon::get_card(db,&bid,&c["id"].to_string()).await? {
            cards.push(chapter_digest(no+1,&c["title"],&card["card"]));
        }
    }
    let latest=latest_report(db,pid).await?;
    let report_stale=latest["artifactId"].is_null() && p["previousReportId"].is_string();
    let report=match latest["artifactId"].as_str().or(p["previousReportId"].as_str()) {
        Some(aid)=>get(db,crate::agent_store::NS,&format!("artifact:{aid}")).await?.unwrap_or(Value::Null),
        None=>Value::Null,
    };
    Ok(json!({"projectId":pid,"title":p["title"],"total":catalog.len(),"rangeFrom":from,"rangeTo":to,"rangeTotal":to.saturating_sub(from-1),"indexed":covered.len(),"offset":offset,"cards":cards,
        "report":report,"reportStale":report_stale,"legacyReport":p["report"],"job":crate::agent_breakdown_job::view(db,pid).await?,
        "savedToWorkbench":p["visibility"]!="conversation", "needsRepair":p["parserVersion"]!=2,"repairNote":p["repairNote"],"notes":p["importNotes"],
        "searchReady":p["searchCursor"].as_u64().is_none_or(|n|n>=catalog.len() as u64)}))
}

/// 对话侧栏只展示本会话、同一章节区间的报告，不借用其他会话最近生成的报告。
pub async fn conversation_report(db: &mut SqliteConnection, pid: &str, sid: &str, from: usize, to: usize) -> Result<Value,String> {
    let raw:Option<String>=sqlx::query_scalar("SELECT value FROM desktop_records WHERE namespace=? AND key LIKE '\"artifact:%' AND json_extract(value,'$.kind')='breakdown_report' AND json_extract(value,'$.projectId')=? AND json_extract(value,'$.sessionId')=? AND json_extract(value,'$.range.from')=? AND json_extract(value,'$.range.to')=? AND json_extract(value,'$.deletedAt') IS NULL ORDER BY json_extract(value,'$.createdAt') DESC LIMIT 1")
        .bind(crate::agent_store::NS).bind(pid).bind(sid).bind(from as i64).bind(to as i64).fetch_optional(db).await.map_err(crate::agent_store::err)?;
    raw.map(|r|serde_json::from_str(&r).map_err(crate::agent_store::err)).transpose().map(|r|r.unwrap_or(Value::Null))
}
