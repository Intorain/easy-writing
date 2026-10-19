//! 开书准备与正式作品分开；建书用的是作者看到的具体版本，不是模型声称“已确认”。
//! 进度不靠模型记：按作者拍板的决定和各步内容从数据算出，宿主、界面、模型看同一份。
use crate::{agent_contract, agent_creation as creation, agent_service as service, agent_store::*, agent_tool_boundary::{failure, FailureKind}};
use serde_json::{json, Value};
use sqlx::SqliteConnection;
use std::path::Path;

/// 八步顺序与作者决定保持不变，显示名称使用明确的创作概念。
pub const STEPS: &[(&str, &str, &[&str])] = &[
    ("scope", "题材定位", &["genre", "platform"]),
    ("hook", "故事构思", &["hook"]),
    ("protagonist", "人物设定", &["protagonistName", "protagonistGoal", "narration"]),
    ("world", "世界观设定", &[]),
    ("story", "剧情大纲", &[]),
    ("opening", "开篇章纲", &["openingDirection"]),
    ("packaging", "书名与简介", &[]),
    ("build", "创建作品", &[]),
];
const DECISION_LABELS: &[(&str, &str)] = &[
    ("genre", "题材"), ("platform", "平台与读者"), ("hook", "已选故事方向"),
    ("protagonistName", "主角姓名"), ("protagonistGoal", "主角行动目标"),
    ("narration", "叙述人称"), ("openingDirection", "已选开篇"),
];
fn decision_label(key: &str) -> &'static str {
    DECISION_LABELS.iter().find(|(k, _)| *k == key).map_or("", |(_, label)| label)
}
fn text(v: &Value) -> Option<&str> {
    v.as_str().map(str::trim).filter(|s| !s.is_empty())
}
fn items(v: &Value) -> &[Value] { v.as_array().map_or(&[], Vec::as_slice) }

fn step_content(plan: &Value, key: &str) -> bool {
    match key {
        "scope" => text(&plan["decisions"]["genre"]["value"]).is_some()
            || text(&plan["decisions"]["platform"]["value"]).is_some()
            || text(&plan["profileId"]).is_some() || text(&plan["audience"]).is_some(),
        "hook" => ["premise", "sellingPoint", "conflict"].iter().any(|field| text(&plan[field]).is_some())
            || text(&plan["decisions"]["hook"]["value"]).is_some(),
        "protagonist" => !items(&plan["characters"]).is_empty() || text(&plan["protagonist"]).is_some()
            || text(&plan["decisions"]["protagonistName"]["value"]).is_some(),
        "world" => !items(&plan["world"]).is_empty(),
        "story" => text(&plan["outline"]).is_some(),
        "opening" => text(&plan["opening"]).is_some(),
        "packaging" => text(&plan["workingTitle"]).is_some() || text(&plan["intro"]).is_some(),
        _ => false,
    }
}

fn decision<'a>(plan: &'a Value, key: &str) -> &'a Value {
    if key == "narration" && plan["writingPreferences"]["narration"].is_object() {
        &plan["writingPreferences"]["narration"]
    } else { &plan["decisions"][key] }
}

/// 进度从数据算出：每步 done（作者定了）/ assumed（作者让模型定，先按这个）/ partial（定了一部分）/ todo；
/// current 是第一步没走完的；buildable 表示能建书；blocking 是还没定的作者决定。已有作品重新定位只要求书名。
pub fn progress(plan: &Value, applied: bool, existing_book: bool) -> Value {
    let mut steps = Vec::new();
    let mut blocking: Vec<String> = Vec::new();
    let mut current = None;
    for (key, label, decisions) in STEPS {
        let mut missing: Vec<String> = Vec::new();
        let status = match *key {
            "build" => if applied { "done" } else { "todo" },
            "packaging" => {
                if step_content(plan, key) { "done" } else { missing.push("书名".into()); "todo" }
            }
            _ if decisions.is_empty() => if step_content(plan, key) { "done" } else { "todo" },
            _ => {
                let mut present = 0;
                let mut assumed = false;
                for d in decisions.iter() {
                    match text(&decision(plan, d)["value"]).filter(|_| matches!(decision(plan, d)["by"].as_str(), Some("author" | "assumed"))) {
                        Some(_) => {
                            present += 1;
                            assumed |= decision(plan, d)["by"] == "assumed";
                        }
                        None => missing.push(decision_label(d).into()),
                    }
                }
                if present == decisions.len() {
                    if assumed { "assumed" } else { "done" }
                } else if present > 0 || step_content(plan, key) {
                    "partial"
                } else {
                    "todo"
                }
            }
        };
        if !existing_book || *key == "packaging" {
            blocking.extend(missing.iter().cloned());
        }
        if current.is_none() && matches!(status, "todo" | "partial") {
            current = Some(*key);
        }
        steps.push(json!({"key":key,"label":label,"status":status,"missing":missing}));
    }
    if applied { current = None; }
    json!({"steps":steps,"current":current,"buildable":!applied && blocking.is_empty(),"blocking":blocking})
}

