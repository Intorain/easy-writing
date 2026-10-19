//! Candidate delivery state and revision receipts. Formal chapter storage is untouched.
use crate::{
    agent_canon as canon, agent_creation as creation, agent_quality as quality,
    agent_ruleset as ruleset, agent_store::*,
};
use serde_json::{json, Value};
use sqlx::SqliteConnection;

pub async fn verify_sources(db: &mut SqliteConnection, artifact: &Value) -> Result<(), String> {
    crate::agent_writing_basis::verify(db, artifact).await?;
    crate::agent_batch::verify_candidate_sources(db, artifact).await?;
    if let Some(bid) = artifact["bookId"].as_str().filter(|s| !s.is_empty()) {
        if artifact["sourceContext"].is_object() {
            crate::agent_memory::verify_proof(db, &artifact["sourceContext"], bid).await?;
        }
    }
    Ok(())
}

/// Session lifecycle and prose delivery are distinct. The host derives this
/// outcome from current artifacts, never from an agent's final sentence or done event.
pub async fn task_outcome(db:&mut SqliteConnection,s:&Value)->Result<Value,String> {
    let mut latest=std::collections::BTreeMap::<String,Value>::new();
    // 按任务统计：答卡、"继续"之后，本任务前几轮写出的稿件仍算这件事的成果
    for a in list(db,NS,"artifact:").await? {
        if a["sessionId"]!=s["id"] || !quality::is_prose(&a)
            || (!crate::agent_task::contains_run(s,&a["runId"]) && !crate::agent_task::contains_run(s,&a["delivery"]["runId"]))
            || a["deletedAt"].is_number() {continue;}
        let key=json!([a["bookId"],a["chapterId"],a["variant"]]).to_string();
        if latest.get(&key).is_none_or(|old|old["createdAt"].to_string()<=a["createdAt"].to_string()) {latest.insert(key,a);}
    }
    let mut items=Vec::new();
    for a in latest.values() {
        // A successful transactional submission is a completed historical action.
        // Its own write necessarily changes the chapter version used to draft it.
        let submitted=a["applied"] == true && a["appliedVersion"].is_number();
        let ready=submitted || (matches!(a["delivery"]["status"].as_str(),Some("ready"|"awaiting_author"))
            && !a["pendingRevision"].is_object() && completion_report(a,&a["delivery"]).is_ok()
            && verify_sources(db,a).await.is_ok());
        items.push(json!({"artifactId":a["id"],"revision":a["revision"],"applied":a["applied"]==true,
            "ready":ready,"deliveryStatus":a["delivery"]["status"]}));
    }
    let remaining=items.iter().filter(|i|i["ready"]!=true).count();
    let waiting=items.iter().any(|i|i["applied"]!=true && i["deliveryStatus"]=="awaiting_author");
    let state=if items.is_empty(){"no_prose"}else if remaining>0{"incomplete"}else if waiting{"awaiting_author"}else{"delivered"};
    let message=match state {
        "incomplete"=>format!("这件事的稿件已保留，仍有 {remaining} 份未完成生成、修订或当前版本复评。"),
        "awaiting_author"=>"可处理的修订已完成，仍有创作事项待你决定。".to_owned(),
        _=>String::new(),
    };
    Ok(json!({"state":state,"message":message,"artifacts":items}))
}

fn completion_report<'a>(artifact: &'a Value, status: &Value) -> Result<&'a Value, String> {
    let report = artifact["reports"]
        .as_array()
        .and_then(|r| r.last())
        .ok_or("当前稿件没有评审报告")?;
    if report["critic"]["status"] != "available"
        || !crate::agent_submission::report_current(artifact,report)
        || report["reviewedFingerprint"]
            != quality::fingerprint(artifact["content"].as_str().unwrap_or(""))
        || report["blocked"] == true
    {
        return Err("当前版本尚未通过交付复评".into());
    }
    if artifact["pendingRevision"].is_object() {
        let assessment = &report["critic"]["revisionAssessment"];
        if assessment["progress"] != "improved" || assessment["factsPreserved"] != true || assessment["withinScope"] != true {
            return Err("修订尚未核实改善、事实保留和修改范围，不能标记完成".into());
        }
    }
    // 编辑没改成或没改的意见留在报告里作内部记录；交付只看程序核实的阻断与当前版本的核对是否完成。
    if status["status"] == "ready"
        && report["issues"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|i| i["repairMode"] == "author_decision")
    {
        return Err("仍有待作者决定的事项，不能标记全部交付完成".into());
    }
    Ok(report)
}

