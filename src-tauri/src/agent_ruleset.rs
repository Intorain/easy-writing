//! 任务规则集：一次写作任务开始时解析一次，规划、生成、评审共用同一编号，技能之间不再"打架"。
//! 优先级：作者在本任务里的原话 > 作者规则 > 作者选定的一个写作方法技能 > 平台画像默认。
//! 自定义技能要替换默认方法，须声明 `kind: method` 且 `replaces` 指向同一目标；同目标多个启用时要求作者二选一。
use crate::agent_canon as canon;
use crate::agent_contract::validate_document;
use crate::agent_store::{get, id, list, now, put, str_field, NS};
use serde_json::{json, Value};
use sqlx::SqliteConnection;

/// 章节写作方法的替换目标；未声明 `replaces` 的方法技能默认指向它。
pub const WRITING_TARGET: &str = "chapter-writing";
/// 短篇写作方法的替换目标；不挂作品的短篇默认在这些技能里选。
pub const SHORT_TARGET: &str = "short-story";
const RECORD_PREFIX: &str = "ruleset:";

/// Older tool responses exposed the storage key as `id`. It is an exact alias
/// of the public reference, not a different ruleset or permission scope.
pub fn canonical_id(reference: &str) -> &str {
    reference.strip_prefix(RECORD_PREFIX).unwrap_or(reference)
}

/// Keep database addressing out of the model-facing result.
/// 规划模型看到的规则集：作者规则、方法、画像、本书要求照常给。文风档案给规划模型看的是写法卡与叙事模式
/// （不含数字）；范例原文在起草时由宿主按场景交给写作模型，这里不给。旧的只有正文的文风技能按原文给规划模型。
pub fn tool_view(record: &Value) -> Value {
    let mut out = record.clone();
    if let Some(fields) = out.as_object_mut() {
        // skillInstructions 已在 expanded 里，不重复给
        for key in ["id", "sessionId", "runId", "createdAt", "skillInstructions"] { fields.remove(key); }
    }
    if record["styleSkill"].is_object() {
        out["styleSkill"] = json!({"id":record["styleSkill"]["id"],"name":record["styleSkill"]["name"],"revision":record["styleSkill"]["revision"],
            "hasExamples":record["styleSkill"]["hasExamples"]});
    }
    out
}

/// 交给写作与修订的要求：作者原话、作者规则、本书文风要求，以及保留来源标记的写作偏好。
/// 没有方法技能，没有平台画像，没有文风技能正文：写法靠范例和前文，不靠这些形容词与数字。
pub fn writer_text(record: &Value) -> String {
    let mut sections = Vec::new();
    if let Some(instruction) = record["writingInstructions"].as_str().filter(|s| !s.is_empty()) {
        sections.push(format!("【作者对本次任务的要求】\n{instruction}"));
    } else if let Some(instruction) = record["authorInstruction"].as_str().filter(|s| !s.is_empty()) {
        sections.push(format!("【作者对本次任务的要求】\n{instruction}"));
    }
    let rules = record["rules"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| r["text"].as_str().filter(|t| !t.is_empty()).map(|t| format!("- {t}")))
        .collect::<Vec<_>>();
    if !rules.is_empty() {
        sections.push(format!("【作者规则】\n{}", rules.join("\n")));
    }
    if let Some(style) = record["bookRequirements"]["styleRequirements"].as_str().filter(|s| !s.is_empty()) {
        sections.push(format!("【作者确认的本书文风要求】\n{style}"));
    }
    if record["bookRequirements"]["writingPreferences"].is_object() {
        sections.push(format!("【本书写作偏好】\n{}\n每项 by 标明来源：author 为作者明确要求，assumed 为委托拟定，suggested 为可调整建议；作者本次要求优先。", record["bookRequirements"]["writingPreferences"]));
    }
    if let Some(suggestion) = record["bookRequirements"]["styleSuggestions"].as_str().filter(|s| !s.is_empty()) {
        sections.push(format!("【AI 文风建议（可调整）】\n{suggestion}\n仅作参考；作者明确要求优先，不能把建议视为每章必须执行的模板。"));
    }
    sections.join("\n\n")
}

pub struct ResolveRequest<'a> {
    pub book_id: &'a str,
    pub skill_id: Option<&'a str>,
    /// None 继承本轮选择或作品绑定；Some("") 明确停用。
    pub style_skill_id: Option<&'a str>,
    pub author_instruction: Option<&'a str>,
    /// false 为界面预览，不写入
    pub persist: bool,
}

