//! 创作领域操作：候选仍属于 Agent，正式资料复用编辑器 book-reference 文档。
use crate::agent_store::*;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::SqliteConnection;
use std::{
    path::Path,
    sync::atomic::{AtomicI64, Ordering},
};

pub const MATERIAL_NS: &str = "ew-local-reference/book-reference";
static LAST_ID: AtomicI64 = AtomicI64::new(0);

pub(crate) fn entity_id() -> i64 {
    // 与原书库负数 ID 约定一致；同毫秒的多条人物/目录也各有独立身份。
    let time = -now() * 1000 - 500;
    let old = LAST_ID
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |old| {
            Some(time.min(old - 1))
        })
        .unwrap();
    time.min(old - 1)
}

pub fn version(value: &Value) -> String {
    format!("{:x}", Sha256::digest(value.to_string().as_bytes()))
}

/// 判断写作依据、记忆、草案是否过期时用的资料版本：只看故事内容。
/// 人物关系图与故事线画布上的坐标、文档保存时间是界面布局，拖动节点不该让候选和记忆失效。
pub fn story_version(doc: &Value) -> String {
    let mut story = doc.clone();
    if let Some(fields) = story.as_object_mut() {
        fields.remove("characterCanvasPositions");
        fields.remove("updatedAt");
    }
    for node in story["storylineNodes"].as_array_mut().into_iter().flatten() {
        if let Some(fields) = node.as_object_mut() {
            for key in ["positionX", "positionY", "updateTime"] {
                fields.remove(key);
            }
        }
    }
    version(&story)
}

/// 记录里的资料版本是否仍对应当前故事内容；升级前按整份资料记的版本也认，已有候选不因换算口径而失效。
pub fn same_story(recorded: &Value, doc: &Value) -> bool {
    recorded.as_str().is_some_and(|v| v == story_version(doc) || v == version(doc))
}

pub async fn materials(db: &mut SqliteConnection, bid: &str) -> Result<Value, String> {
    book(db, bid).await?;
    let mut doc = json!({"version":1,"bookId":bid,"outlineNodes":[],"characters":[],
        "characterGroups":[],"characterRelations":[],"characterCanvasPositions":{},"worldSettings":[],
        "worldSettingGroups":[],"timelineEvents":[],"storylines":[],"storylineRelations":[],
        "storylineNodes":[],"storylineNodeRelations":[],"plotBindings":[],"updatedAt":""});
    if let Some(stored) = get(db, MATERIAL_NS, bid).await? {
        let fields = stored.as_object().ok_or("作品资料格式无效")?;
        doc.as_object_mut().unwrap().extend(fields.clone());
    }
    Ok(doc)
}

/// 人物编号 → 名字；时间线、故事线里的人物编号换成名字给模型看。
fn character_names(doc: &Value) -> std::collections::BTreeMap<String, String> {
    doc["characters"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| Some((c["id"].to_string().trim_matches('"').to_owned(), c["name"].as_str()?.to_owned())))
        .collect()
}

fn names_of(ids: &Value, names: &std::collections::BTreeMap<String, String>) -> Vec<String> {
    ids.as_array()
        .into_iter()
        .flatten()
        .filter_map(|id| names.get(id.to_string().trim_matches('"')).cloned())
        .collect()
}

/// 写作台时间线：按故事时间排序，去掉画布与账号字段。
pub fn plot_timeline(doc: &Value) -> Value {
    let names = character_names(doc);
    let storylines: std::collections::BTreeMap<String, String> = doc["storylines"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|l| Some((l["id"].to_string().trim_matches('"').to_owned(), l["title"].as_str()?.to_owned())))
        .collect();
    let mut events = doc["timelineEvents"].as_array().cloned().unwrap_or_default();
    events.sort_by_key(|e| e["timeOrder"].as_i64().unwrap_or(0));
    let events = events
        .iter()
        .map(|e| {
            json!({"id":e["id"],"title":e["title"],"timeLabel":e["timeLabel"],"timePoint":e["timePoint"],"timeOrder":e["timeOrder"],
                "parallelGroupId":e["parallelGroupId"],"location":e["location"],"summary":e["summary"],"cause":e["cause"],"effect":e["effect"],
                "chapterId":e["chapterId"],"storyline":e["storylineId"].as_str().and_then(|id| storylines.get(id)),
                "characters":names_of(&e["characterIds"], &names),"conflictLevel":e["conflictLevel"]})
        })
        .collect::<Vec<_>>();
    json!({"count":events.len(),"events":events,
        "note":if events.is_empty() { "作者还没有在写作台维护时间线" } else { "作者维护的时间线；与正文冲突时以正文和作者决定为准" }})
}