async fn cached(
    db: &mut SqliteConnection,
    s: &Value,
    a: &Value,
    operation: &str,
) -> Result<Option<Value>, String> {
    if a["delivery"]["operationId"] != operation
        || a["delivery"]["revision"] != a["revision"]
        || a["delivery"]["fingerprint"] != quality::fingerprint(a["content"].as_str().unwrap_or(""))
        || !matches!(
            a["delivery"]["status"].as_str(),
            Some("ready" | "awaiting_author")
        )
    {
        return Ok(None);
    }
    let mut checked = a.clone();
    let rule = ruleset::for_artifact(db, s, &mut checked, None).await?;
    if rule["toolError"].is_object() {
        return Ok(None);
    }
    if verify_sources(db, a).await.is_err() { return Ok(None); }
    let Ok(report) = completion_report(a, &a["delivery"]) else {
        return Ok(None);
    };
    let mut out = json!({"artifact_id":a["id"],"revision":a["revision"],"cached":true,"delivery":a["delivery"],
        "reviewComplete":report["critic"]["status"] == "available","critic":report["critic"],"issues":report["issues"],"summary":quality::summary(&report)});
    save_target(&mut out, a);
    Ok(Some(out))
}

/// 交付后自动保存需要的目标：稿子挂在哪一章、基于哪个正文版本、是否已经写入。短篇与试写没有章节，不会被保存。
fn save_target(out: &mut Value, a: &Value) {
    out["book_id"] = a["bookId"].clone();
    out["chapter_id"] = if a["kind"] == "chapter" { a["chapterId"].clone() } else { Value::Null };
    out["expected_version"] = a["expectedVersion"].clone();
    out["applied"] = json!(a["applied"] == true);
    out["appliedVersion"] = a["appliedVersion"].clone();
}

