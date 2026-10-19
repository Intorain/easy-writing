//! Immutable, source-bound writing inputs shared by drafting, editing and review.
//! This is candidate metadata, never an additional authoritative chapter store.
use crate::{agent_context, agent_creation, agent_quality, agent_store::*};
use serde_json::{json, Value};
use sqlx::SqliteConnection;

pub async fn capture(
    db: &mut SqliteConnection,
    artifact: &Value,
    context: &Value,
    rules: &Value,
) -> Result<Value, String> {
    let bid = artifact["bookId"].as_str().unwrap_or("");
    let materials = if bid.is_empty() {
        json!({})
    } else {
        agent_creation::materials(db, bid).await?
    };
    let plan = match artifact["planArtifactId"]
        .as_str()
        .filter(|s| !s.is_empty())
    {
        Some(id) => get(db, NS, &format!("artifact:{id}")).await?,
        None => None,
    };
    let mut style = context["layers"]["style"].as_array().cloned().unwrap_or_default();
    // Existing chapters and short drafts use the same selected style source as
    // new chapters. Long drafting already recorded the exact per-scene selection.
    if !context["writerInput"].is_object() {
        if let Some(profile) = crate::agent_style_profile::bound(db,rules).await? {
            style = crate::agent_style_profile::select_examples(&profile,"",plan.as_ref().and_then(|p|p["content"].as_str()).unwrap_or(""),
                crate::agent_style_profile::EXAMPLES_PER_SCENE).into_iter().map(|e| {
                    let mut source=e["source"].clone();source["exampleId"]=e["id"].clone();
                    json!({"label":"原文范例","text":e["text"],"source":source})
                }).collect();
        }
    }
    let lock = get(
        db,
        crate::agent_canon::AUTHOR_NS,
        &format!("style-lock:{bid}"),
    )
    .await?;
    // Only prose already available to a reader belongs in this projection.
    // No plans, world bible, character secrets, future threads or style examples.
    let reader_context = ["adjacent", "task"]
        .into_iter()
        .flat_map(|layer| context["layers"][layer].as_array().into_iter().flatten())
        .filter(|b| {
            matches!(
                b["source"]["type"].as_str(),
                Some("previousTail" | "candidate")
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    let briefs = materials["outlineNodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|n| n["agentBookBrief"] == true)
        .cloned()
        .collect::<Vec<_>>();
    // 作者要求取起草那一刻本任务的全部原话（候选上记着）；规则集里的只是解析当时的快照
    let author_task = if artifact["authorTask"].is_string() { &artifact["authorTask"] } else { &rules["authorInstruction"] };
    let mut writer_rules = rules.clone();
    if let Some(map) = writer_rules.as_object_mut() { map.remove("authorInstruction"); map.remove("writingInstructions"); }
    let writer_rules = crate::agent_ruleset::writer_text(&writer_rules);
    let mut basis = json!({"version":2,"writerInput":context["writerInput"],"baseWriterRulesText":writer_rules,"rulesetId":artifact["rulesetId"],"rulesFingerprint":rules["fingerprint"],
        "rulesText":rules["expanded"],"authorTask":author_task,"styleSkill":rules["styleSkill"],"styleBinding":rules["bookRequirements"]["styleSkillId"],
        "chapterPlan":plan.as_ref().map(|p|p["content"].clone()),
        "planFingerprint":plan.as_ref().map(|p|agent_creation::version(&json!([p["plan"],p["content"]]))),
        "materialsVersion":agent_creation::story_version(&materials),"styleLockVersion":agent_creation::version(&json!(lock)),
        "style":style,
        "readerContext":reader_context,"omitted":context["omitted"].as_array().cloned().unwrap_or_default(),
        "sources":context["sources"].as_array().cloned().unwrap_or_default(),
        "protectedContext":{"adoptedBookBriefs":briefs,"chapterPlan":plan.as_ref().map(|p|p["content"].clone()),
            "characters":materials["characters"],"worldSettings":materials["worldSettings"]}});
    basis["fingerprint"] = json!(agent_creation::version(&basis));
    Ok(basis)
}

fn basis_conflict(message: &str) -> Value {
    crate::agent_tool_boundary::failure("stale_writing_basis",crate::agent_tool_boundary::FailureKind::Conflict,
        message,true,"polish_draft",json!({}))
}

/// String-returning editor/storage APIs keep their existing interface. Model tools
/// use check_current so expected source changes retain a machine-readable cause.
pub async fn verify(db: &mut SqliteConnection, artifact: &Value) -> Result<(),String> {
    match check_current(db,artifact).await? {
        Some(fault)=>Err(fault["toolError"]["message"].as_str().unwrap_or("写作依据已变化").to_owned()),
        None=>Ok(()),
    }
}

pub async fn check_current(db: &mut SqliteConnection, artifact: &Value) -> Result<Option<Value>, String> {
    let basis = &artifact["writingBasis"];
    if !basis.is_object() {
        return Ok(None);
    } // Legacy candidates acquire a basis when reviewed.
    let bid = artifact["bookId"].as_str().unwrap_or("");
    if let Some(style_id) = basis["styleSkill"]["id"].as_str() {
        let current = crate::agent_ruleset::style_skill(db, style_id).await?;
        if current != basis["styleSkill"] {
            return Ok(Some(basis_conflict("采用的文风技能已变化或停用，请按当前文风重新准备候选")));
        }
    }
    if basis.get("styleBinding").is_some() && !bid.is_empty() {
        let requirements = get(db, crate::agent_canon::AUTHOR_NS, &format!("book-requirements:{bid}")).await?.unwrap_or(Value::Null);
        if requirements["styleSkillId"].as_str().unwrap_or("") != basis["styleBinding"].as_str().unwrap_or("") {
            return Ok(Some(basis_conflict("本书绑定的文风已变化，请重新准备候选")));
        }
    }
    let materials = if bid.is_empty() {
        json!({})
    } else {
        agent_creation::materials(db, bid).await?
    };
    let lock = get(
        db,
        crate::agent_canon::AUTHOR_NS,
        &format!("style-lock:{bid}"),
    )
    .await?;
    if basis["rulesetId"] != artifact["rulesetId"]
        || !agent_creation::same_story(&basis["materialsVersion"], &materials)
        || basis["styleLockVersion"] != agent_creation::version(&json!(lock))
    {
        return Ok(Some(basis_conflict("写作依据已变化，请重新准备修订候选，保留原稿")));
    }
    if let Some(pid) = artifact["planArtifactId"]
        .as_str()
        .filter(|s| !s.is_empty())
    {
        let Some(plan) = get(db, NS, &format!("artifact:{pid}")).await? else {
            return Ok(Some(basis_conflict("写作计划已不存在，请重新规划后准备候选")));
        };
        if basis["planFingerprint"]
            != agent_creation::version(&json!([plan["plan"], plan["content"]]))
        {
            return Ok(Some(basis_conflict("写作计划已变化，请重新准备候选的写作依据")));
        }
    }
    for source in basis["sources"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|s| s["type"] == "chapter")
    {
        let cid = str_field(source, "chapterId")?;
        let current = crate::agent_indexer::chapter_fingerprint(db, bid, cid).await?;
        if current.as_deref() != source["revision"].as_str() {
            return Ok(Some(basis_conflict("写作依据中的前文已变化，请重新准备候选")));
        }
    }
    for example in basis["style"].as_array().into_iter().flatten() {
        let source = &example["source"];
        if source["type"] == "reference" {
            let body = crate::agent_breakdown::chapter(db,str_field(source,"projectId")?,str_field(source,"chapterId")?).await?;
            if !crate::agent_style_profile::source_matches(body["textContent"].as_str().unwrap_or(""),example["text"].as_str().unwrap_or("")) {
                return Ok(Some(basis_conflict("采用的参考原文已变化，请重新准备写作依据")));
            }
        }
        if source["type"] == "sample" {
            let cid = str_field(source, "chapterId")?;
            let current = crate::agent_indexer::chapter_fingerprint(db, bid, cid).await?;
            let confirmed: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM chapter_chunks WHERE bookId=? AND chapterId=? AND seq=? AND provenance='author_confirmed'")
                .bind(bid).bind(cid).bind(source["seq"].as_i64().unwrap_or(-1)).fetch_one(&mut *db).await.map_err(err)?;
            if current.as_deref() != source["fingerprint"].as_str() || confirmed == 0 {
                return Ok(Some(basis_conflict("采用的表达示例已变化或被撤销，请重新准备写作依据")));
            }
        }
    }
    Ok(None)
}

pub async fn ensure(db: &mut SqliteConnection, artifact: &mut Value) -> Result<(), String> {
    if artifact["writingBasis"].is_object() {
        return verify(db, artifact).await;
    }
    let bid = artifact["bookId"].as_str().unwrap_or("");
    let rules = crate::agent_ruleset::load(db, str_field(artifact, "rulesetId")?).await?;
    let plan = match artifact["planArtifactId"]
        .as_str()
        .filter(|s| !s.is_empty())
    {
        Some(id) => get(db, NS, &format!("artifact:{id}"))
            .await?
            .unwrap_or(Value::Null),
        None => Value::Null,
    };
    let selection = artifact
        .get("styleSampleIds")
        .unwrap_or(&plan["plan"]["styleSampleIds"]);
    let ids = selection
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    let context = if bid.is_empty() {
        let style = crate::agent_style::select_examples(
            db,
            bid,
            plan["content"].as_str().unwrap_or(""),
            &ids,
        )
        .await?
        .iter()
        .map(crate::agent_style::example_block)
        .collect::<Vec<_>>();
        json!({"layers":{"style":style}})
    } else {
        agent_context::assemble_with_samples(
            db,
            None,
            &agent_context::ContextRequest {
                book_id: bid,
                purpose: "review",
                chapter_id: artifact["chapterId"].as_str(),
                ruleset_id: artifact["rulesetId"].as_str(),
                query_hint: plan["content"].as_str(),
                characters_hint: Vec::new(),
                model_max_context: artifact["writerModel"]["configuration"]["maxContext"]
                    .as_u64()
                    .unwrap_or(32000) as usize,
                output_reserve: artifact["writerModel"]["configuration"]["maxOutputTokens"]
                    .as_u64()
                    .unwrap_or(4096) as usize,
                budget_tokens: None,
            },
            &ids,
        )
        .await?
    };
    if context["ok"] == false {
        return Err("当前必要上下文超出预算，未建立不完整的写作依据".into());
    }
    artifact["writingBasis"] = capture(db, artifact, &context, &rules).await?;
    // Existing reports were made against different inputs, even if the prose is unchanged.
    for report in artifact["reports"].as_array_mut().into_iter().flatten() {
        report["stale"] = json!(true);
    }
    Ok(())
}

pub fn fingerprint(artifact: &Value) -> Option<&str> {
    artifact["writingBasis"]["fingerprint"].as_str()
}

/// Existing formal chapters have no generation snapshot. Review them against a
/// fresh, explicitly ephemeral basis using the same source projection as drafts.
pub async fn for_chapter(
    db: &mut SqliteConnection,
    book_id: &str,
    chapter_id: &str,
    ruleset_id: Option<&str>,
) -> Result<Value, String> {
    let context = agent_context::assemble(
        db,
        None,
        &agent_context::ContextRequest {
            book_id,
            purpose: "review",
            chapter_id: Some(chapter_id),
            ruleset_id,
            query_hint: None,
            characters_hint: Vec::new(),
            model_max_context: 32000,
            output_reserve: 4096,
            budget_tokens: None,
        },
    )
    .await?;
    if context["ok"] != true {
        return Err("章节评审所需上下文超出预算".into());
    }
    let rules = match ruleset_id {
        Some(id) => crate::agent_ruleset::load(db, id).await?,
        None => json!({"expanded":context["layers"]["fixed"].as_array().into_iter().flatten()
            .filter(|b|matches!(b["source"]["type"].as_str(),Some("rules"|"profile")))
            .filter_map(|b|b["text"].as_str()).collect::<Vec<_>>().join("\n")}),
    };
    capture(
        db,
        &json!({"bookId":book_id,"chapterId":chapter_id,"rulesetId":ruleset_id}),
        &context,
        &rules,
    )
    .await
}

pub fn reader_report_is_bound(artifact: &Value, report: &Value) -> bool {
    fingerprint(artifact).is_none_or(|f| report["writingBasisFingerprint"] == f)
        && report["reviewedFingerprint"]
            == agent_quality::fingerprint(artifact["content"].as_str().unwrap_or(""))
}