/// 写作台故事线、节点与人物关系：节点按故事线与排序号排列，人物编号换成名字。
pub fn plot_storylines(doc: &Value) -> Value {
    let names = character_names(doc);
    let storylines = doc["storylines"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|l| {
            json!({"id":l["id"],"title":l["title"],"lineType":l["lineType"],"status":l["status"],"importance":l["importance"],
                "chapterRange":l["chapterRangeText"],"goal":l["goal"],"conflict":l["conflict"],"summary":l["summary"],
                "firstChapterId":l["firstChapterId"],"revealChapterId":l["revealChapterId"],"keyCharacters":names_of(&l["keyCharacterIds"], &names)})
        })
        .collect::<Vec<_>>();
    let mut nodes = doc["storylineNodes"].as_array().cloned().unwrap_or_default();
    nodes.sort_by_key(|n| (n["storylineId"].to_string(), n["sortNo"].as_i64().unwrap_or(0)));
    let nodes = nodes
        .iter()
        .map(|n| {
            json!({"id":n["id"],"storylineId":n["storylineId"],"title":n["title"],"nodeType":n["nodeType"],"status":n["status"],
                "summary":n["summary"],"location":n["location"],"chapterIds":n["chapterIds"],"characters":names_of(&n["characterIds"], &names),
                "predecessors":n["predecessorNodeIds"],"successors":n["successorNodeIds"]})
        })
        .collect::<Vec<_>>();
    let name_of = |id: &Value| names.get(id.to_string().trim_matches('"')).cloned().unwrap_or_else(|| id.to_string());
    let relations = doc["characterRelations"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| json!({"from":name_of(&r["fromId"]),"to":name_of(&r["toId"]),"type":r["relationType"],"label":r["label"]}))
        .collect::<Vec<_>>();
    json!({"storylines":storylines,"nodes":nodes,"storylineRelations":doc["storylineRelations"],
        "nodeRelations":doc["storylineNodeRelations"],"characterRelations":relations})
}

pub fn collection(kind: &str) -> Result<&'static str, String> {
    match kind {
        "character" => Ok("characters"),
        "outline" => Ok("outlineNodes"),
        "world_setting" => Ok("worldSettings"),
        _ => Err("不支持的资料类型".into()),
    }
}

pub fn validate_fields(kind: &str, fields: &Value) -> Result<(), String> {
    collection(kind)?;
    if fields.to_string().len() > 120_000 {
        return Err("资料内容超过单次保存上限".into());
    }
    crate::agent_contract::validate_material(kind, fields)
}

pub fn material_content(kind: &str, fields: &Value) -> String {
    let labels = [
        ("name", "名称"),
        ("title", "标题"),
        ("age", "年龄"),
        ("appearance", "外貌"),
        ("personality", "性格"),
        ("background", "背景"),
        ("ability", "能力"),
        ("aliases", "别名"),
        ("voice", "语言指纹"),
        ("content", "内容"),
        ("detail", "设定"),
    ];
    let lines = labels
        .into_iter()
        .filter_map(|(key, label)| match &fields[key] {
            Value::String(s) if !s.is_empty() => Some(format!("{label}：{s}")),
            Value::Array(items) if !items.is_empty() => Some(format!(
                "{label}：{}",
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("、")
            )),
            _ => None,
        })
        .collect::<Vec<_>>();
    if lines.is_empty() {
        format!("{kind} 资料变更")
    } else {
        lines.join("\n\n")
    }
}

