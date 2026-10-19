//! 审批门：按通道档案与决策 D4 决定哪些写操作要先经作者在确认卡上点选。
//! 确认卡由宿主生成（问题写明真实对象与数量），作者的点选由宿主核对；模型参数、聊天措辞都不构成授权。
//! 三条删除在任何入口都确认；approval=always 的入口（微信）对作品数据的写入一律确认。
//! 稿子比正文旧（作者在稿子生成后改过这一章）时，写入会覆盖这些改动：任何入口都先问，问题里写明覆盖。
use crate::{agent_channel, agent_prompt, agent_store::*};
use serde_json::{json, Value};
use sqlx::SqliteConnection;

/// 任何入口都先问：三条删除，建立 / 删除自动任务（建立须显示频率与估算用量，决策 D13），以及花钱的出图
const ALWAYS: [&str; 6] = ["delete_chapter", "delete_volume", "delete_book", "create_automation", "delete_automation", "generate_cover"];
const CHANNEL_WRITES: [&str; 10] = [
    "save_chapter",
    "create_volume",
    "create_chapter",
    "apply_creation_artifact",
    "save_material",
    "update_volume",
    "update_chapter",
    "update_book",
    "add_inspiration",
    "book_plan_adopt_begin",
];
/// 作者点了确认后，同一操作在这段时间内重试不再重复询问（如首次执行因版本冲突失败、重读后再提交）
const APPROVAL_TTL_MS: i64 = 30 * 60 * 1000;
pub const CONFIRM: &str = "确认";
pub const CANCEL: &str = "取消";

/// 这次调用要不要先问作者
pub fn requires(channel: &Value, name: &str, args: &Value) -> bool {
    if ALWAYS.contains(&name) {
        return true;
    }
    if channel["approval"] != "always" || !CHANNEL_WRITES.contains(&name) {
        return false;
    }
    // 资料候选（draft）不是写入，只有正式保存才确认
    name != "save_material" || args["draft"] == false
}

/// 同一组参数算同一次操作；操作编号每次调用都不同，不参与比对
fn fingerprint(name: &str, args: &Value) -> String {
    let mut intent = args.clone();
    if let Some(fields) = intent.as_object_mut() {
        fields.remove("operation_id");
    }
    crate::agent_creation::version(&json!([name, intent]))
}

fn declined() -> Value {
    json!({"status":"declined","authorDecision":"cancelled",
        "message":"作者在确认卡上选择了取消，本次没有执行。不要再次发起同一操作，除非作者重新提出。"})
}

fn not_confirmed(reply: &str) -> Value {
    json!({"status":"not_confirmed","authorReply":reply,
        "message":"作者没有在确认卡上点选确认，本次没有执行。按作者的回复继续；作者明确要求时再调用会重新发出确认。"})
}

