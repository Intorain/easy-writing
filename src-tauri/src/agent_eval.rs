//! 效果评测：Agent 写的章节候选被采纳了多少、作者改了多少、体检一次通过的比例、定向修订了几轮、
//! 状态疑点被驳回的比例；按写作方法（任务规则集里的方法技能）与写作模型分组。
//! 同一会话同一章按不同方法各写一稿即为一次 A/B，作者写入正文的那稿记为胜出。
//! 全部由已有记录推算，不调模型，也不另存统计。
use crate::agent_store::{chapter, list, NS};
use serde_json::{json, Value};
use sqlx::SqliteConnection;
use std::collections::{BTreeMap, BTreeSet};

/// Explicit author choices are separate from adoption and model self-scores.
/// Bind every comparison to exact revisions so subsequent edits cannot inherit votes.
pub async fn record_reading_choice(db: &mut SqliteConnection, args: &Value) -> Result<Value, String> {
    use crate::agent_store::{get, put, now, str_field};
    let criterion = str_field(args, "criterion")?;
    if !matches!(criterion, "naturalness" | "continue_reading") { return Err("阅读评价维度无效".into()); }
    let refs = args["candidates"].as_array().filter(|v| v.len() >= 2 && v.len() <= 10).ok_or("需要两份以上的对照稿件")?;
    let winner = str_field(args, "winnerId")?;
    let mut seen = BTreeSet::new();
    let mut candidates = Vec::new();
    let mut scope = None;
    for reference in refs {
        let aid = str_field(reference, "id")?;
        if !seen.insert(aid.to_owned()) { return Err("对照稿件不能重复".into()); }
        let a = get(db, NS, &format!("artifact:{aid}")).await?.ok_or("对照稿件不存在")?;
        if a["deletedAt"].is_number() || a["partial"] == true || !crate::agent_quality::is_prose(&a)
            || a["revision"] != reference["revision"] { return Err("对照稿件已变化，请重新打开后评价".into()); }
        let owner = if a["kind"] == "short" { json!([str_field(&a,"sessionId")?,str_field(&a,"planArtifactId")?]) }
            else { json!([str_field(&a,"bookId")?,str_field(&a,"chapterId")?]) };
        if scope.as_ref().is_some_and(|s| s != &owner) { return Err("只能比较同一写作目标的稿件".into()); }
        scope = Some(owner);
        candidates.push(json!({"id":aid,"revision":a["revision"],"fingerprint":crate::agent_quality::fingerprint(a["content"].as_str().unwrap_or("")),
            "bookId":a["bookId"],"model":a["writerModel"],"method":a["variant"]}));
    }
    if !seen.contains(winner) { return Err("选中的稿件不在本次对照中".into()); }
    candidates.sort_by_key(|a| a["id"].as_str().unwrap_or("").to_owned());
    let id = format!("reading-choice:{}", crate::agent_creation::version(&json!([criterion,candidates])));
    let record = json!({"id":id,"criterion":criterion,"candidates":candidates,"winnerId":winner,"source":"author_choice","blinded":false,"createdAt":now()});
    put(db,NS,&id,&record).await?;
    Ok(record)
}

const DEFAULT_METHOD: &str = "默认方法";
const UNKNOWN_MODEL: &str = "未记录";

/// 字符差异比例：新增、删除、替换均计入，替换片段按两侧较长者计数，分母为两稿较长的字符数。
/// 这是稿件差异指标，不等同于作者耗时；限制最坏计算时间，大改稿时差异算法可保守合并变更片段。
pub fn edit_ratio(generated: &str, final_text: &str) -> f64 {
    let total = generated.chars().count().max(final_text.chars().count());
    if total == 0 {
        return 0.0;
    }
    let diff = similar::TextDiff::configure()
        .timeout(std::time::Duration::from_millis(100))
        .diff_chars(generated, final_text);
    let changed: usize = diff
        .ops()
        .iter()
        .map(|op| match op {
            similar::DiffOp::Equal { .. } => 0,
            similar::DiffOp::Delete { old_len, .. } => *old_len,
            similar::DiffOp::Insert { new_len, .. } => *new_len,
            similar::DiffOp::Replace {
                old_len, new_len, ..
            } => (*old_len).max(*new_len),
        })
        .sum();
    (changed as f64 / total as f64).min(1.0)
}

#[derive(Default)]
struct Group {
    candidates: usize,
    applied: usize,
    edit_sum: f64,
    edit_samples: usize,
    first_pass: usize,
    checked: usize,
    final_pass: usize,
    rounds: usize,
}

impl Group {
    fn to_json(&self, key: &str) -> Value {
        let ratio = |a: usize, b: usize| {
            if b == 0 {
                Value::Null
            } else {
                json!(((a as f64 / b as f64) * 1000.0).round() / 1000.0)
            }
        };
        json!({"key":key,"candidates":self.candidates,"applied":self.applied,"adoptionRate":ratio(self.applied,self.candidates),
            "editRatio":if self.edit_samples == 0 { Value::Null } else { json!(((self.edit_sum / self.edit_samples as f64) * 1000.0).round() / 1000.0) },
            "editSamples":self.edit_samples,"firstPassRate":ratio(self.first_pass,self.checked),"finalPassRate":ratio(self.final_pass,self.checked),
            "avgReviseRounds":if self.candidates == 0 { Value::Null } else { json!(((self.rounds as f64 / self.candidates as f64) * 100.0).round() / 100.0) }})
    }
}

fn blocked(report: &Value) -> bool {
    report["blocked"] == true
        || report["issues"]
            .as_array()
            .is_some_and(|i| i.iter().any(|x| x["level"] == "block"))
}