pub async fn save_material_candidate(
    root: &Path,
    sid: &str,
    args: &Value,
) -> Result<Value, String> {
    let mut db = crate::desktop_storage::connect_existing_at(root).await?;
    let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
    let bid = str_field(args, "book_id")?;
    let s = session(&mut tx, sid).await?;
    crate::agent_service::allowed_book(&mut tx, &s, bid, false).await?;
    let kind = str_field(args, "material_kind")?;
    validate_fields(kind, &args["fields"])?;
    let key = format!("artifact:{sid}:{}", str_field(args, "operation_id")?);
    if let Some(saved) = get(&mut tx, NS, &key).await? {
        if saved["request"] != *args {
            return Err("重复资料操作编号对应不同内容".into());
        }
        return Ok(saved);
    }
    // 同一件事里前几轮读过的资料快照仍可作保存基准（快照带版本，作者中途改过照样能核出来），
    // 答完一张卡不必为保存再读一遍
    let mut pointer = None;
    for run in crate::agent_task::runs(&s).into_iter().rev() {
        pointer = get(&mut tx, OP_NS, &format!("material-read:{sid}:{run}:{bid}")).await?;
        if pointer.is_some() {
            break;
        }
    }
    let base = if let Some(pointer) = pointer {
        get(&mut tx, OP_NS, str_field(&pointer,"snapshotKey")?).await?
    } else if args["expected_version"].is_string() {
        // Existing saved tool calls retain the version they actually read.
        get(&mut tx, OP_NS, &format!("read-materials:{sid}:{bid}")).await?
    } else { None };
    let Some(base) = base else {
        return Ok(crate::agent_tool_boundary::failure("material_read_required",crate::agent_tool_boundary::FailureKind::Precondition,
            "这件事里还没有读取过作品资料，请先读取资料后继续，未执行保存。",true,"read_book_materials",json!({"bookId":bid})));
    };
    if args["expected_version"].is_string() && args["expected_version"] != version(&base) {
        return Ok(crate::agent_tool_boundary::failure("material_read_mismatch",crate::agent_tool_boundary::FailureKind::Conflict,
            "提交的资料版本与读取快照不一致，未执行保存。请核对原读取结果，不要猜测版本或覆盖作者修改。",false,"read_book_materials",json!({"bookId":bid})));
    }
    let target = args["entity_id"].as_i64();
    if !args["entity_id"].is_null() && target.is_none() {
        return Err("资料编号格式错误".into());
    }
    let original = if let Some(target) = target {
        base[collection(kind)?]
            .as_array()
            .and_then(|items| items.iter().find(|i| i["id"] == target))
            .cloned()
            .ok_or("原资料不存在或不属于当前作品")?
    } else {
        Value::Null
    };
    let artifact = json!({"id":format!("{sid}:{}",str_field(args,"operation_id")?),"sessionId":sid,
        "bookId":bid,"bookTitle":book(&mut tx,bid).await?["title"],"title":str_field(args,"title")?,
        "kind":"material","materialKind":kind,"materialFields":args["fields"],"materialBase":original,
        "entityId":target,"original":if original.is_null(){String::new()}else{material_content(kind,&original)},
        "content":material_content(kind,&args["fields"]),"materialSnapshotVersion":version(&base),"applied":false,"revision":0,"request":args});
    put(&mut tx, NS, &key, &artifact).await?;
    tx.commit().await.map_err(err)?;
    Ok(artifact)
}

/// 逐条比较原资料，允许其他条目的并发更新；候选与操作回执在同一事务内变为已应用。
pub async fn apply_material(root: &Path, aid: &str) -> Result<Value, String> {
    apply_material_checked(root, aid, None).await
}