/// 兼容已拆分的人称字段；不自动替作者决定人称。
pub fn normalize(plan: &Value, _existing_book: bool) -> Value {
    let mut out = plan.clone();
    if !out["writingPreferences"].is_object() { out["writingPreferences"] = json!({}); }
    if out["writingPreferences"]["narration"].is_null() && out["decisions"]["narration"].is_object() {
        out["writingPreferences"]["narration"] = out["decisions"]["narration"].clone();
    }
    if let Some(old) = text(&out["writingPreferences"]["narration"]["value"]).map(str::to_owned) {
        if !["第一人称", "第三人称"].contains(&old.as_str()) {
            // 仅兼容旧数据中明确以人称名称起头的写法，附加说明完整保留。
            out["writingPreferences"]["legacyNarrationNote"] = json!(old);
            if let Some(person) = ["第一人称", "第三人称"].into_iter().find(|person|
                old.starts_with(person) && !(old.contains("第一人称") && old.contains("第三人称"))) {
                out["writingPreferences"]["narration"]["value"] = json!(person);
            } else {
                out["writingPreferences"].as_object_mut().unwrap().remove("narration");
            }
        }
    }
    out
}

/// 读取时只刷新派生展示，不改旧方案的作者内容或版本。
pub fn with_progress(mut artifact: Value) -> Value {
    if artifact["kind"] == "book_plan" {
        artifact["bookPlan"] = normalize(&artifact["bookPlan"], artifact["bookId"].as_str().is_some_and(|id| !id.is_empty()));
        artifact["progress"] = progress(&artifact["bookPlan"], artifact["applied"] == true,
            artifact["bookId"].as_str().is_some_and(|id| !id.is_empty()));
        if artifact["title"] == "开书准备" || artifact["title"] == "开书草案" { artifact["title"] = json!("开书方案"); }
    }
    artifact
}

/// 沿用当前创作任务中作者实际点选的故事快照，不让模型重新猜选择结果。
fn selected_direction(session: &Value, previous: Option<&Value>) -> Value {
    let runs = items(&session["taskRunIds"]);
    session["messages"].as_array().into_iter().flatten().rev()
        .find(|m| m["role"] == "user" && m["selectedDirection"].is_object()
            && (m["runId"] == session["runId"] || runs.contains(&m["runId"])))
        .map(|m| m["selectedDirection"].clone())
        .or_else(|| previous.and_then(|p| p.get("selectedDirection")).cloned())
        .unwrap_or(Value::Null)
}

/// 测试用：八步都由作者定好的准备表，只差建书。
#[cfg(test)]
pub fn decided_plan(title: &str) -> Value {
    let by = |v: &str| json!({"value":v,"by":"author"});
    json!({"workingTitle":title,"decisions":{"genre":by("都市高武"),"platform":by("番茄，男频"),"hook":by("副本里靠背规则活下去"),
        "protagonistName":by("林渡"),"protagonistGoal":by("把妹妹从名单上划掉"),"narration":by("第三人称"),"openingDirection":by("冲突开场")}})
}

