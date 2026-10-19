//! 作品包装：取名、简介、封面。模型出候选，宿主做确定性核对（重名、易混、对标书人名、长度），
//! 简介经作者点“保存到作品”才写进书籍；封面先只存方案，图片由作者在封面工具里生成并确认，
//! 或作者明确要出图时经 generate_cover（任何入口先弹确认卡）按方案出一张，由网页层调图片模型。
use crate::agent_store::{book, err, get, now, put, str_field, NS};
use serde_json::{json, Value};
use sqlx::SqliteConnection;
use std::collections::{BTreeMap, BTreeSet};

pub const NAME_KINDS: &[&str] = &["人物", "地点", "势力", "功法", "物品", "书名"];
pub const INTRO_MIN: usize = 20;
pub const INTRO_MAX: usize = 600;
/// 封面工具里的风格选项；空串表示自动匹配。
pub const COVER_STYLES: &[&str] = &["", "水墨玄幻", "赛博修仙", "悬疑惊悚", "古言唯美", "都市写实", "二次元"];
/// generate_cover 出图尺寸，与封面工具一致（竖版）。
pub const COVER_SIZE: &str = "768x1152";
const PERSON_NAME_MAX: usize = 4;

fn char_count(s: &str) -> usize {
    s.chars().count()
}

/// 两个名字相同的字数（按多重集合计）。
fn shared_chars(a: &str, b: &str) -> usize {
    let mut pool: BTreeMap<char, usize> = BTreeMap::new();
    for c in b.chars() {
        *pool.entry(c).or_default() += 1;
    }
    a.chars()
        .filter(|c| match pool.get_mut(c) {
            Some(n) if *n > 0 => {
                *n -= 1;
                true
            }
            _ => false,
        })
        .count()
}

/// 等长且只有一个位置不同。
fn one_char_apart(a: &str, b: &str) -> bool {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    a.len() == b.len() && a.iter().zip(&b).filter(|(x, y)| x != y).count() == 1
}

/// 逐个候选给出核对结论；空数组表示没发现问题。`existing` 是本书已有的名字（人物含别名，其他类型为设定标题或其他书名）。
pub fn name_flags(kind: &str, candidates: &[(String, String)], existing: &[String], reference: &BTreeSet<String>) -> Vec<Value> {
    let mut seen = BTreeSet::new();
    candidates
        .iter()
        .map(|(name, meaning)| {
            let mut flags = Vec::new();
            if !seen.insert(name.clone()) {
                flags.push("候选之间重复".to_owned());
            }
            if let Some(same) = existing.iter().find(|e| *e == name) {
                flags.push(format!("与已有的「{same}」重名"));
            } else if kind == "人物" {
                if let Some(close) = existing.iter().find(|e| char_count(e) >= 2 && (shared_chars(name, e) >= 2 || one_char_apart(name, e))) {
                    flags.push(format!("和「{close}」字面相近，读者容易混"));
                }
            }
            if kind == "人物" && reference.contains(name) {
                flags.push("和对标书的人物同名".to_owned());
            }
            match kind {
                "人物" if char_count(name) > PERSON_NAME_MAX => flags.push(format!("超过 {PERSON_NAME_MAX} 个字，不好记")),
                _ => {}
            }
            json!({"name":name,"meaning":meaning,"flags":flags})
        })
        .collect()
}

/// 本书已有的名字：人物取名字与别名，其余取设定标题；书名与本地其他作品比。
async fn existing_names(db: &mut SqliteConnection, kind: &str, book_id: Option<&str>) -> Result<Vec<String>, String> {
    if kind == "书名" {
        let titles: Vec<String> = sqlx::query_scalar("SELECT title FROM local_books WHERE deletedAt IS NULL AND (? IS NULL OR CAST(id AS TEXT)<>?)")
            .bind(book_id)
            .bind(book_id)
            .fetch_all(&mut *db)
            .await
            .map_err(err)?;
        return Ok(titles);
    }
    let Some(bid) = book_id else {
        return Ok(Vec::new());
    };
    let doc = crate::agent_creation::materials(db, bid).await?;
    let mut names = Vec::new();
    if kind == "人物" {
        for c in doc["characters"].as_array().into_iter().flatten() {
            names.extend(c["name"].as_str().map(str::to_owned));
            names.extend(c["aliases"].as_array().into_iter().flatten().filter_map(|a| a.as_str().map(str::to_owned)));
        }
    } else {
        for w in doc["worldSettings"].as_array().into_iter().flatten() {
            names.extend(w["title"].as_str().or(w["name"].as_str()).map(str::to_owned));
        }
    }
    names.retain(|n| !n.trim().is_empty());
    Ok(names)
}