pub async fn protected_context(db: &mut SqliteConnection, a: &Value) -> Result<Value, String> {
    if a["writingBasis"].is_object() {
        crate::agent_writing_basis::verify(db, a).await?;
        return Ok(a["writingBasis"]["protectedContext"].clone());
    }
    let plan = match a["planArtifactId"].as_str().filter(|s| !s.is_empty()) {
        Some(id) => get(db, NS, &format!("artifact:{id}"))
            .await?
            .map(|p| p["content"].clone())
            .unwrap_or(Value::Null),
        None => Value::Null,
    };
    let materials = if let Some(bid) = a["bookId"].as_str().filter(|s| !s.is_empty()) {
        creation::materials(db, bid).await?
    } else {
        json!({})
    };
    let briefs = materials["outlineNodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|n| n["agentBookBrief"] == true)
        .map(|n| json!({"title":n["title"],"content":n["content"]}))
        .collect::<Vec<_>>();
    Ok(json!({"adoptedBookBriefs":briefs,"chapterPlan":plan,
        "characters":materials["characters"],"worldSettings":materials["worldSettings"]}))
}

pub async fn begin(db: &mut SqliteConnection, s: &Value, args: &Value) -> Result<Value, String> {
    let aid = str_field(args, "artifact_id")?;
    let mut a = crate::agent_artifacts::read(db, s, aid).await?;
    if !quality::is_prose(&a) {
        return Err("只能处理正文候选".into());
    }
    let foreign = a["sessionId"] != s["id"];
    let operation = str_field(args, "operation_id")?;
    let own_replay = a["delivery"]["operationId"] == operation
        && a["generatedRevision"] == a["revision"]
        && a["generatedContent"] == a["content"];
    if a["revision"] != args["expected_revision"] && !own_replay {
        return Err("候选版本已变化，请重新读取后继续".into());
    }
    if a["applied"] == true || a["partial"] == true {
        return Err("已采用或尚未生成完成的稿件不能直接修订".into());
    }
    let automatic = args["automatic"] == true;
    // Keep the exact provisional text and its before-version across interruptions;
    // the next check verifies it instead of paying to generate it again.
    if automatic && foreign {
        return Err("跨会话稿件须以新的修订任务建立副本".into());
    }
    if automatic && a["generatedContent"] != a["content"] {
        return Err("作者已编辑候选，本次不自动覆盖作者改动".into());
    }
    if let Some(done) = cached(db, s, &a, operation).await? {
        return Ok(done);
    }
    if automatic {
        verify_sources(db, &a).await?;
    }
    if !automatic {
        // An explicit new editing task keeps the source draft intact when its
        // author instruction/profile has changed; bind a new candidate to the
        // current rules rather than secretly changing an old draft's method.
        let bid = a["bookId"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| {
                canon::short_book(a["profileId"].as_str().unwrap_or(quality::SHORT_PROFILE))
            });
        let selected = args["ruleset_id"].as_str().filter(|s| !s.is_empty());
        let old = match a["rulesetId"].as_str() {
            Some(r) => ruleset::load(db, r).await.ok(),
            None => None,
        };
        let rule = match selected {
            Some(id) => ruleset::verify(db, s, id, &bid).await?,
            None => {
                ruleset::resolve(
                    db,
                    s,
                    &ruleset::ResolveRequest {
                        style_skill_id: None,
                        book_id: &bid,
                        skill_id: old.as_ref().and_then(|r| r["skillId"].as_str()),
                        author_instruction: None,
                        persist: true,
                    },
                )
                .await?
            }
        };
        if rule["toolError"].is_object() || rule["stale"] == true {
            return Err("当前写作规则未就绪，请先解决规则冲突".into());
        }
        if foreign || a["rulesetId"] != rule["rulesetId"] || crate::agent_writing_basis::verify(db, &a).await.is_err() {
            let next_id = format!("{}:{operation}:revision", str_field(s, "id")?);
            if let Some(existing) = get(db, NS, &format!("artifact:{next_id}")).await? {
                if existing["applied"] == true
                    || existing["generatedRevision"] != existing["revision"]
                    || existing["generatedContent"] != existing["content"]
                {
                    return Err("交付候选已被作者修改或采用，本次不覆盖".into());
                }
                a = existing;
            } else {
                a["writingBasis"] = Value::Null;
                a["authorTask"] = json!(crate::agent_task::instruction(s));
                // A new task chooses against current samples; do not inherit stale example IDs.
                a["styleSampleIds"] = json!([]);
                a["sourceArtifactId"] = json!(aid);
                a["sessionId"] = s["id"].clone();
                a["id"] = json!(next_id);
                a["rulesetId"] = rule["rulesetId"].clone();
                a["profileId"] = rule["profileId"].clone();
                a["revision"] = json!(0);
                a["reports"] = json!([]);
                a["delivery"] = Value::Null;
                a["generatedContent"] = a["content"].clone();
                a["generatedRevision"] = json!(0);
                a["runId"] = s["runId"].clone();
                a["createdAt"] = json!(now());
                if !a["bookId"].as_str().unwrap_or("").is_empty() {
                    let context = crate::agent_memory::context(db, s, a["bookId"].as_str()).await?;
                    let mut context = context;
                    crate::agent_memory::capture_context(
                        db,
                        s,
                        str_field(s, "runId")?,
                        &mut context,
                    )
                    .await?;
                    a["sourceContext"] = get(
                        db,
                        OP_NS,
                        &format!("context:{}:{}", str_field(s, "id")?, str_field(s, "runId")?),
                    )
                    .await?
                    .unwrap_or(Value::Null);
                }
            }
        }
    }
    if let Some(done) = cached(db, s, &a, operation).await? {
        return Ok(done);
    }
    crate::agent_writing_basis::ensure(db, &mut a).await?;
    let resume_budget = if a["delivery"]["operationId"] == operation {a["delivery"]["budget"].clone()} else {Value::Null};
    a["delivery"] = json!({"status":"reviewing","revision":a["revision"],"rounds":0,"reason":"正在准备交付修订。",
        "remainingIssueIds":[],"history":[],"operationId":operation,"runId":s["runId"],"fingerprint":quality::fingerprint(a["content"].as_str().unwrap_or(""))});
    if resume_budget.is_object() {a["delivery"]["budget"] = resume_budget.clone();}
    put(db, NS, &format!("artifact:{}", str_field(&a, "id")?), &a).await?;
    let mut out = json!({"artifact_id":a["id"],"revision":a["revision"],"delivery":a["delivery"],"resumeBudget":resume_budget});
    save_target(&mut out, &a);
    Ok(out)
}

/// A revision is provisional until its own report verifies improvement. Failed
/// attempts stay in receipts; a task interruption never replaces the usable draft.
async fn settle_revision(db:&mut SqliteConnection,a:&mut Value,accept:bool,reason:&str)->Result<(),String> {
    let pending = a["pendingRevision"].clone();
    if !pending.is_object() {return Ok(());}
    if pending["revision"] != a["revision"] || pending["fingerprint"] != quality::fingerprint(a["content"].as_str().unwrap_or("")) {
        return Err("待验修订已被编辑，未覆盖当前稿件".into());
    }
    if !accept {
        let key = format!("rejected-revision:{}:{}",str_field(a,"id")?,a["revision"]);
        put(db,OP_NS,&key,&json!({"content":a["content"],"reports":a["reports"],"pendingRevision":pending,"reason":reason})).await?;
        let revision = a["revision"].as_u64().unwrap_or(0)+1;
        a["content"] = pending["before"]["content"].clone();
        a["generatedContent"] = a["content"].clone();
        a["revision"] = json!(revision);
        a["generatedRevision"] = json!(revision);
        a["textFingerprint"] = json!(quality::fingerprint(a["content"].as_str().unwrap_or("")));
        a["reports"] = pending["before"]["reports"].clone();
        if !a["reports"].is_array() {a["reports"]=json!([]);}
        let fingerprint=a["textFingerprint"].clone();
        if let Some(report)=a["reports"].as_array_mut().and_then(|r|r.last_mut()) {
            if report["reviewedFingerprint"] == fingerprint {
                report["reviewedRevision"]=json!(revision);
                report["stale"]=json!(false);
            }
        }
        a["readerReadout"]=Value::Null;
        a["rejectedRevisionReceipt"]=json!(key);
    }
    a["pendingRevision"]=Value::Null;
    Ok(())
}

/// An edit is accepted only after a version-bound comparison with its source.
pub fn accept_reviewed_revision(a:&mut Value)->Result<(),String> {
    if !a["pendingRevision"].is_object() {return Ok(());}
    let report=a["reports"].as_array().and_then(|r|r.last()).ok_or("待验修订缺少报告")?;
    if !crate::agent_submission::report_current(a,report) || report["blocked"] == true
        || report["critic"]["status"] != "available"
        || report["critic"]["revisionAssessment"]["progress"] != "improved"
        || report["critic"]["revisionAssessment"]["factsPreserved"] != true
        || report["critic"]["revisionAssessment"]["withinScope"] != true {
        return Err("上一轮修订后的正文核对未通过或缺少编辑结论，请先 check_chapter 或恢复原稿".into());
    }
    a["pendingRevision"]=Value::Null;
    Ok(())
}

/// Missing issues cannot silently become resolved during a verification pass.
pub fn reconcile_revision_review(artifact: &Value, review: &mut Value) -> Result<(), String> {
    let pending = &artifact["pendingRevision"];
    if !pending.is_object() { return Ok(()); }
    let assessment = &review["revisionAssessment"];
    if !assessment.is_object() { return Err("修订核验缺少 revisionAssessment".into()); }
    let previous = pending["before"]["reports"].as_array().and_then(|r|r.last());
    let previous_issues = previous.and_then(|r|r["issues"].as_array()).cloned().unwrap_or_default();
    let resolved: Vec<String> = assessment["resolvedIssueIds"].as_array().ok_or("缺少已核实的问题编号")?
        .iter().filter_map(|v|v.as_str().map(str::to_owned)).collect();
    if resolved.iter().any(|id| !previous_issues.iter().any(|i|i["id"] == id.as_str())) {
        return Err("修订核验引用了不存在的旧问题".into());
    }
    let issues = review["issues"].as_array_mut().ok_or("核验问题列表无效")?;
    for old in previous_issues.iter().filter(|i|i["source"] == "critic") {
        if resolved.iter().any(|id|old["id"] == id.as_str()) { continue; }
        if !issues.iter().any(|i|i["code"] == old["code"] && i["quotes"] == old["quotes"]) {
            let mut issue = old.clone();
            if let Some(map) = issue.as_object_mut() { map.remove("evidenceStatus"); }
            issues.push(issue);
        }
    }
    Ok(())
}

pub async fn progress(db: &mut SqliteConnection, s: &Value, args: &Value) -> Result<Value, String> {
    crate::agent_contract::validate_document("deliveryStatus", &args["delivery"])?;
    let aid = str_field(args, "artifact_id")?;
    let key = format!("artifact:{aid}");
    let mut a = get(db, NS, &key).await?.ok_or("候选不存在")?;
    if a["sessionId"] != s["id"]
        || a["revision"] != args["expected_revision"]
        || args["delivery"]["revision"] != a["revision"]
    {
        return Err("候选已经变化，本次处理状态未覆盖新版本".into());
    }
    let mut status = args["delivery"].clone();
    status["operationId"] = a["delivery"]["operationId"].clone();
    status["runId"] = s["runId"].clone();
    status["fingerprint"] = json!(quality::fingerprint(a["content"].as_str().unwrap_or("")));
    if matches!(status["status"].as_str(), Some("ready" | "awaiting_author")) {
        verify_sources(db, &a).await?;
        let rule = ruleset::for_artifact(db, s, &mut a, None).await?;
        if rule["toolError"].is_object() {
            return Err("写作规则已变化，不能将旧规则下的稿件标记为交付完成".into());
        }
        completion_report(&a, &status)?;
    }
    let terminal = !matches!(status["status"].as_str(),Some("reviewing"|"revising"));
    if terminal {
        let accepted = matches!(status["status"].as_str(),Some("ready"|"awaiting_author"));
        let reason=status["reason"].as_str().unwrap_or("").to_owned();
        if accepted { settle_revision(db,&mut a,true,&reason).await?; }
        status["revision"]=a["revision"].clone();
        status["fingerprint"]=a["textFingerprint"].clone();
    }
    a["delivery"] = status;
    put(db, NS, &key, &a).await?;
    let report=a["reports"].as_array().and_then(|r|r.last());
    let reviewed=report.is_some_and(|r|r["critic"]["status"]=="available" && crate::agent_submission::report_current(&a,r))
        && verify_sources(db,&a).await.is_ok();
    Ok(json!({"artifact_id":aid,"revision":a["revision"],"delivery":a["delivery"],"report":report,"reviewComplete":reviewed}))
}

pub async fn restore(db: &mut SqliteConnection, s: &Value, args: &Value) -> Result<Value, String> {
    let aid = str_field(args, "artifact_id")?;
    let key = format!("artifact:{aid}");
    let mut a = get(db, NS, &key).await?.ok_or("候选不存在")?;
    let op = format!(
        "{}:{}",
        str_field(s, "id")?,
        str_field(args, "operation_id")?
    );
    let receipt = get(db, OP_NS, &op).await?.ok_or("修订回执不存在")?;
    if a["sessionId"] != s["id"]
        || a["revision"] != args["expected_revision"]
        || a["applied"] == true
        || receipt["result"]["artifact_id"] != aid
        || receipt["result"]["revision"] != a["revision"]
        || receipt["afterFingerprint"] != quality::fingerprint(a["content"].as_str().unwrap_or(""))
        || receipt["before"]["rulesetId"] != a["rulesetId"]
    {
        return Err("稿件已变化，不能恢复旧修订前版本".into());
    }
    if a["pendingRevision"].is_object() {
        settle_revision(db,&mut a,false,"复评未确认改善，恢复上一份有效稿件。").await?;
        put(db,NS,&key,&a).await?;
        return Ok(json!({"artifact_id":aid,"revision":a["revision"],"report":a["reports"].as_array().and_then(|r|r.last())}));
    }
    let before = receipt["before"]["content"]
        .as_str()
        .ok_or("该修订没有可恢复的原稿")?;
    let revision = a["revision"].as_i64().unwrap_or(0) + 1;
    let report =
        quality::check_artifact(db, &a, before, aid, revision, a["rulesetId"].as_str()).await?;
    a["content"] = json!(before);
    a["generatedContent"] = json!(before);
    a["generatedRevision"] = json!(revision);
    a["revision"] = json!(revision);
    a["textFingerprint"] = report["reviewedFingerprint"].clone();
    for old in a["reports"].as_array_mut().into_iter().flatten() {
        old["stale"] = json!(true);
    }
    a["reports"]
        .as_array_mut()
        .ok_or("报告列表无效")?
        .push(report.clone());
    put(db, NS, &key, &a).await?;
    Ok(json!({"artifact_id":aid,"revision":revision,"report":report}))
}