/// 准备表按八步整理成（标题，行）给作者看：对话里的 Markdown 与作品大纲里的 HTML 共用。以“- ”开头的行是条目。
fn sections(plan: &Value) -> Vec<(String, Vec<String>)> {
    let decision = |key: &str| -> String {
        match text(&decision(plan, key)["value"]) {
            Some(v) => format!(
                "{}：{v}{}",
                decision_label(key),
                match decision(plan, key)["by"].as_str() { Some("assumed") => "（按作者委托拟定）", Some("suggested") => "（AI 建议）", _ => "（作者已定）" }
            ),
            None => format!("{}：还没定", decision_label(key)),
        }
    };
    let mut out = Vec::new();
    for (number, (key, label, decisions)) in STEPS.iter().enumerate() {
        if *key == "build" {
            continue;
        }
        let mut lines: Vec<String> = decisions.iter().map(|d| decision(d)).collect();
        match *key {
            "scope" => {
                for (field, name) in [("audience", "目标读者"), ("styleRequirements", "文风要求")] {
                    if let Some(v) = text(&plan[field]) {
                        lines.push(format!("{name}：{v}"));
                    }
                }
            }
            "hook" => {
                for (field, label) in [("premise", "故事构思"), ("sellingPoint", "核心卖点"), ("conflict", "主要冲突")] {
                    if let Some(v) = text(&plan[field]) {
                        lines.push(format!("{label}：{v}"));
                    }
                }
            }
            "protagonist" => {
                for c in items(&plan["characters"]) {
                    let mut line = format!(
                        "- {}（{}）：{}",
                        text(&c["name"]).unwrap_or("未命名"),
                        c["role"].as_str().unwrap_or("人物"),
                        text(&c["summary"]).unwrap_or("")
                    );
                    for (field, name) in [("goal", "行动目标"), ("motivation", "人物动机"), ("longTermGoal", "长期目标"), ("personality", "性格"), ("ability", "特殊能力及限制")] {
                        if let Some(v) = text(&c[field]) {
                            line.push_str(&format!("；{name}：{v}"));
                        }
                    }
                    lines.push(line);
                }
                if let Some(v) = text(&plan["protagonist"]) {
                    lines.push(v.to_owned());
                }
            }
            "world" => {
                for w in items(&plan["world"]) {
                    lines.push(format!(
                        "- {}{}：{}",
                        text(&w["name"]).unwrap_or(""),
                        w["type"].as_str().map(|t| format!("（{t}）")).unwrap_or_default(),
                        text(&w["detail"]).unwrap_or("")
                    ));
                }
            }
            "story" => {
                if let Some(v) = text(&plan["outline"]) {
                    lines.push(v.to_owned());
                }
            }
            "opening" => {
                if let Some(v) = text(&plan["opening"]) {
                    lines.push(v.to_owned());
                }
            }
            "packaging" => {
                lines.push(format!("书名：{}", text(&plan["workingTitle"]).unwrap_or("还没定")));
                if let Some(v) = text(&plan["intro"]) {
                    lines.push(format!("简介：{v}"));
                }
            }
            _ => {}
        }
        out.push((format!("{} {label}", number + 1), lines));
    }
    let preferences = normalize(plan, true)["writingPreferences"].clone();
    let mut lines = Vec::new();
    for (key, label) in [("narration", "叙述人称"), ("viewpoint", "叙述视角"), ("tone", "故事基调")] {
        if let Some(value) = text(&preferences[key]["value"]) {
            let origin = match preferences[key]["by"].as_str() { Some("author") => "作者已定", Some("assumed") => "按作者委托拟定", _ => "AI 建议，可调整" };
            lines.push(format!("{label}：{value}（{origin}）"));
        }
    }
    if let Some(note) = text(&preferences["legacyNarrationNote"]) { lines.push(format!("原有叙述说明：{note}")); }
    if let Some(style) = text(&plan["styleSuggestions"]) { lines.push(format!("AI 文风建议（可调整）：{style}")); }
    if !lines.is_empty() { out.push(("写作偏好".into(), lines)); }
    let unresolved: Vec<String> = items(&plan["unresolved"]).iter().filter_map(Value::as_str).map(|s| format!("- {s}")).collect();
    if !unresolved.is_empty() {
        out.push(("未定事项".into(), unresolved));
    }
    out
}

pub fn content(plan: &Value) -> String {
    let mut out = format!("# {}\n\n书名随时可改。", text(&plan["workingTitle"]).unwrap_or("未定名作品"));
    for (heading, lines) in sections(plan) {
        out.push_str(&format!("\n\n## {heading}"));
        for line in lines {
            out.push_str(if line.starts_with("- ") { "\n" } else { "\n\n" });
            out.push_str(&line);
        }
    }
    out
}