/// 一本书（或不传书时全部作品）的评测报告。
pub async fn report(db: &mut SqliteConnection, book_id: Option<&str>) -> Result<Value, String> {
    let mut artifacts = list(db, NS, "artifact:")
        .await?
        .into_iter()
        .filter(|a| a["kind"] == "chapter" && a["partial"] != true && !a["deletedAt"].is_number())
        .filter(|a| book_id.map_or(true, |b| a["bookId"] == b))
        .collect::<Vec<_>>();
    artifacts.sort_by_key(|a| a["createdAt"].as_i64().unwrap_or(0));
    let (mut total, mut by_method, mut by_model) = (
        Group::default(),
        BTreeMap::<String, Group>::new(),
        BTreeMap::<String, Group>::new(),
    );
    let mut trials: BTreeMap<(String, String), Vec<Value>> = BTreeMap::new();
    for a in &artifacts {
        let method = a["variant"]
            .as_str()
            .filter(|v| !v.is_empty())
            .unwrap_or(DEFAULT_METHOD)
            .to_owned();
        let model = a["writerModel"]["name"]
            .as_str()
            .filter(|v| !v.is_empty())
            .unwrap_or(UNKNOWN_MODEL)
            .to_owned();
        let applied = a["applied"] == true;
        let reports = a["reports"].as_array().cloned().unwrap_or_default();
        // 终稿：写入正文后以作者在编辑器里改过的正式正文为准，否则是面板里的成果正文
        let edit = match a["generatedContent"]
            .as_str()
            .filter(|t| !t.trim().is_empty())
        {
            Some(generated) => {
                let final_text = if applied {
                    match (a["bookId"].as_str(), a["chapterId"].as_str()) {
                        (Some(bid), Some(cid)) => chapter(db, bid, cid)
                            .await
                            .ok()
                            .and_then(|c| c["textContent"].as_str().map(str::to_owned)),
                        _ => None,
                    }
                } else {
                    a["content"].as_str().map(str::to_owned)
                };
                final_text.map(|t| edit_ratio(generated, &t))
            }
            None => None,
        };
        let rounds = a["reviseRounds"]
            .as_u64()
            .map(|n| n as usize)
            .unwrap_or_else(|| reports.len().saturating_sub(1));
        for group in [
            &mut total,
            by_method.entry(method.clone()).or_default(),
            by_model.entry(model.clone()).or_default(),
        ] {
            group.candidates += 1;
            group.applied += applied as usize;
            if let Some(e) = edit {
                group.edit_sum += e;
                group.edit_samples += 1;
            }
            if let (Some(first), Some(last)) = (reports.first(), reports.last()) {
                group.checked += 1;
                group.first_pass += !blocked(first) as usize;
                group.final_pass += !blocked(last) as usize;
            }
            group.rounds += rounds;
        }
        if let (Some(sid), Some(cid)) = (a["sessionId"].as_str(), a["chapterId"].as_str()) {
            trials.entry((sid.to_owned(), cid.to_owned())).or_default().push(json!({"artifactId":a["id"],"method":method,"model":model,
                "applied":applied,"title":a["title"],"editRatio":edit,"blocked":reports.last().map(blocked)}));
        }
    }
    // A/B：同一会话同一章、至少两种方法各有一稿
    let mut ab = Vec::new();
    let mut ab_by_method: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for ((sid, cid), variants) in trials {
        let methods: BTreeSet<&str> = variants
            .iter()
            .filter_map(|v| v["method"].as_str())
            .collect();
        if methods.len() < 2 {
            continue;
        }
        let winner = variants
            .iter()
            .find(|v| v["applied"] == true)
            .and_then(|v| v["method"].as_str())
            .map(str::to_owned);
        for m in &methods {
            let entry = ab_by_method.entry((*m).to_owned()).or_default();
            entry.0 += 1;
            entry.1 += (winner.as_deref() == Some(*m)) as usize;
        }
        ab.push(json!({"sessionId":sid,"chapterId":cid,"title":variants[0]["title"],"variants":variants,"winner":winner}));
    }
    // 疑点裁决：驳回多说明疑点规则太敏感
    let verdicts = list(db, crate::agent_canon::AUTHOR_NS, "verdict:")
        .await?
        .into_iter()
        .filter(|v| book_id.map_or(true, |b| v["bookId"] == b))
        .collect::<Vec<_>>();
    let dismissed = verdicts
        .iter()
        .filter(|v| v["decision"] == "dismissed")
        .count();
    let choices = list(db, NS, "reading-choice:").await?.into_iter()
        .filter(|c| book_id.map_or(true, |bid| c["candidates"].as_array().into_iter().flatten().any(|a| a["bookId"] == bid))).collect::<Vec<_>>();
    Ok(json!({"bookId":book_id,"totals":total.to_json("全部"),
        "readingChoices":choices,"evaluationNote":"采用率、模型评分与人工阅读选择分别记录；改动比例不代表人工耗时，未收集的人工数据不推算。",
        "byMethod":by_method.iter().map(|(k, g)| g.to_json(k)).collect::<Vec<_>>(),
        "byModel":by_model.iter().map(|(k, g)| g.to_json(k)).collect::<Vec<_>>(),
        "suspects":{"accepted":verdicts.len() - dismissed,"dismissed":dismissed,
            "dismissalRate":if verdicts.is_empty() { Value::Null } else { json!(((dismissed as f64 / verdicts.len() as f64) * 1000.0).round() / 1000.0) }},
        "abTrials":ab,
        "abByMethod":ab_by_method.iter().map(|(m, (trials, wins))| json!({"method":m,"trials":trials,"wins":wins})).collect::<Vec<_>>()}))
}