fn tool_error(code: &str, message: &str, extra: Value) -> Value {
    let mut error = json!({"code":code,"retryable":false,"message":message});
    if let (Some(target), Some(source)) = (error.as_object_mut(), extra.as_object()) {
        for (k, v) in source {
            target.insert(k.clone(), v.clone());
        }
    }
    json!({"ok":false,"toolError":error})
}

fn skill_target(skill: &Value) -> &str {
    skill["replaces"].as_str().unwrap_or(WRITING_TARGET)
}

/// 启用的写作方法技能（`kind == method`）；没有 kind 的技能只是补充材料，不参与方法选择。
/// 已被文件技能接替（retiredAt）的旧内置技能即使还标着启用也不算：它的方法已由接替者承担。
pub async fn method_skills(db: &mut SqliteConnection) -> Result<Vec<Value>, String> {
    Ok(list(db, NS, "skill:")
        .await?
        .into_iter()
        .filter(|s| s["enabled"] == true && !s["deletedAt"].is_number() && !s["retiredAt"].is_number() && s["kind"] == "method")
        .collect())
}

/// 文风只作表达参考，不参与主写作方法的竞争。读取文件当前版本，避免索引落后。
pub async fn style_skill(db: &mut SqliteConnection, skill_id: &str) -> Result<Value, String> {
    let Some(mut skill) = get(db, NS, &format!("skill:{skill_id}")).await? else {
        return Ok(tool_error("style_skill_unavailable", "文风技能不存在，请从技能库重新选择", json!({"skillId":skill_id})));
    };
    if skill["enabled"] != true || skill["deletedAt"].is_number() || skill["retiredAt"].is_number() {
        return Ok(tool_error("style_skill_unavailable", "文风技能已停用或删除，请重新选择或解除绑定", json!({"skillId":skill_id})));
    }
    if skill["filePath"].is_string() {
        let draft = crate::agent_skill_files::read_draft(&skill)?;
        for field in ["name", "kind", "instructions", "revision"] { skill[field] = draft[field].clone(); }
    }
    if skill["kind"] != "style" {
        return Ok(tool_error("skill_not_style", "请选择文风类型的技能；写作方法使用 skill_id", json!({"skillId":skill_id})));
    }
    // 文风档案（带范例文件）：规则集里的文风正文就是写法卡与叙事模式，给规划与评审看；范例原文另由起草时交给写作模型。
    // 旧文风技能只有正文，照原文给规划模型。
    let profile = match crate::agent_style_profile::load(db,skill_id).await {
        Ok(profile) => profile,
        Err(message) => return Ok(tool_error("style_profile_invalid",&message,json!({"skillId":skill_id}))),
    };
    let planner_text = profile.as_ref().map(crate::agent_style_profile::planner_text).unwrap_or_default();
    let instructions = if !planner_text.is_empty() { planner_text }
        else if profile.is_some() { "原文范例已核实，起草时按需提供；尚无经核实的写法卡，不把旧技能的剧情套路当作本书情节。".into() }
        else { method_text(skill["instructions"].as_str().unwrap_or("")) };
    Ok(json!({"id":skill_id,"name":skill["name"],"revision":skill["revision"],"instructions":instructions,"hasExamples":profile.is_some()}))
}

pub(crate) fn task_instruction(session: &Value) -> String {
    crate::agent_task::instruction(session)
}

fn author_said(session: &Value, instruction: &str) -> bool {
    task_instruction(session).contains(instruction)
}

pub fn profile_summary(profile: &Value) -> String {
    let words = &profile["chapterWords"];
    format!(
        "以下是平台参考建议，不是逐章必须达成的公式；作者要求与本章目标优先。参考篇幅 {}–{} 字，移动阅读段长参考 {} 字；开篇关注人物处境和阅读期待，结尾承接本章已经发生的变化。节奏与回报按故事阶段安排，过渡与日常场景不必新增危机、反转或爽点。设定释放参考：{}。读者画像：{}",
        words[0], words[1], profile["paragraphMaxChars"],
        match profile["infoReleaseLevel"].as_str().unwrap_or("mid") { "low" => "少", "high" => "多", _ => "中" },
        profile["readerPersona"].as_str().unwrap_or("")
    )
}

/// 方法技能里只写给规划 Agent 的小节：怎么在平台上落地、什么时候问作者。
/// 规则集会交给写作模型与读者评审，它们只需要写法本身，这些小节展开时去掉。
pub const PLANNER_ONLY_SECTIONS: &[&str] = &["## 在平台上怎么做", "## 什么时候问作者"];