/// 正式大纲使用编辑器原有富文本格式；结构化字段转为安全段落，不把 Markdown 标记写入正文。
fn outline_html(plan: &Value) -> String {
    let escape = |s: &str| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    };
    let paragraphs = |s: &str| {
        s.lines()
            .map(|line| format!("<p>{}</p>", escape(line)))
            .collect::<String>()
    };
    let mut html = format!(
        "<h2>{}</h2><p>书名随时可改。</p>",
        escape(text(&plan["workingTitle"]).unwrap_or(""))
    );
    for (heading, lines) in sections(plan) {
        html.push_str(&format!("<h3>{}</h3>", escape(&heading)));
        let (bullets, plain): (Vec<&String>, Vec<&String>) = lines.iter().partition(|l| l.starts_with("- "));
        for line in plain {
            html.push_str(&paragraphs(line));
        }
        if !bullets.is_empty() {
            html.push_str("<ul>");
            for b in bullets {
                html.push_str(&format!("<li>{}</li>", escape(&b[2..])));
            }
            html.push_str("</ul>");
        }
    }
    html
}


pub async fn save(root: &Path, sid: &str, args: &Value) -> Result<Value, String> {
    agent_contract::validate_book_plan(&args["plan"])?;
    let mut db = crate::desktop_storage::connect_existing_at(root).await?;
    let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
    let s = session(&mut tx, sid).await?;
    if let Some(profile) = args["plan"]["profileId"].as_str() {
        crate::agent_canon::platform_profile(&mut tx, profile).await?;
    }
    let op = format!("book-plan:{sid}:{}", str_field(args, "operation_id")?);
    if let Some(done) = get(&mut tx, OP_NS, &op).await? {
        if done["request"] != *args {
            return Err("重复草案操作编号对应不同内容".into());
        }
        return get(
            &mut tx,
            NS,
            &format!("artifact:{}", str_field(&done, "artifactId")?),
        )
        .await?
        .ok_or("草案不存在".into());
    }
    let aid = args["plan_id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("{sid}:{}", str_field(args, "operation_id").unwrap()));
    let key = format!("artifact:{aid}");
    let existing = get(&mut tx, NS, &key).await?;
    if let Some(a) = &existing {
        if a["kind"] != "book_plan" || a["sessionId"] != sid {
            return Err("该开书草案不可改写，请另建候选版本".into());
        }
        // 模型能自行纠正的前置条件（已采用、版本过期）以结构化失败退回，不当成宿主故障中断运行
        if a["applied"] == true {
            return Ok(failure("plan_not_editable", FailureKind::Precondition,
                "这份草案已采用并建立了作品，不能再改写：改书名用 update_book，改简介用 save_book_intro；要另出方案就不带 plan_id 新建草案。",
                false, "update_book", json!({"planId":aid,"bookId":a["bookId"]})));
        }
        if args["expected_revision"].as_u64() != Some(a["revision"].as_u64().unwrap_or(0)) {
            return Ok(failure("plan_revision_stale", FailureKind::Conflict,
                "草案已有新版本（作者可能在面板改过）：用 read_artifact 读取当前内容与版本，在最新版上更新，不要覆盖作者的修改。",
                false, "read_artifact", json!({"planId":aid,"revision":a["revision"]})));
        }
    }

    let inherited_bid = args["book_id"].as_str().filter(|id| !id.is_empty())
        .or_else(|| existing.as_ref().and_then(|a| a["bookId"].as_str())).unwrap_or("").to_owned();
    let bid = inherited_bid.as_str();
    let mut plan = existing.as_ref().map(|a| a["bookPlan"].clone()).unwrap_or(json!({}));
    for (key, value) in args["plan"].as_object().ok_or("开书方案格式错误")? {
        if matches!(key.as_str(), "decisions" | "writingPreferences") && value.as_object().is_some_and(|v| !v.is_empty()) && plan[key].is_object() {
            for (field, value) in value.as_object().unwrap() { plan[key][field] = value.clone(); }
        } else { plan[key] = value.clone(); }
    }
    if !bid.is_empty() {
        service::allowed_book(&mut tx, &s, bid, false).await?;
        let stored = get(&mut tx, crate::agent_canon::AUTHOR_NS, &format!("book-requirements:{bid}")).await?.unwrap_or(Value::Null);
        if stored["writingPreferences"].is_object() {
            let mut inherited = stored["writingPreferences"].clone();
            for (key, value) in plan["writingPreferences"].as_object().into_iter().flatten() { inherited[key] = value.clone(); }
            plan["writingPreferences"] = inherited;
        }
    }
    if let Some(proposed) = text(&args["plan"]["styleRequirements"]).map(str::to_owned) {
        let prior = existing.as_ref().and_then(|a| a["bookPlan"]["styleRequirements"].as_str()).map(str::to_owned);
        let stored = if bid.is_empty() { Value::Null } else {
            get(&mut tx, crate::agent_canon::AUTHOR_NS, &format!("book-requirements:{bid}")).await?.unwrap_or(Value::Null)
        };
        let quote = text(&args["plan"]["styleRequirementQuote"]);
        let quoted = quote.is_some_and(|quote| items(&s["messages"]).iter().any(|m|
            m["role"] == "user" && m["source"].is_null() && m["text"].as_str().is_some_and(|body| body.contains(quote))));
        let inherited = prior.as_deref() == Some(proposed.as_str()) || stored["styleRequirements"] == proposed;
        if !quoted && !inherited {
            let other = text(&plan["styleSuggestions"]).unwrap_or("");
            plan["styleSuggestions"] = json!(if other.is_empty() || other == proposed { proposed } else { format!("{other}\n{proposed}") });
            plan.as_object_mut().unwrap().remove("styleRequirementQuote");
            if let Some(prior) = prior.or_else(|| stored["styleRequirements"].as_str().map(str::to_owned)) {
                plan["styleRequirements"] = json!(prior);
            } else { plan.as_object_mut().unwrap().remove("styleRequirements"); }
        }
    }
    plan = normalize(&plan, !bid.is_empty());
    let selected = selected_direction(&s, existing.as_ref());
    if selected.is_object() && text(&plan["premise"]).is_none() {
        if let Some(premise) = text(&selected["card"]["sellingPoint"]) { plan["premise"] = json!(premise); }
    }
    let mut base = Value::Null;
    if !bid.is_empty() {
        service::allowed_book(&mut tx, &s, bid, false).await?;
        base = json!({"title":book(&mut tx,bid).await?["title"],"materials":creation::story_version(&creation::materials(&mut tx,bid).await?)});
    }
    let is_new = existing.is_none();
    let mut a = json!({"id":aid,"sessionId":sid,"kind":"book_plan","title":"开书方案","bookId":bid,
        "bookTitle":base["title"].as_str().unwrap_or(""),"bookPlan":plan,"planBase":base,
        "original":"","content":content(&plan),"applied":false,
        "revision":existing.map_or(0,|a|a["revision"].as_u64().unwrap_or(0)+1)});
    if selected.is_object() { a["selectedDirection"] = selected; }
    a["progress"] = progress(&a["bookPlan"], false, !bid.is_empty());
    put(&mut tx, NS, &key, &a).await?;
    // 同一对话里另建的新草案接替之前还没采用的旧草案：旧的标记为已被接替，不再供采用，对话里只留最新一份
    if is_new {
        for mut old in list(&mut tx, NS, "artifact:").await? {
            if old["sessionId"] == sid && old["kind"] == "book_plan" && old["applied"] != true && old["superseded"] != true && old["id"] != aid {
                old["superseded"] = json!(true);
                old["supersededBy"] = json!(aid);
                put(&mut tx, NS, &format!("artifact:{}", str_field(&old, "id")?), &old).await?;
            }
        }
    }
    put(
        &mut tx,
        OP_NS,
        &op,
        &json!({"request":args,"artifactId":aid}),
    )
    .await?;
    tx.commit().await.map_err(err)?;
    Ok(a)
}

/// 旧版自动建书留下的空容器也需要采用草案；不删除、不迁移用户现有作品。
pub async fn needs_review(db: &mut SqliteConnection, bid: &str) -> Result<bool, String> {
    let created:i64=sqlx::query_scalar("SELECT COUNT(*) FROM desktop_records WHERE namespace=? AND json_extract(value,'$.created')=1 AND json_extract(value,'$.bookId')=?")
        .bind(OP_NS).bind(bid).fetch_one(&mut *db).await.map_err(err)?;
    if created == 0 {
        return Ok(false);
    }
    Ok(!list(db, NS, "artifact:")
        .await?
        .iter()
        .any(|a| a["kind"] == "book_plan" && a["bookId"] == bid && a["applied"] == true))
}

pub fn review_required() -> Value {
    json!({"ok":false,"toolError":{"code":"author_review_required","retryable":false,
        "message":"作品还没创建。本次未创建作品或写入正式资料。请先 prepare_book_plan 展示开书方案；作者明确要求创建时调用 adopt_book_plan，也可在面板点“创建作品”。选择故事不等于授权创建作品。"}})
}

/// 界面采用与已验证的对话授权共用此入口。创建/改名、构思资料、会话引用及回执在同一事务提交。
pub async fn adopt(db: &mut SqliteConnection, aid: &str, revision: u64) -> Result<Value, String> {
    let key = format!("artifact:{aid}");
    let mut a = get(db, NS, &key).await?.ok_or("开书草案不存在")?;
    if a["kind"] != "book_plan" {
        return Err("该成果不是开书草案".into());
    }
    if a["applied"] == true {
        book(db, str_field(&a, "bookId")?).await?;
        return Ok(a);
    }
    if a["revision"].as_u64().unwrap_or(0) != revision {
        return Err("草案内容已变化，请查看最新版后采用".into());
    }
    let existing_book = a["bookId"].as_str().is_some_and(|b| !b.is_empty());
    a["bookPlan"] = normalize(&a["bookPlan"], existing_book);
    agent_contract::validate_book_plan(&a["bookPlan"])?;
    let readiness = progress(&a["bookPlan"], false, existing_book);
    if readiness["buildable"] != true {
        let blocking = items(&readiness["blocking"]).iter().filter_map(Value::as_str).collect::<Vec<_>>().join("、");
        return Err(format!("还不能建书，还没定：{blocking}"));
    }
    let title = text(&a["bookPlan"]["workingTitle"]).unwrap_or("").to_owned();
    let sid = str_field(&a, "sessionId")?.to_owned();
    let mut s = session(db, &sid).await?;
    let old_bid = a["bookId"].as_str().unwrap_or("").to_owned();
    let bid = if old_bid.is_empty() {
        service::create_book(db, &title).await?
    } else {
        service::allowed_book(db, &s, &old_bid, false).await?;
        if book(db, &old_bid).await?["title"] != a["planBase"]["title"]
            || !creation::same_story(&a["planBase"]["materials"], &creation::materials(db, &old_bid).await?)
        {
            return Err("原作品资料已变化，请重新核对草案；本次未覆盖".into());
        }
        let stamp = chrono::Utc::now().to_rfc3339();
        sqlx::query("UPDATE local_books SET title=?,payload=json_set(payload,'$.title',?,'$.updateTime',?),updateTime=? WHERE CAST(id AS TEXT)=?")
            .bind(&title).bind(&title).bind(&stamp).bind(&stamp).bind(&old_bid).execute(&mut *db).await.map_err(err)?;
        old_bid
    };
    if let Some(profile) = a["bookPlan"]["profileId"].as_str() {
        crate::agent_canon::set_book_profile(db, &bid, profile).await?;
    }
    // 简介随草案进作品：作者采用的是看到的这一版，写入前记下原简介供撤回核对
    let intro = a["bookPlan"]["intro"].as_str().map(|s| s.trim().to_owned()).filter(|s| !s.is_empty());
    if let Some(intro) = intro {
        a["previousIntro"] = book(db, &bid).await?["intro"].clone();
        let stamp = chrono::Utc::now().to_rfc3339();
        sqlx::query("UPDATE local_books SET payload=json_set(payload,'$.intro',?,'$.updateTime',?),updateTime=? WHERE CAST(id AS TEXT)=?")
            .bind(intro).bind(&stamp).bind(&stamp).bind(&bid).execute(&mut *db).await.map_err(err)?;
    }
    // Requirements belong to the adopted book, not to a transient conversation.
    let requirements_key = format!("book-requirements:{bid}");
    let mut requirements = get(db, crate::agent_canon::AUTHOR_NS, &requirements_key).await?.unwrap_or(json!({"bookId":bid}));
    for field in ["audience", "styleRequirements", "styleSuggestions"] {
        if let Some(value) = a["bookPlan"].get(field) { requirements[field] = value.clone(); }
    }
    if a["bookPlan"]["writingPreferences"].as_object().is_some_and(|p| !p.is_empty()) {
        if !requirements["writingPreferences"].is_object() { requirements["writingPreferences"] = json!({}); }
        for (key, value) in a["bookPlan"]["writingPreferences"].as_object().unwrap() {
            requirements["writingPreferences"][key] = value.clone();
        }
    }
    put(db, crate::agent_canon::AUTHOR_NS, &requirements_key, &requirements).await?;
    let plan_characters = items(&a["bookPlan"]["characters"]).to_vec();
    let plan_world = items(&a["bookPlan"]["world"]).to_vec();
    let has_brief = ["premise", "conflict", "protagonist", "opening", "outline"].iter().any(|field| text(&a["bookPlan"][field]).is_some())
        || !plan_characters.is_empty() || !plan_world.is_empty();
    if has_brief {
        let mut doc = creation::materials(db, &bid).await?;
        // 建书时把人物写进人物表、世界规矩写进作品资料：建完就能接着写，不用再问一遍。同名的不重复写。
        let mut written = Vec::new();
        for c in &plan_characters {
            let name = text(&c["name"]).unwrap_or("");
            if name.is_empty() || items(&doc["characters"]).iter().any(|x| x["name"] == name) {
                continue;
            }
            let n = items(&doc["characters"]).len() + 1;
            let mut background = text(&c["summary"]).unwrap_or("").to_owned();
            if let Some(goal) = text(&c["goal"]) {
                background.push_str(&format!("\n行动目标：{goal}"));
            }
            for (field, label) in [("motivation", "人物动机"), ("longTermGoal", "长期目标")] {
                if let Some(value) = text(&c[field]) { background.push_str(&format!("\n{label}：{value}")); }
            }
            let role = match c["role"].as_str() { Some("主角") => 0, Some("反派") => 2, _ => 1 };
            let record = json!({"id":creation::entity_id(),"bookId":bid,"sortNo":n,"name":name,"role":role,"gender":2,"age":"","appearance":"",
                "personality":text(&c["personality"]).unwrap_or(""),"background":background,"ability":text(&c["ability"]).unwrap_or(""),
                "tags":[],"aliases":[],"voice":"","groupId":null,"sourcePlanId":aid});
            written.push(record["id"].clone());
            doc["characters"].as_array_mut().ok_or("人物资料格式错误")?.push(record);
        }
        for w in &plan_world {
            let name = text(&w["name"]).unwrap_or("");
            if name.is_empty() || items(&doc["worldSettings"]).iter().any(|x| x["name"] == name) {
                continue;
            }
            let n = items(&doc["worldSettings"]).len() + 1;
            let kind = match w["type"].as_str() { Some("地理") => 0, Some("势力") => 1, Some("功法") => 2, Some("物品") => 3, Some("等级") => 4, _ => 5 };
            doc["worldSettings"].as_array_mut().ok_or("设定资料格式错误")?.push(json!({"id":creation::entity_id(),"bookId":bid,"sortNo":n,
                "name":name,"type":kind,"detail":text(&w["detail"]).unwrap_or(""),"groupId":null,"sourcePlanId":aid}));
        }
        a["characterIds"] = json!(written);
        // 整份准备按原有大纲格式保存；没定的仍明确标注，不能擅自补成正式人物事实。
        let html = outline_html(&a["bookPlan"]);
        let node = json!({"id":creation::entity_id(),"bookId":bid,"title":"开书构思","content":html,"parentId":"0","nodeType":1,"sortNo":doc["outlineNodes"].as_array().map_or(1,|v|v.len()+1),"agentBookBrief":true});
        let nodes = doc["outlineNodes"]
            .as_array_mut()
            .ok_or("大纲数据格式错误")?;
        if let Some(previous) = nodes
            .iter_mut()
            .find(|n| n["agentBookBrief"] == true || n["sourcePlanId"].is_string())
        {
            put(
                db,
                OP_NS,
                &format!("book-plan-before:{aid}:{revision}"),
                previous,
            )
            .await?;
            previous["content"] = node["content"].clone();
            previous.as_object_mut().unwrap().remove("sourcePlanId");
            previous["agentBookBrief"] = json!(true);
            a["outlineId"] = previous["id"].clone();
        } else {
            a["outlineId"] = node["id"].clone();
            nodes.push(node);
        }
        doc["updatedAt"] = json!(chrono::Utc::now().to_rfc3339());
        put(db, creation::MATERIAL_NS, &bid, &doc).await?;
    }
    // 故事走向写进第一卷的简介；作者已经写过卷简介的不覆盖
    if let Some(outline) = text(&a["bookPlan"]["outline"]) {
        if let Some(first) = service::volumes(db, &bid).await?.into_iter().next() {
            if text(&first["summary"]).is_none() {
                let mut v = first.clone();
                v["summary"] = json!(outline);
                v["updateTime"] = json!(chrono::Utc::now().to_rfc3339());
                let vid = match &v["id"] { Value::String(s) => s.clone(), other => other.to_string() };
                sqlx::query("UPDATE local_volumes SET payload=? WHERE CAST(id AS TEXT)=? AND bookId=?")
                    .bind(v.to_string()).bind(&vid).bind(&bid).execute(&mut *db).await.map_err(err)?;
            }
        }
    }
    a["bookId"] = json!(bid);
    a["bookTitle"] = json!(title);
    a["applied"] = json!(true);
    a["progress"] = progress(&a["bookPlan"], true, existing_book);
    a["adoptedRevision"] = json!(revision);
    a["revision"] = json!(revision + 1);
    a["adoptedAt"] = json!(now());
    put(db, NS, &key, &a).await?;
    put(db,OP_NS,&format!("book-plan-adopt:{aid}:{revision}"),&json!({"bookId":bid,"title":title,"planId":aid,"revision":revision,"authorAction":"adopt"})).await?;
    let refs = s["references"].as_array_mut().ok_or("会话引用格式错误")?;
    if let Some(reference) = refs
        .iter_mut()
        .find(|r| r["bookId"] == bid && r["kind"] == "book")
    {
        reference["title"] = json!(title);
    } else {
        refs.push(json!({"id":format!("book:{bid}"),"kind":"book","bookId":bid,"title":title,"description":"采用开书草案的作品"}));
    }
    // 输入框的引用也带上这本书：下一条消息照常把它带进对话，不会被输入框的旧引用列表覆盖掉
    if let Some(drafts) = s["draftReferences"].as_array_mut() {
        if let Some(reference) = drafts.iter_mut().find(|r| r["bookId"] == bid && r["kind"] == "book") {
            reference["title"] = json!(title);
        } else {
            drafts.push(json!({"id":format!("book:{bid}"),"kind":"book","bookId":bid,"title":title,"description":"采用开书草案的作品"}));
        }
    }
    s["contextBookId"] = json!(bid);
    // 作者在这件事里刚采用的书就是它的写入目标：接着写资料、写章节不用再选一次；
    // 采用之后做什么仍按作者的话来，不在这里替作者决定停下。
    s["targetBookId"] = json!(bid);
    if let Some(fields) = s.as_object_mut() {
        fields.remove("creationReviewTurn");
    }
    save_session(db, &mut s).await?;
    Ok(a)
}

#[cfg(test)]
mod workflow_regressions {
    use super::*;

    #[test]
    fn every_original_author_decision_still_blocks_new_book_until_answered_or_delegated() {
        let full = decided_plan("核验作品");
        assert_eq!(progress(&full, false, false)["buildable"], true);
        for (_, _, keys) in STEPS {
            for key in *keys {
                let mut plan = full.clone();
                plan["decisions"].as_object_mut().unwrap().remove(*key);
                let p = progress(&plan, false, false);
                assert_eq!(p["buildable"], false, "missing {key}: {p}");
                plan["decisions"][*key] = json!({"value":"模型自拟","by":"suggested"});
                assert_eq!(progress(&plan, false, false)["buildable"], false, "suggested {key}");
                plan["decisions"][*key]["by"] = json!("assumed");
                assert_eq!(progress(&plan, false, false)["buildable"], true, "delegated {key}");
            }
        }
    }

    #[test]
    fn narration_uses_new_author_field_without_accepting_default_suggestion() {
        let mut plan = decided_plan("核验作品");
        plan["decisions"].as_object_mut().unwrap().remove("narration");
        plan["writingPreferences"] = json!({"narration":{"value":"第三人称","by":"suggested"}});
        assert_eq!(progress(&plan, false, false)["current"], "protagonist");
        assert_eq!(progress(&plan, false, false)["buildable"], false);
        plan["writingPreferences"]["narration"]["by"] = json!("author");
        assert_eq!(progress(&plan, false, false)["buildable"], true);
        assert_eq!(progress(&json!({"workingTitle":"已有作品"}), false, true)["buildable"], true);
    }
}