pub async fn apply_material_checked(
    root: &Path,
    aid: &str,
    expected: Option<u64>,
) -> Result<Value, String> {
    let mut db = crate::desktop_storage::connect_existing_at(root).await?;
    let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
    let key = format!("artifact:{aid}");
    let mut artifact = get(&mut tx, NS, &key).await?.ok_or("成果不存在")?;
    if artifact["applied"] == true {
        return Ok(artifact);
    }
    if expected.is_some_and(|revision| revision != artifact["revision"].as_u64().unwrap_or(0)) {
        return Err("成果已被作者编辑，请读取最新成果后继续".into());
    }
    let bid = str_field(&artifact, "bookId")?.to_owned();
    let s = session(&mut tx, str_field(&artifact, "sessionId")?).await?;
    crate::agent_service::allowed_book(&mut tx, &s, &bid, false).await?;
    let kind = str_field(&artifact, "materialKind")?.to_owned();
    let fields = &artifact["materialFields"];
    validate_fields(&kind, fields)?;
    let mut doc = materials(&mut tx, &bid).await?;
    let list_key = collection(&kind)?;
    let existing_id = artifact["entityId"].as_i64();
    let entries = doc[list_key].as_array().ok_or("作品资料列表格式错误")?;
    let before = if let Some(eid) = existing_id {
        entries
            .iter()
            .find(|e| e["id"] == eid)
            .cloned()
            .ok_or("资料已删除，候选已保留")?
    } else {
        Value::Null
    };
    if before != artifact["materialBase"] {
        return Err("资料版本冲突：作者已修改该条资料，候选已保留，未覆盖新内容".into());
    }
    let mut after = if before.is_null() {
        let common = json!({"id":entity_id(),"bookId":bid,"sortNo":entries.len()+1});
        let mut entity = match kind.as_str() {
            "character" => {
                json!({"name":"","role":0,"gender":2,"age":"","appearance":"","personality":"","background":"","ability":"","tags":[],"aliases":[],"voice":"","groupId":null})
            }
            "outline" => json!({"title":"","content":"","parentId":"0","nodeType":1}),
            _ => json!({"name":"","type":5,"detail":"","groupId":null}),
        };
        entity
            .as_object_mut()
            .unwrap()
            .extend(common.as_object().unwrap().clone());
        entity
    } else {
        before.clone()
    };
    after
        .as_object_mut()
        .unwrap()
        .extend(fields.as_object().unwrap().clone());
    let label = if kind == "outline" { "title" } else { "name" };
    if after[label].as_str().unwrap_or("").trim().is_empty() {
        return Err("资料名称不能为空".into());
    }
    if kind == "outline" {
        let mut parent = after["parentId"].as_str().unwrap_or("0").to_owned();
        let mut visited = std::collections::HashSet::new();
        visited.insert(after["id"].to_string());
        while parent != "0" {
            if !visited.insert(parent.clone()) {
                return Err("大纲层级不能循环引用".into());
            }
            let node = entries
                .iter()
                .find(|e| e["id"].to_string() == parent && e["nodeType"] == 0)
                .ok_or("大纲目录不存在")?;
            parent = node["parentId"].as_str().unwrap_or("0").to_owned();
        }
    }
    let entries = doc[list_key].as_array_mut().unwrap();
    if let Some(eid) = existing_id {
        *entries.iter_mut().find(|e| e["id"] == eid).unwrap() = after.clone();
    } else {
        entries.push(after.clone());
    }
    doc["updatedAt"] = json!(chrono::Utc::now().to_rfc3339());
    put(&mut tx, MATERIAL_NS, &bid, &doc).await?;
    artifact["applied"] = json!(true);
    artifact["saved"] = json!(true);
    artifact["materialRevision"] = json!(story_version(&doc));
    artifact["entityId"] = after["id"].clone();
    artifact["revision"] = json!(artifact["revision"].as_u64().unwrap_or(0) + 1);
    put(&mut tx, NS, &key, &artifact).await?;
    put(
        &mut tx,
        OP_NS,
        &format!("material:{aid}"),
        &json!({"bookId":bid,"kind":kind,"before":before,"after":after,"artifactId":aid}),
    )
    .await?;
    tx.commit().await.map_err(err)?;
    Ok(artifact)
}