/// 方法技能正文中交给写作与评审的部分。
pub fn method_text(instructions: &str) -> String {
    let mut out = Vec::new();
    let mut skipping = false;
    for line in instructions.lines() {
        if line.starts_with("## ") {
            skipping = PLANNER_ONLY_SECTIONS.contains(&line.trim_end());
        }
        if !skipping {
            out.push(line);
        }
    }
    out.join("\n").trim().to_owned()
}

/// 展开为模型可读的规则文本；顺序即优先级。
pub fn expand(record: &Value) -> String {
    let mut sections = Vec::new();
    if let Some(instruction) = record["writingInstructions"]
        .as_str()
        .filter(|s| !s.is_empty())
    {
        sections.push(format!("【作者对本次任务的创作要求（最高优先级）】\n{instruction}"));
    }
    let rules = record["rules"].as_array().cloned().unwrap_or_default();
    if !rules.is_empty() {
        let lines = rules
            .iter()
            .map(|r| {
                format!(
                    "- [{}] {}",
                    r["category"].as_str().unwrap_or(""),
                    r["text"].as_str().unwrap_or("")
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        sections.push(format!("【作者规则】\n{lines}"));
    }
    if let Some(name) = record["skillName"].as_str() {
        sections.push(format!(
            "【写作方法：{name}】\n{}",
            record["skillInstructions"].as_str().unwrap_or("")
        ));
    }
    sections.push(format!(
        "【平台画像：{}】\n{}",
        record["profileName"].as_str().unwrap_or(""),
        record["profileSummary"].as_str().unwrap_or("")
    ));
    if let Some(audience) = record["bookRequirements"]["audience"].as_str().filter(|s| !s.is_empty()) {
        sections.push(format!("【本书读者】\n{audience}"));
    }
    if record["bookRequirements"]["writingPreferences"].is_object() {
        sections.push(format!("【本书写作偏好】\n{}\n保留各项来源，AI 建议不能当成作者明确要求。", record["bookRequirements"]["writingPreferences"]));
    }
    if let Some(style) = record["bookRequirements"]["styleRequirements"].as_str().filter(|s| !s.is_empty()) {
        sections.push(format!("【作者确认的本书文风要求】\n{style}"));
    }
    if record["styleSkill"].is_object() {
        sections.push(format!("【参考文风：{}】\n只迁移适用的表达与叙事方法，服从作者要求和当前故事事实；参考中的人物与事件不是本书事实。\n{}",
            record["styleSkill"]["name"].as_str().unwrap_or(""), record["styleSkill"]["instructions"].as_str().unwrap_or("")));
    }
    if let Some(suggestion) = record["bookRequirements"]["styleSuggestions"].as_str().filter(|s| !s.is_empty()) {
        sections.push(format!("【AI 文风建议（可调整）】\n{suggestion}\n仅作参考；作者明确要求优先，不能把建议视为每章必须执行的模板。"));
    }
    sections.join("\n\n")
}

/// 解析规则集。冲突与未核实的原话以 `toolError` 返回，交给模型循环处理，不抛异常。
pub async fn resolve(
    db: &mut SqliteConnection,
    session: &Value,
    request: &ResolveRequest<'_>,
) -> Result<Value, String> {
    let book_id = request.book_id;
    let profile_id = canon::book_profile_id(db, book_id).await?;
    let profile = canon::platform_profile(db, &profile_id).await?;
    let stored = get(db, canon::AUTHOR_NS, &format!("book-requirements:{book_id}")).await?.unwrap_or(Value::Null);
    let mut requirements = serde_json::Map::new();
    for field in ["audience", "styleRequirements", "styleSuggestions", "styleSkillId"] {
        if let Some(value) = stored[field].as_str().filter(|s| !s.is_empty()) { requirements.insert(field.into(),json!(value)); }
    }
    if stored["writingPreferences"].is_object() { requirements.insert("writingPreferences".into(), stored["writingPreferences"].clone()); }
    let requirements = if requirements.is_empty() { Value::Null } else { Value::Object(requirements) };
    let selected = match session["selectedSkillId"].as_str().filter(|s| !s.is_empty()) {
        Some(id) => get(db, NS, &format!("skill:{id}")).await?.filter(|s| s["kind"] == "style"),
        None => None,
    };
    let style_id = request.style_skill_id
        .or_else(|| selected.as_ref().and_then(|s| s["id"].as_str()))
        .or_else(|| stored["styleSkillId"].as_str()).unwrap_or("");
    let style = if style_id.is_empty() { None } else {
        let style = style_skill(db, style_id).await?;
        if style["toolError"].is_object() { return Ok(style); }
        Some(style)
    };
    let rules = canon::list_rules(db, Some(book_id)).await?;
    let methods = method_skills(db).await?;
    let skill = match request.skill_id.filter(|s| !s.is_empty()).or_else(|| {
        session["selectedSkillId"]
            .as_str()
            .filter(|id| methods.iter().any(|s| s["id"] == *id))
    }) {
        Some(skill_id) => {
            let Some(skill) = methods.iter().find(|s| s["id"] == skill_id) else {
                return Ok(tool_error(
                    "skill_not_method",
                    "指定的技能不是已启用的写作方法技能；请选择方法技能或不指定",
                    json!({}),
                ));
            };
            Some(skill.clone())
        }
        None => {
            let target = if book_id.starts_with(canon::SHORT_PREFIX) {
                SHORT_TARGET
            } else {
                WRITING_TARGET
            };
            let candidates = methods
                .iter()
                .filter(|s| skill_target(s) == target)
                .cloned()
                .collect::<Vec<_>>();
            if candidates.len() > 1 {
                return Ok(tool_error(
                    "ruleset_conflict",
                    "多个写作方法技能同时启用且替换同一目标，请通过 ask_author 让作者选择一个后带 skill_id 重新解析",
                    json!({"candidates":candidates.iter().map(|s|json!({"id":s["id"],"name":s["name"]})).collect::<Vec<_>>()}),
                ));
            }
            candidates.into_iter().next()
        }
    };
    let instruction = request
        .author_instruction
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let task_instruction = task_instruction(session);
    if let Some(instruction) = instruction.filter(|_| task_instruction.is_empty()) {
        if !author_said(session, instruction) {
            return Ok(tool_error(
                "author_instruction_unverified",
                "author_instruction 必须是作者在本任务里的原话摘录，不能改写或补充",
                json!({}),
            ));
        }
    }
    // 作者原话由宿主投递，不能因为模型漏填或只摘了一句而丢失其他要求。
    let instruction = if task_instruction.is_empty() {
        instruction
    } else {
        Some(task_instruction.as_str())
    };
    // 指纹只含稳定输入：画像、作者规则、方法与文风技能、本书要求。作者在任务里补一句话、答一张卡
    // 不让已做好的拍子和候选作废；作者要求另随任务记录，起草时按当时的全部原话交给写作。
    let mut fingerprint_parts = vec![
        profile,
        json!(rules
            .iter()
            .map(|r| json!([r["id"], r["text"]]))
            .collect::<Vec<_>>()),
        json!(skill.as_ref().map(|s| json!([s["id"], s["instructions"]]))),
    ];
    if !requirements.is_null() { fingerprint_parts.push(requirements.clone()); }
    if let Some(style) = &style { fingerprint_parts.push(style.clone()); }
    let profile = &fingerprint_parts[0];
    let fingerprint = crate::agent_creation::version(&json!(fingerprint_parts))
    .chars()
    .take(16)
    .collect::<String>();
    let mut document = json!({"rulesetId":id(),"bookId":book_id,"profileId":profile_id,
        "ruleIds":rules.iter().map(|r|r["id"].clone()).collect::<Vec<_>>(),"fingerprint":fingerprint,"styleSkillId":style_id});
    if let Some(skill) = &skill {
        document["skillId"] = skill["id"].clone();
    }
    if let Some(instruction) = instruction {
        document["authorInstruction"] = json!(instruction);
    }
    validate_document("ruleset", &document)?;
    let mut record = document.clone();
    record["id"] = json!(format!("ruleset:{}", str_field(&document, "rulesetId")?));
    record["sessionId"] = session["id"].clone();
    record["runId"] = session["runId"].clone();
    record["profileName"] = profile["name"].clone();
    record["bookRequirements"] = requirements;
    if let Some(style) = style { record["styleSkill"] = style; }
    record["profileSummary"] = json!(profile_summary(&profile));
    record["rules"] = json!(rules
        .iter()
        .map(|r| json!({"id":r["id"],"category":r["category"],"text":r["text"]}))
        .collect::<Vec<_>>());
    if let Some(skill) = &skill {
        record["skillName"] = skill["name"].clone();
        record["skillInstructions"] =
            json!(method_text(skill["instructions"].as_str().unwrap_or("")));
    }
    record["expanded"] = json!(expand(&record));
    record["createdAt"] = json!(now());
    if request.persist {
        put(db, canon::AUTHOR_NS, str_field(&record, "id")?, &record).await?;
    }
    Ok(record)
}

pub async fn load(db: &mut SqliteConnection, ruleset_id: &str) -> Result<Value, String> {
    get(db, canon::AUTHOR_NS, &format!("{RECORD_PREFIX}{}", canonical_id(ruleset_id)))
        .await?
        .ok_or_else(|| "规则集不存在，请先 resolve_ruleset".to_owned())
}

/// 审稿与修订共用成果已经绑定的规则集。旧候选没有绑定时按当前作者要求补齐，
/// 有明确编号时核对归属与有效性；不能无声切换方法，也不能以空规则继续修订。
pub async fn for_artifact(
    db: &mut SqliteConnection,
    session: &Value,
    artifact: &mut Value,
    requested: Option<&str>,
) -> Result<Value, String> {
    let requested = requested.filter(|id| !id.is_empty()).map(canonical_id);
    let bound = artifact["rulesetId"].as_str().filter(|id| !id.is_empty()).map(canonical_id);
    // 起草时被当场换成新版的旧编号仍指这同一套规则
    let replaced = artifact["rulesetRefreshedFrom"].as_str().map(canonical_id);
    if matches!((bound, requested), (Some(a), Some(b)) if a != b && replaced != Some(b)) {
        return Ok(tool_error(
            "ruleset_mismatch",
            "候选已绑定另一规则集，请按当前候选的规则检查；换写法时另开候选",
            json!({}),
        ));
    }
    let book_id = match artifact["bookId"].as_str().filter(|b| !b.is_empty()) {
        Some(id) => id.to_owned(),
        None if artifact["kind"] == "short" => canon::short_book(
            artifact["profileId"]
                .as_str()
                .unwrap_or(crate::agent_quality::SHORT_PROFILE),
        ),
        _ => return Err("候选未关联作品，无法确定写作规则".into()),
    };
    let record = match bound.or(requested) {
        Some(id) => verify(db, session, id, &book_id).await?,
        None => {
            resolve(
                db,
                session,
                &ResolveRequest {
                    style_skill_id: None,
                    book_id: &book_id,
                    skill_id: None,
                    author_instruction: None,
                    persist: true,
                },
            )
            .await?
        }
    };
    if record["toolError"].is_object() {
        return Ok(record);
    }
    if record["stale"] == true {
        return Ok(tool_error(
            "stale_ruleset",
            "写作规则已变化，请重新解析规则集并准备新的修改候选",
            json!({}),
        ));
    }
    artifact["rulesetId"] = record["rulesetId"].clone();
    Ok(record)
}

/// 校验工具传入的规则集属于当前作品，并标出它是否已过期（规则、画像、技能有变化）。
pub async fn verify(
    db: &mut SqliteConnection,
    session: &Value,
    ruleset_id: &str,
    book_id: &str,
) -> Result<Value, String> {
    let mut record = load(db, ruleset_id).await?;
    if record["bookId"] != book_id {
        return Err("规则集不属于当前作品".into());
    }
    let current = resolve(
        db,
        session,
        &ResolveRequest {
            style_skill_id: Some(record["styleSkillId"].as_str().unwrap_or("")),
            book_id,
            skill_id: record["skillId"].as_str(),
            author_instruction: record["authorInstruction"].as_str(),
            persist: false,
        },
    )
    .await?;
    let stale = current["toolError"].is_object() || current["fingerprint"] != record["fingerprint"];
    record["stale"] = json!(stale);
    Ok(record)
}

/// 起草用的规则集：作者规则、画像、技能或本书要求在规划后变了，就按原来选定的方法与文风
/// 当场重新解析并改用新版，不让模型为同一套拍子重走解析和保存。新版解析不出来
/// （技能已停用、方法冲突）时以 toolError 交回。返回值的 `refreshedFrom` 记录被替换的旧编号。
pub async fn current(
    db: &mut SqliteConnection,
    session: &Value,
    ruleset_id: &str,
    book_id: &str,
) -> Result<Value, String> {
    let record = verify(db, session, ruleset_id, book_id).await?;
    if record["stale"] != true {
        return Ok(record);
    }
    let mut fresh = resolve(
        db,
        session,
        &ResolveRequest {
            style_skill_id: Some(record["styleSkillId"].as_str().unwrap_or("")),
            book_id,
            skill_id: record["skillId"].as_str(),
            author_instruction: None,
            persist: true,
        },
    )
    .await?;
    if fresh["toolError"].is_null() {
        fresh["refreshedFrom"] = record["rulesetId"].clone();
    }
    Ok(fresh)
}