/// 放行返回 None；要先问作者时返回 confirmation_required，作者已拒绝或答了别的则返回对应结果。
/// `facts` 是宿主核对保存依据后得出的事实（见 `agent_service::ChapterSaveBasis::facts`），没有时传 Null。
pub async fn gate(
    db: &mut SqliteConnection,
    s: &Value,
    run: &str,
    name: &str,
    args: &Value,
    facts: &Value,
) -> Result<Option<Value>, String> {
    let channel = agent_channel::of_session(s);
    if !requires(&channel, name, args) && facts["overwritesEdits"] != true {
        return Ok(None);
    }
    // 起草、修订工具交付后的自动保存不发确认卡：要问作者的情况（通道要求逐次确认、会覆盖生成期间的改动）如实退回，稿子留在对话里，
    // 作者要求写入时再由 save_chapter 正常发卡。这个参数只会让宿主少写，不会让它多写。
    if args["automatic"] == true {
        let reason = if facts["overwritesEdits"] == true {
            "这一章在稿子生成期间改过，自动保存不覆盖这些改动；作者确认覆盖后再用 save_chapter 写入"
        } else {
            "当前入口要求作者在确认卡上点选后才写入；作者要求写入时用 save_chapter 发起确认"
        };
        return Ok(Some(json!({"ok":false,"saved":false,"toolError":{"code":"confirmation_required","retryable":false,"message":reason}})));
    }
    let sid = str_field(s, "id")?;
    let print = fingerprint(name, args);
    let prefix = format!("approval:{sid}:");
    // 同一操作最近一次发出的确认
    let mut records = list(db, OP_NS, &prefix)
        .await?
        .into_iter()
        .filter(|r| r["fingerprint"] == print)
        .collect::<Vec<_>>();
    records.sort_by_key(|r| r["issuedAt"].as_i64().unwrap_or(0));
    if let Some(mut record) = records.pop() {
        let token = record["token"].as_str().unwrap_or("").to_owned();
        let key = format!("{prefix}{token}");
        match record["decision"].as_str() {
            Some("approved") if now() - record["decidedAt"].as_i64().unwrap_or(0) < APPROVAL_TTL_MS => {
                return Ok(None);
            }
            // 用过一次的其他结论：重新发确认
            Some(_) => {}
            None => match agent_prompt::confirmation_answer(s, &token) {
                // 旧卡没作答就被新消息顶掉了：换新卡重新问
                None => {}
                Some((Some(_), reply)) if reply == CONFIRM => {
                    record["decision"] = json!("approved");
                    record["decidedAt"] = json!(now());
                    put(db, OP_NS, &key, &record).await?;
                    return Ok(None);
                }
                Some((Some(_), _)) => {
                    record["decision"] = json!("cancelled");
                    record["decidedAt"] = json!(now());
                    put(db, OP_NS, &key, &record).await?;
                    return Ok(Some(declined()));
                }
                Some((None, reply)) => {
                    record["decision"] = json!("unanswered");
                    record["decidedAt"] = json!(now());
                    put(db, OP_NS, &key, &record).await?;
                    return Ok(Some(not_confirmed(&reply)));
                }
            },
        }
    }
    let token = id();
    let question = describe(db, s, name, args, facts).await?;
    put(
        db,
        OP_NS,
        &format!("{prefix}{token}"),
        &json!({"token":token,"tool":name,"fingerprint":print,"runId":run,"issuedAt":now()}),
    )
    .await?;
    Ok(Some(json!({"status":"confirmation_required","confirmation":{
        "token":token,"question":question,"choices":[CONFIRM, CANCEL],
        "prompt":{"kind":"confirm","question":question,
            "options":[{"id":"yes","title":CONFIRM},{"id":"no","title":CANCEL}],"allowFreeText":false}}})))
}

fn id_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

fn clip(text: &str, max: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max {
        trimmed.to_owned()
    } else {
        format!("{}…", trimmed.chars().take(max).collect::<String>())
    }
}

async fn book_title(db: &mut SqliteConnection, bid: &str) -> String {
    book(db, bid)
        .await
        .ok()
        .and_then(|b| b["title"].as_str().map(str::to_owned))
        .unwrap_or_else(|| "这本书".to_owned())
}

/// "第N章《标题》"：N 是目录位置，与界面和微信里的说法一致
async fn chapter_label(db: &mut SqliteConnection, bid: &str, cid: &str) -> (String, i64) {
    let entries = catalog(db, bid).await.unwrap_or_default();
    match entries.iter().position(|e| id_text(&e["id"]) == cid) {
        Some(i) => (
            format!("第{}章《{}》", i + 1, entries[i]["title"].as_str().unwrap_or("")),
            entries[i]["wordCount"].as_i64().unwrap_or(0),
        ),
        None => ("这一章".to_owned(), 0),
    }
}

async fn volume_label(db: &mut SqliteConnection, bid: &str, vid: &str) -> (String, usize) {
    let volumes = crate::agent_service::volumes(db, bid).await.unwrap_or_default();
    let entries = catalog(db, bid).await.unwrap_or_default();
    match volumes.iter().position(|v| id_text(&v["id"]) == vid) {
        Some(i) => (
            format!("第{}卷《{}》", i + 1, volumes[i]["title"].as_str().unwrap_or("")),
            entries.iter().filter(|e| id_text(&e["volumeId"]) == vid).count(),
        ),
        None => ("这一卷".to_owned(), 0),
    }
}

fn material_label(kind: &str) -> &'static str {
    match kind {
        "character" => "人物",
        "outline" => "大纲",
        "world_setting" => "设定",
        _ => "资料",
    }
}