pub async fn create_catalog_entry(
    root: &Path,
    sid: &str,
    kind: &str,
    args: &Value,
) -> Result<Value, String> {
    let mut db = crate::desktop_storage::connect_existing_at(root).await?;
    let mut tx = crate::desktop_storage::begin_write(&mut db).await?;
    let bid = str_field(args, "book_id")?;
    let s = session(&mut tx, sid).await?;
    crate::agent_service::allowed_book(&mut tx, &s, bid, true).await?;
    let op = format!("catalog:{sid}:{}", str_field(args, "operation_id")?);
    if let Some(old) = get(&mut tx, OP_NS, &op).await? {
        if old["request"] != *args || old["kind"] != kind {
            return Err("重复操作编号对应不同目录请求".into());
        }
        return Ok(old["result"].clone());
    }
    let title = str_field(args, "title")?.trim();
    if title.is_empty() || title.chars().count() > 120 {
        return Err("卷章标题应为 1—120 字".into());
    }
    let volume = args["volume_id"].as_str();
    if kind == "chapter" {
        let vid = volume.ok_or("缺少卷编号")?;
        let exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM local_volumes WHERE bookId=? AND CAST(id AS TEXT)=? AND deletedAt IS NULL")
            .bind(bid).bind(vid).fetch_one(&mut *tx).await.map_err(err)?;
        if exists != 1 {
            return Err("卷不存在或不属于当前作品".into());
        }
    }
    let query = if kind == "volume" {
        "SELECT COALESCE(MAX(sortNo),0)+1 FROM local_volumes WHERE bookId=? AND deletedAt IS NULL"
    } else {
        "SELECT COALESCE(MAX(sortNo),0)+1 FROM local_chapters WHERE bookId=? AND volumeId=? AND deletedAt IS NULL"
    };
    let mut query = sqlx::query_scalar::<_, i64>(query).bind(bid);
    if kind == "chapter" {
        query = query.bind(volume);
    }
    let sort = query.fetch_one(&mut *tx).await.map_err(err)?;
    let eid = entity_id();
    let stamp = chrono::Utc::now().to_rfc3339();
    let mut item = json!({"id":eid,"bookId":bid,"title":title,"summary":args["summary"].as_str().unwrap_or(""),"sortNo":sort,"type":kind,"createTime":stamp,"updateTime":stamp,"deletedAt":null});
    if kind == "chapter" {
        item["volumeId"] = json!(volume);
        item["wordCount"] = json!(0);
        item["textWordCount"] = json!(0);
        sqlx::query("INSERT INTO local_chapters(id,bookId,volumeId,payload,deletedAt,sortNo) VALUES(?,?,?,?,NULL,?)")
            .bind(eid).bind(bid).bind(volume).bind(item.to_string()).bind(sort).execute(&mut *tx).await.map_err(err)?;
    } else {
        sqlx::query(
            "INSERT INTO local_volumes(id,bookId,payload,deletedAt,sortNo) VALUES(?,?,?,NULL,?)",
        )
        .bind(eid)
        .bind(bid)
        .bind(item.to_string())
        .bind(sort)
        .execute(&mut *tx)
        .await
        .map_err(err)?;
    }
    sqlx::query("UPDATE local_books SET payload=json_set(payload,'$.chapterCount',(SELECT COUNT(*) FROM local_chapters WHERE bookId=? AND deletedAt IS NULL),'$.updateTime',?),updateTime=? WHERE CAST(id AS TEXT)=?")
        .bind(bid).bind(&stamp).bind(&stamp).bind(bid).execute(&mut *tx).await.map_err(err)?;
    let result = json!({"created":true,"bookId":bid,"entry":item});
    put(
        &mut tx,
        OP_NS,
        &op,
        &json!({"kind":kind,"request":args,"result":result}),
    )
    .await?;
    tx.commit().await.map_err(err)?;
    Ok(result)
}