fn book_ref<'a>(args: &'a Value) -> Option<&'a str> {
    args["book_id"].as_str().filter(|v| !v.is_empty())
}

async fn book_title(db: &mut SqliteConnection, book_id: Option<&str>) -> Result<Value, String> {
    Ok(match book_id {
        Some(bid) => book(db, bid).await?["title"].clone(),
        None => json!(""),
    })
}

fn record(session_id: &str, run_id: &str, op: &str, book_id: Option<&str>, title: Value, kind: &str, heading: String, content: String) -> Value {
    json!({"id":format!("{session_id}:{op}"),"sessionId":session_id,"runId":run_id,"bookId":book_id.unwrap_or(""),"bookTitle":title,
        "kind":kind,"title":heading,"content":content,"original":"","applied":false,"revision":0,"createdAt":now()})
}

/// 取名候选：核对后保存为名单成果，核对结论一并返回给模型，有问题的名字换掉再给作者。
pub async fn save_names(db: &mut SqliteConnection, s: &Value, run_id: &str, args: &Value) -> Result<Value, String> {
    let session_id = str_field(s, "id")?;
    let kind = str_field(args, "kind")?;
    if !NAME_KINDS.contains(&kind) {
        return Err(format!("取名类型只能是：{}", NAME_KINDS.join("、")));
    }
    let bid = book_ref(args);
    if let Some(bid) = bid {
        crate::agent_service::allowed_book(db, s, bid, false).await?;
    }
    let candidates = args["candidates"]
        .as_array()
        .ok_or("缺少候选名字")?
        .iter()
        .map(|c| (c["name"].as_str().unwrap_or("").trim().to_owned(), c["meaning"].as_str().unwrap_or("").trim().to_owned()))
        .collect::<Vec<_>>();
    if candidates.iter().any(|(name, _)| name.is_empty()) { return Err("候选名字不能为空".into()); }
    if candidates.is_empty() || candidates.len() > 20 {
        return Err("一次给 1 到 20 个候选名字".into());
    }
    let existing = existing_names(db, kind, bid).await?;
    let reference = crate::agent_breakdown::session_reference_names(db, s).await?;
    let mut checked = name_flags(kind, &candidates, &existing, &reference);
    if kind == "书名" {
        for (candidate, source) in checked.iter_mut().zip(args["candidates"].as_array().ok_or("缺少候选")?) {
            for field in ["readerPromise", "storyBasis", "tradeoff"] {
                let text = source[field].as_str().filter(|text| !text.trim().is_empty()).ok_or("书名候选需要读者期待、设定依据和命名取舍")?;
                candidate[field] = json!(text.trim());
            }
        }
    }
    let check_summary = if kind == "书名" {
        "仅检查本批候选重复和本地作品库重名；未做全网或目标平台重名检索、平台审核及点击效果验证。"
    } else if bid.is_some() {
        "仅检查本批候选和当前作品已有名称；人物名另检查字面近似、常见长度及当前引用的参考书人物名。未做全网检索。"
    } else {
        "未关联作品，仅检查本批候选重复；人物名另检查常见长度及当前引用的参考书人物名。未核对本书人物或设定，未做全网检索。"
    };
    let lines = checked
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let flags = c["flags"].as_array().map(|f| f.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("；")).unwrap_or_default();
            let meaning = c["meaning"].as_str().filter(|m| !m.is_empty()).map(|m| format!(" —— {m}")).unwrap_or_default();
            let mut line = format!("{}. {}{meaning}{}", i + 1, c["name"].as_str().unwrap_or(""), if flags.is_empty() { String::new() } else { format!("（注意：{flags}）") });
            for (key, label) in [("readerPromise", "读者期待"), ("storyBasis", "设定依据"), ("tradeoff", "命名取舍")] {
                if let Some(value) = c[key].as_str() { line.push_str(&format!("\n   {label}：{value}")); }
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n");
    let op = str_field(args, "operation_id")?;
    let title = book_title(db, bid).await?;
    let mut artifact = record(session_id, run_id, op, bid, title, "names", format!("{kind}取名候选"), lines);
    artifact["nameKind"] = json!(kind);
    artifact["nameCheckSummary"] = json!(check_summary);
    artifact["candidates"] = json!(checked);
    put(db, NS, &format!("artifact:{}", str_field(&artifact, "id")?), &artifact).await?;
    Ok(json!({"artifact_id":artifact["id"],"candidates":checked,"checkSummary":check_summary}))
}

/// 简介候选：作者点“保存到作品”后才写进书籍。
pub async fn save_intro(db: &mut SqliteConnection, s: &Value, run_id: &str, args: &Value) -> Result<Value, String> {
    let session_id = str_field(s, "id")?;
    let intro = str_field(args, "intro")?.trim().to_owned();
    let n = char_count(&intro);
    if !(INTRO_MIN..=INTRO_MAX).contains(&n) {
        return Ok(json!({"ok":false,"toolError":{"code":"intro_length","retryable":true,
            "message":format!("简介现在 {n} 字；请控制在 {INTRO_MIN}–{INTRO_MAX} 字")}}));
    }
    let bid = book_ref(args);
    if let Some(bid) = bid {
        crate::agent_service::allowed_book(db, s, bid, false).await?;
    }
    let names = crate::agent_breakdown::session_reference_names(db, s).await?;
    if let Some(error) = crate::agent_breakdown::leak_error(&intro, &names) {
        return Ok(error);
    }
    let title = book_title(db, bid).await?;
    let heading = match title.as_str().filter(|t| !t.is_empty()) {
        Some(t) => format!("《{t}》简介"),
        None => "作品简介".to_owned(),
    };
    let artifact = record(session_id, run_id, str_field(args, "operation_id")?, bid, title, "intro", heading, intro);
    put(db, NS, &format!("artifact:{}", str_field(&artifact, "id")?), &artifact).await?;
    Ok(json!({"artifact_id":artifact["id"],"chars":n,"bookId":bid}))
}

/// 封面描述：画面、构图、配色与风格。不在这里出图，作者在封面工具里生成、排版并确认。
pub async fn save_cover_brief(db: &mut SqliteConnection, s: &Value, run_id: &str, args: &Value) -> Result<Value, String> {
    let session_id = str_field(s, "id")?;
    let prompt = str_field(args, "prompt")?.trim().to_owned();
    if !(10..=800).contains(&char_count(&prompt)) {
        return Ok(json!({"ok":false,"toolError":{"code":"prompt_length","retryable":true,"message":"封面描述请写 10–800 字：主体、场景、构图、配色"}}));
    }
    let style = args["style"].as_str().unwrap_or("");
    if !COVER_STYLES.contains(&style) {
        return Ok(json!({"ok":false,"toolError":{"code":"unknown_style","retryable":true,
            "message":format!("风格只能是：{}（留空为自动匹配）", COVER_STYLES[1..].join("、"))}}));
    }
    let bid = book_ref(args);
    if let Some(bid) = bid {
        crate::agent_service::allowed_book(db, s, bid, false).await?;
    }
    let title = book_title(db, bid).await?;
    let heading = match title.as_str().filter(|t| !t.is_empty()) {
        Some(t) => format!("《{t}》封面方案"),
        None => "封面方案".to_owned(),
    };
    let mut artifact = record(session_id, run_id, str_field(args, "operation_id")?, bid, title, "cover", heading, prompt);
    artifact["coverStyle"] = json!(style);
    put(db, NS, &format!("artifact:{}", str_field(&artifact, "id")?), &artifact).await?;
    Ok(json!({"artifact_id":artifact["id"],"style":style}))
}

fn tool_error(code: &str, message: &str) -> Value {
    json!({"ok":false,"toolError":{"code":code,"retryable":false,"message":message}})
}

/// 作者在模型管理里配置的图片模型：优先“图像生成”分组选定的那个，否则排在最前的启用图片模型。
/// 只回编号与名字；密钥留在网页层自己读，宿主不碰。
pub async fn image_model(db: &mut SqliteConnection) -> Result<Option<Value>, String> {
    let Some(raw) = get(db, "app-settings", "ew-local-ai-models").await? else {
        return Ok(None);
    };
    let settings: Value = serde_json::from_str(raw.as_str().ok_or("模型设置格式无效")?).map_err(err)?;
    let models = settings["models"].as_array().cloned().unwrap_or_default();
    let mut enabled = models.iter().filter(|m| m["scene"] == "image" && m["status"] != 0).collect::<Vec<_>>();
    enabled.sort_by_key(|m| (m["sort"].as_i64().unwrap_or(0), m["id"].as_i64().unwrap_or(0)));
    let preferred = settings["preferences"]["image_generation"].as_str().filter(|v| !v.is_empty());
    let chosen = preferred
        .and_then(|code| enabled.iter().find(|m| m["id"].to_string() == code).copied())
        .or_else(|| enabled.first().copied());
    Ok(chosen.map(|m| {
        let name = m["name"].as_str().filter(|v| !v.trim().is_empty()).or(m["modelCode"].as_str()).unwrap_or("图片模型");
        json!({"code":m["id"].to_string(),"name":name})
    }))
}

/// 出图前的预检：方案存在且是封面方案、作品可访问、有图片模型。通过时返回交给网页层的任务（不含任何密钥）。
pub async fn cover_job(db: &mut SqliteConnection, s: &Value, args: &Value) -> Result<Result<Value, Value>, String> {
    let aid = str_field(args, "artifact_id")?;
    let Some(a) = get(db, NS, &format!("artifact:{aid}")).await? else {
        return Ok(Err(tool_error("artifact_not_found", "没有这份封面方案；先用 request_cover 保存方案，再按返回的 artifact_id 出图。")));
    };
    if a["kind"] != "cover" || a["deletedAt"].is_number() {
        return Ok(Err(tool_error("not_cover_brief", "这份成果不是封面方案，不能按它出图。")));
    }
    let prompt = a["content"].as_str().unwrap_or("").trim().to_owned();
    if prompt.is_empty() {
        return Ok(Err(tool_error("empty_prompt", "封面方案没有画面描述，先补全方案再出图。")));
    }
    let bid = a["bookId"].as_str().filter(|v| !v.is_empty());
    if let Some(bid) = bid {
        crate::agent_service::allowed_book(db, s, bid, false).await?;
    }
    let Some(model) = image_model(db).await? else {
        return Ok(Err(tool_error("no_image_model", "还没有可用的图片模型：请作者到桌面端「模型管理」添加并启用一个图片模型后再出图。")));
    };
    Ok(Ok(json!({"artifactId":aid,"prompt":prompt,"style":a["coverStyle"].as_str().unwrap_or(""),
        "bookId":bid.unwrap_or(""),"bookTitle":a["bookTitle"].as_str().unwrap_or(""),
        "regenerate":a["coverUrl"].as_str().is_some_and(|v| !v.is_empty()),
        "modelCode":model["code"],"modelName":model["name"],"size":COVER_SIZE})))
}

/// 把简介写进书籍或撤回；撤回时恢复写入前的简介。
pub async fn apply_intro(db: &mut SqliteConnection, a: &mut Value, undo: bool) -> Result<(), String> {
    let bid = str_field(a, "bookId")?.to_owned();
    if bid.is_empty() {
        return Err("这份简介没有关联作品，请在对话里说明要用在哪本书".into());
    }
    let intro = if undo {
        a["previousIntro"].as_str().unwrap_or("").to_owned()
    } else {
        a["previousIntro"] = book(db, &bid).await?["intro"].clone();
        a["content"].as_str().unwrap_or("").to_owned()
    };
    let stamp = chrono::Utc::now().to_rfc3339();
    sqlx::query("UPDATE local_books SET payload=json_set(payload,'$.intro',?,'$.updateTime',?),updateTime=? WHERE CAST(id AS TEXT)=?")
        .bind(&intro)
        .bind(&stamp)
        .bind(&stamp)
        .bind(&bid)
        .execute(&mut *db)
        .await
        .map_err(err)?;
    a["applied"] = json!(!undo);
    Ok(())
}

/// 作者在封面工具里生成并确认后，把封面地址记在方案上。
pub async fn attach_cover(db: &mut SqliteConnection, artifact_id: &str, url: &str) -> Result<Value, String> {
    let key = format!("artifact:{artifact_id}");
    let mut a = get(db, NS, &key).await?.ok_or("成果不存在")?;
    if a["kind"] != "cover" {
        return Err("不是封面方案".into());
    }
    a["coverUrl"] = json!(url);
    a["revision"] = json!(a["revision"].as_u64().unwrap_or(0) + 1);
    put(db, NS, &key, &a).await?;
    Ok(a)
}