/// 确认卡的问题：写明真实对象、数量与后果，作者一眼能判断
async fn describe(db: &mut SqliteConnection, s: &Value, name: &str, args: &Value, facts: &Value) -> Result<String, String> {
    let bid = args["book_id"].as_str().unwrap_or("");
    Ok(match name {
        "save_chapter" => {
            let book = book_title(db, bid).await;
            let (chapter, _) = chapter_label(db, bid, args["chapter_id"].as_str().unwrap_or("")).await;
            let words = match args["artifact_id"].as_str().filter(|v| !v.is_empty()) {
                Some(aid) => get(db, NS, &format!("artifact:{aid}"))
                    .await?
                    .and_then(|a| a["content"].as_str().map(|c| c.chars().filter(|c| !c.is_whitespace()).count()))
                    .unwrap_or(0),
                None => args["text"].as_str().map(|c| c.chars().filter(|c| !c.is_whitespace()).count()).unwrap_or(0),
            };
            let size = if words > 0 { format!("（约 {words} 字）") } else { String::new() };
            if facts["overwritesEdits"] == true {
                let current = facts["currentWords"].as_u64().filter(|w| *w > 0).map(|w| format!("（现在约 {w} 字）")).unwrap_or_default();
                format!("把这版正文{size}写进《{book}》{chapter}？这一章在稿子生成后改过{current}，写入会覆盖这些改动，原正文会留一份快照。")
            } else {
                format!("把这版正文{size}写进《{book}》{chapter}？原正文会留一份快照。")
            }
        }
        "create_volume" => format!("在《{}》里新建一卷《{}》？", book_title(db, bid).await, clip(args["title"].as_str().unwrap_or(""), 40)),
        "create_chapter" => {
            let (volume, _) = volume_label(db, bid, args["volume_id"].as_str().unwrap_or("")).await;
            format!("在《{}》{volume}下新建一章《{}》？", book_title(db, bid).await, clip(args["title"].as_str().unwrap_or(""), 40))
        }
        "apply_creation_artifact" => {
            let artifact = get(db, NS, &format!("artifact:{}", args["artifact_id"].as_str().unwrap_or("")))
                .await?
                .unwrap_or(Value::Null);
            let abid = artifact["bookId"].as_str().unwrap_or(bid).to_owned();
            let book = book_title(db, &abid).await;
            let title = clip(artifact["title"].as_str().unwrap_or("这份成果"), 40);
            match artifact["kind"].as_str() {
                Some("material") => format!("把资料候选《{title}》写进《{book}》的作品资料？"),
                Some("chapter") => {
                    let (chapter, _) = chapter_label(db, &abid, &id_text(&artifact["chapterId"])).await;
                    if facts["overwritesEdits"] == true {
                        let current = facts["currentWords"].as_u64().filter(|w| *w > 0).map(|w| format!("（现在约 {w} 字）")).unwrap_or_default();
                        format!("把稿子《{title}》写进《{book}》{chapter}？这一章在稿子生成后改过{current}，写入会覆盖这些改动，原正文会留一份快照。")
                    } else {
                        format!("把稿子《{title}》写进《{book}》{chapter}？原正文会留一份快照。")
                    }
                }
                _ => format!("把成果《{title}》应用到《{book}》？"),
            }
        }
        "save_material" => {
            let verb = if args["entity_id"].is_null() { "新增" } else { "修改" };
            format!(
                "{verb}《{}》的{}《{}》？",
                book_title(db, bid).await,
                material_label(args["material_kind"].as_str().unwrap_or("")),
                clip(args["title"].as_str().unwrap_or(""), 40)
            )
        }
        "update_volume" | "update_chapter" => {
            let book = book_title(db, bid).await;
            let (label, noun) = if name == "update_volume" {
                (volume_label(db, bid, args["volume_id"].as_str().unwrap_or("")).await.0, ("卷名", "简介"))
            } else {
                (chapter_label(db, bid, args["chapter_id"].as_str().unwrap_or("")).await.0, ("章名", "章纲"))
            };
            let mut changes = Vec::new();
            if let Some(title) = args["title"].as_str().filter(|v| !v.trim().is_empty()) {
                changes.push(format!("{}→《{}》", noun.0, clip(title, 40)));
            }
            if let Some(summary) = args["summary"].as_str().filter(|v| !v.trim().is_empty()) {
                changes.push(format!("{}→「{}」", noun.1, clip(summary, 40)));
            }
            format!("修改《{book}》{label}：{}？", changes.join("、"))
        }
        "update_book" => {
            let old = book_title(db, bid).await;
            let mut changes = Vec::new();
            if let Some(title) = args["title"].as_str().filter(|v| !v.trim().is_empty()) {
                changes.push(format!("书名→《{}》", clip(title, 40)));
            }
            if let Some(category) = args["category"].as_str().filter(|v| !v.trim().is_empty()) {
                changes.push(format!("分类→{}", clip(category, 20)));
            }
            if let Some(tags) = args["tags"].as_array().filter(|v| !v.is_empty()) {
                changes.push(format!("标签→{}", tags.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("/")));
            }
            if let Some(intro) = args["intro"].as_str().filter(|v| !v.trim().is_empty()) {
                changes.push(format!("简介→「{}」", clip(intro, 40)));
            }
            format!("修改作品《{old}》：{}？", changes.join("、"))
        }
        "delete_chapter" => {
            let book = book_title(db, bid).await;
            let (chapter, words) = chapter_label(db, bid, args["chapter_id"].as_str().unwrap_or("")).await;
            let size = if words > 0 { format!("{words} 字，") } else { String::new() };
            format!("删除《{book}》{chapter}（{size}进回收站，桌面端可恢复）？")
        }
        "delete_volume" => {
            let book = book_title(db, bid).await;
            let (volume, count) = volume_label(db, bid, args["volume_id"].as_str().unwrap_or("")).await;
            format!("删除《{book}》{volume}及其 {count} 章（进回收站，桌面端可恢复）？")
        }
        "delete_book" => format!("把《{}》移进回收站（桌面端可恢复）？", book_title(db, bid).await),
        "create_automation" => {
            use crate::agent_scheduler as scheduler;
            let schedule = scheduler::validate_schedule(&args["schedule"]).map(|v| scheduler::describe_schedule(&v)).unwrap_or_else(|e| e);
            let deliver = args["deliver"].as_str().filter(|v| !v.is_empty()).unwrap_or(s["channel"]["kind"].as_str().unwrap_or("desktop"));
            let estimate = scheduler::estimate(s, &args["schedule"]);
            format!(
                "建立自动任务「{}」：{schedule} 运行，{}；{}。建立？",
                clip(args["title"].as_str().unwrap_or(""), 40),
                scheduler::describe_deliver(deliver),
                scheduler::describe_estimate(&estimate)
            )
        }
        "delete_automation" => {
            let task = crate::agent_scheduler::get_task(db, args["id"].as_str().unwrap_or("")).await?.unwrap_or(Value::Null);
            format!(
                "删除自动任务「{}」（{}）？它的运行记录一并删除。",
                clip(task["title"].as_str().unwrap_or("这个任务"), 40),
                crate::agent_scheduler::describe_schedule(&task["schedule"])
            )
        }
        "generate_cover" => {
            let artifact = get(db, NS, &format!("artifact:{}", args["artifact_id"].as_str().unwrap_or("")))
                .await?
                .unwrap_or(Value::Null);
            let model = crate::agent_packaging::image_model(db).await?.unwrap_or(Value::Null);
            let model_name = model["name"].as_str().filter(|v| !v.is_empty()).unwrap_or("已配置的图片模型");
            let target = match artifact["bookTitle"].as_str().filter(|v| !v.is_empty()) {
                Some(title) => format!("《{title}》"),
                None => "这份".to_owned(),
            };
            let verb = if artifact["coverUrl"].as_str().is_some_and(|v| !v.is_empty()) { "重新生成" } else { "生成" };
            let size = crate::agent_packaging::COVER_SIZE.replace('x', "×");
            format!("用图片模型「{model_name}」按{target}封面方案{verb} 1 张封面（{size}，不排书名；费用由模型商按张计费）？")
        }
        "add_inspiration" => {
            let tag = args["tag"].as_str().filter(|v| !v.trim().is_empty()).map(|t| format!("（标签：{t}）")).unwrap_or_default();
            format!("记入灵感便签：「{}」{tag}？", clip(args["content"].as_str().unwrap_or(""), 80))
        }
        "book_plan_adopt_begin" => {
            let artifact = get(db, NS, &format!("artifact:{}", args["artifact_id"].as_str().unwrap_or("")))
                .await?
                .unwrap_or(Value::Null);
            let title = clip(artifact["bookPlan"]["workingTitle"].as_str().filter(|t| !t.trim().is_empty()).unwrap_or("未定名"), 40);
            if artifact["bookId"].as_str().is_some_and(|v| !v.is_empty()) {
                format!("按开书准备更新《{title}》？")
            } else {
                format!("把《{title}》建成作品？")
            }
        }
        _ => {
            let _ = s;
            format!("执行 {name}？")
        }
    })
}
