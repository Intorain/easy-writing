//! 确定性核对：不调模型，不判文风。
//!
//! 只做三类事：
//! 1. 结构与数据完整性：空稿、字数不到下限一半、整段重复、作者自定义敏感词是阻断（保存前要作者豁免）；
//!    字数偏低偏高、相邻段开头相同只提示。
//! 2. 状态层疑点：来自状态账本，附两处原文，由作者裁决。
//! 3. 与参考书原文逐字重合：连续 12 字以上相同即报位置，保存前出确认卡，防"换个名字照抄"。
//! 文风一律不在这里判：没有 AI 味词表，没有对白占比、句长之类的关卡。指纹只量出来给人看（styleDrift），
//! 不进提示词，不拦保存，也不交给评审当线索——数字和词表给模型，只会让它换一套话来满足数字。
//! 各 code 的级别由平台画像的 gate 决定；画像没写的 code 用这里的默认级别。
use crate::agent_canon as canon;
use crate::agent_store::{get, str_field};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::SqliteConnection;
use std::collections::{BTreeMap, BTreeSet, HashMap};

const REPEAT_MIN_PARAGRAPH_CHARS: usize = 16;
/// 与参考书连续多少字相同算逐字重合（不计空白）。
pub const OVERLAP_MIN_CHARS: usize = 12;
/// 单次检查最多报这么多处重合；来源原文超过这个字数只查前面的部分。
const OVERLAP_MAX_HITS: usize = 20;
const OVERLAP_MAX_SOURCE_CHARS: usize = 2_000_000;

fn default_level(code: &str) -> &'static str {
    match code {
        "structure.empty" | "structure.words_min" | "structure.repeat" | "structure.sensitive" | "overlap.reference" => "block",
        c if c.starts_with("state.") => "suspect",
        _ => "notice",
    }
}

fn level_for(profile: &Value, code: &str) -> String {
    for level in ["block", "suspect", "notice"] {
        if profile["gate"][level]
            .as_array()
            .is_some_and(|codes| codes.iter().any(|c| c == code))
        {
            return level.to_owned();
        }
    }
    default_level(code).to_owned()
}

fn paragraphs(text: &str) -> Vec<&str> {
    text.split('\n').map(str::trim).filter(|p| !p.is_empty()).collect()
}

fn std_dev(values: &[f64]) -> f64 {
    if values.len() < 2 {
        return 0.0;
    }
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    (values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / values.len() as f64).sqrt()
}

pub fn fingerprint(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
        .chars()
        .take(16)
        .collect()
}

/// 纯文本指标：结构计数 + 文风指纹。不依赖数据库，不判好坏。
pub fn metrics(text: &str) -> Value {
    let paras = paragraphs(text);
    let words = text.chars().filter(|c| !c.is_whitespace() && *c != '\u{feff}').count();
    let lengths: Vec<f64> = paras.iter().map(|p| p.chars().count() as f64).collect();
    let avg = if lengths.is_empty() { 0.0 } else { lengths.iter().sum::<f64>() / lengths.len() as f64 };
    let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
    let mut repeated = 0;
    for p in paras.iter().copied() {
        if p.chars().count() < REPEAT_MIN_PARAGRAPH_CHARS {
            continue;
        }
        let count = seen.entry(p).or_insert(0);
        *count += 1;
        if *count == 2 {
            repeated += 1;
        }
    }
    let fp = crate::agent_style::style_fingerprint(text);
    let sentence_std = fp["sentenceCv"].as_f64().unwrap_or(0.0) * fp["sentenceMean"].as_f64().unwrap_or(0.0);
    json!({"words":words,"paragraphs":paras.len(),"repeatedParagraphs":repeated,"estimatedTokens":crate::agent_context::estimate_tokens(text),
        "avgParagraphLen":(avg*10.0).round()/10.0,"paragraphLenStd":(std_dev(&lengths)*10.0).round()/10.0,
        "dialogueRatio":fp["dialogueRatio"],"sentenceLenStd":(sentence_std*10.0).round()/10.0,"fingerprint":fp})
}

fn issue(profile: &Value, code: &str, dimension: &str, message: String, paragraphs: Vec<usize>, quotes: Vec<String>, fix: Option<&str>) -> Value {
    let mut value = json!({"id":format!("{code}:{}",paragraphs.first().copied().unwrap_or(0)),"source":"rule","code":code,"dimension":dimension,
        "level":level_for(profile,code),"message":message,"paragraphs":paragraphs,"quotes":quotes.into_iter().map(|q| q.chars().take(200).collect::<String>()).collect::<Vec<_>>()});
    if let Some(fix) = fix {
        value["fix"] = json!(fix);
    }
    if code.starts_with("style.") {
        value["observation"] = json!(true);
    }
    value
}

/// 结构检查。`sensitive` 是作者自定义敏感词（作者显式维护的字段，不是平台词表）。
/// 字数按程序实测：有作者要求或起草目标时以它为准（“约 N 字”，轻微偏差不报），没有时才用画像区间；
/// 只有不到目标一半才算阻断（多半是生成中断或漏写），其余只是提示，不触发改稿。
pub fn rule_issues(text: &str, profile: &Value, sensitive: &[String], target_words: Option<usize>) -> (Value, Vec<Value>) {
    let m = metrics(text);
    let paras = paragraphs(text);
    let mut issues = Vec::new();
    let words = m["words"].as_u64().unwrap_or(0) as usize;
    if words == 0 {
        issues.push(issue(profile, "structure.empty", "结构", "正文为空".into(), vec![], vec![], None));
        return (m, issues);
    }
    let min = profile["chapterWords"][0].as_u64().unwrap_or(2000) as usize;
    let max = profile["chapterWords"][1].as_u64().unwrap_or(4000) as usize;
    let (low, high, basis) = match target_words.filter(|t| *t > 0) {
        Some(target) => (target * 8 / 10, target * 15 / 10, format!("目标约 {target} 字")),
        None => (min, max * 17 / 10, format!("画像区间 {min}–{max} 字")),
    };
    // 阻断线取画像下限与目标里较小的那个的一半：作者要的短章不被画像卡住，默认目标也不比画像更严
    let floor = target_words.filter(|t| *t > 0).map_or(min, |t| t.min(min)) / 2;
    if words < floor {
        issues.push(issue(profile, "structure.words_min", "结构", format!("实测 {words} 字，不到可接受篇幅（{basis}）的一半"), vec![], vec![], Some("核对是否生成中断或漏写了必要情节；不用描写凑字")));
    } else if words < low {
        issues.push(issue(profile, "structure.words_low", "结构", format!("实测 {words} 字，低于{basis}"), vec![], vec![], None));
    } else if words > high {
        issues.push(issue(profile, "structure.words_high", "结构", format!("实测 {words} 字，明显超过{basis}"), vec![], vec![], Some("核对是否有重复冗长的段落；篇幅过长可拆成两章")));
    }
    // 整段重复
    let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
    let mut repeated: Vec<(usize, &str)> = Vec::new();
    for (i, p) in paras.iter().copied().enumerate() {
        if p.chars().count() < REPEAT_MIN_PARAGRAPH_CHARS {
            continue;
        }
        if let Some(first) = seen.get(p) {
            repeated.push((*first, p));
        } else {
            seen.insert(p, i);
        }
    }
    if !repeated.is_empty() {
        issues.push(issue(profile, "structure.repeat", "重复", format!("发现 {} 处整段重复", repeated.len()),
            repeated.iter().map(|(i, _)| *i).collect(), repeated.iter().take(3).map(|(_, p)| (*p).to_owned()).collect(), Some("删去重复段或重写")));
    }
    // 相邻段开头相同：程序能看见的重复，只提示
    let mut same_head = Vec::new();
    for i in 1..paras.len() {
        let a: String = paras[i - 1].chars().take(6).collect();
        let b: String = paras[i].chars().take(6).collect();
        if a.chars().count() == 6 && a == b {
            same_head.push(i);
        }
    }
    if !same_head.is_empty() {
        issues.push(issue(profile, "style.repeat_head", "重复", format!("{} 处相邻段落开头相同", same_head.len()), same_head, vec![], None));
    }
    // 敏感词（作者自定义层）
    let hits: Vec<&String> = sensitive.iter().filter(|w| !w.is_empty() && text.contains(w.as_str())).collect();
    if !hits.is_empty() {
        issues.push(issue(profile, "structure.sensitive", "合规", format!("命中自定义敏感词：{}", hits.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("、")), vec![], vec![], None));
    }
    (m, issues)
}

fn window_hash(window: &[char]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    window.hash(&mut hasher);
    hasher.finish()
}

/// 生成稿与来源原文连续 `OVERLAP_MIN_CHARS` 字以上相同的片段（不计空白）。
/// 来源按窗口建表，生成稿逐窗口查，连续命中合并成一段；每段报来源、原文和所在段落。这是程序性核对，不是风格判断。
pub fn verbatim_overlap(text: &str, sources: &[(String, String)]) -> Vec<Value> {
    let min = OVERLAP_MIN_CHARS;
    let draft: Vec<(char, usize)> = paragraphs(text)
        .iter()
        .enumerate()
        .flat_map(|(i, p)| p.chars().filter(|c| !c.is_whitespace()).map(move |c| (c, i)).collect::<Vec<_>>())
        .collect();
    if draft.len() < min || sources.is_empty() {
        return Vec::new();
    }
    let mut budget = OVERLAP_MAX_SOURCE_CHARS;
    let normalized: Vec<Vec<char>> = sources
        .iter()
        .map(|(_, s)| {
            let chars: Vec<char> = s.chars().filter(|c| !c.is_whitespace()).take(budget).collect();
            budget = budget.saturating_sub(chars.len());
            chars
        })
        .collect();
    let mut index: HashMap<u64, (usize, usize)> = HashMap::new();
    for (sid, chars) in normalized.iter().enumerate() {
        for (pos, window) in chars.windows(min).enumerate() {
            index.entry(window_hash(window)).or_insert((sid, pos));
        }
    }
    let matches_at = |i: usize| -> Option<usize> {
        let window: Vec<char> = draft[i..i + min].iter().map(|(c, _)| *c).collect();
        let (sid, pos) = *index.get(&window_hash(&window))?;
        (normalized[sid][pos..pos + min] == window[..]).then_some(sid)
    };
    let mut hits = Vec::new();
    let mut i = 0;
    while i + min <= draft.len() && hits.len() < OVERLAP_MAX_HITS {
        let Some(sid) = matches_at(i) else {
            i += 1;
            continue;
        };
        let start = i;
        let mut end = i + min;
        while end < draft.len() && matches_at(end + 1 - min) == Some(sid) {
            end += 1;
        }
        let paragraphs: Vec<usize> = draft[start..end].iter().map(|(_, p)| *p).collect::<BTreeSet<_>>().into_iter().collect();
        hits.push(json!({"source":sources[sid].0,"text":draft[start..end].iter().map(|(c,_)|*c).collect::<String>(),
            "chars":end-start,"paragraphs":paragraphs}));
        i = end;
    }
    hits
}

async fn custom_sensitive(db: &mut SqliteConnection) -> Result<Vec<String>, String> {
    let Some(raw) = get(db, "app-settings", "ew-local-sensitive-words").await? else {
        return Ok(Vec::new());
    };
    let lexicon: Value = if let Some(s) = raw.as_str() {
        serde_json::from_str(s).unwrap_or(Value::Null)
    } else {
        raw.clone()
    };
    Ok(lexicon["custom"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|e| e["word"].as_str().map(str::to_owned))
        .collect())
}

pub struct CheckRequest<'a> {
    pub book_id: &'a str,
    pub text: &'a str,
    pub chapter_id: Option<&'a str>,
    pub artifact_id: &'a str,
    pub reviewed_revision: i64,
    pub ruleset_id: Option<&'a str>,
    pub profile_id: Option<&'a str>,
    /// 作者要求或起草时定下的篇幅；没有时按画像区间核对
    pub target_words: Option<usize>,
}

/// 规则集绑定的文风档案；没有规则集或没绑文风时为 None。
async fn bound_style_profile(db: &mut SqliteConnection, ruleset_id: Option<&str>) -> Result<Option<Value>, String> {
    let Some(rid) = ruleset_id.filter(|r| !r.is_empty() && *r != "none") else {
        return Ok(None);
    };
    match crate::agent_ruleset::load(db, rid).await {
        Ok(ruleset) => crate::agent_style_profile::bound(db, &ruleset).await,
        Err(_) => Ok(None),
    }
}

/// 指纹基线：绑定的文风档案优先，其次作者自己确认过的样本（样本不足时没有基线）。
async fn fingerprint_baseline(db: &mut SqliteConnection, book_id: &str, bound: Option<&Value>) -> Result<Option<(String, Value)>, String> {
    if let Some(profile) = bound {
        if profile["fingerprint"].is_object() {
            let name = profile["source"]["title"].as_str().unwrap_or("参考文风");
            return Ok(Some((format!("参考文风：{name}"), profile["fingerprint"].clone())));
        }
    }
    if book_id.is_empty() {
        return Ok(None);
    }
    let Some(style) = get(db, canon::CANON_NS, &format!("style:{book_id}")).await? else {
        return Ok(None);
    };
    if style["insufficient"] == true || !style["metrics"]["fingerprint"].is_object() {
        return Ok(None);
    }
    Ok(Some(("作者确认的样本".to_owned(), style["metrics"]["fingerprint"].clone())))
}

fn overlap_issues(profile: &Value, text: &str, sources: &[(String, String)]) -> Vec<Value> {
    verbatim_overlap(text, sources)
        .into_iter()
        .map(|hit| {
            let paragraphs = hit["paragraphs"].as_array().into_iter().flatten().filter_map(|v| v.as_u64().map(|v| v as usize)).collect::<Vec<_>>();
            let quote = hit["text"].as_str().unwrap_or("").to_owned();
            let mut value = issue(profile, "overlap.reference", "防抄",
                format!("与{}连续 {} 字相同：“{}”", hit["source"].as_str().unwrap_or("参考书"), hit["chars"], quote.chars().take(40).collect::<String>()),
                paragraphs, vec![quote], Some("用自己的话重写这一段；参考书只借写法，不借句子"));
            value["id"] = json!(format!("overlap.reference:{}:{}", hit["paragraphs"][0].as_u64().unwrap_or(0), fingerprint(hit["text"].as_str().unwrap_or("")).chars().take(6).collect::<String>()));
            value
        })
        .collect()
}

/// 完整的确定性报告：结构问题 + 该章的状态疑点 + 与参考书的逐字重合 + 指纹偏移；`critic` 留给评审填充。
pub async fn check(db: &mut SqliteConnection, req: &CheckRequest<'_>) -> Result<Value, String> {
    let profile_id = match req.profile_id.filter(|p| !p.is_empty()) {
        Some(p) => p.to_owned(),
        None => canon::book_profile_id(db, req.book_id).await?,
    };
    let profile = canon::platform_profile(db, &profile_id).await?;
    let sensitive = custom_sensitive(db).await?;
    let (metrics, mut issues) = rule_issues(req.text, &profile, &sensitive, req.target_words);
    if let Some(chapter_id) = req.chapter_id {
        for suspect in canon::list_suspects(db, req.book_id).await? {
            if suspect["chapterId"] != chapter_id {
                continue;
            }
            let quotes = suspect["quotes"].as_array().into_iter().flatten().filter_map(|q| q.as_str().map(str::to_owned)).collect::<Vec<_>>();
            let code = suspect["code"].as_str().unwrap_or("state.conflict");
            issues.push(json!({"id":suspect["issueKey"],"source":"rule","code":code,"dimension":"连续性","level":level_for(&profile,code),
                "message":suspect["message"],"paragraphs":[],"quotes":quotes,"fix":"确认是回忆、伪装或合理剧情则驳回；否则修改本章或前文"}));
        }
    }
    let bound = bound_style_profile(db, req.ruleset_id).await?;
    if let Some(style_profile) = &bound {
        let sources = crate::agent_style_profile::reference_texts(db, style_profile).await?;
        issues.extend(overlap_issues(&profile, req.text, &sources));
    }
    let drift = fingerprint_baseline(db, req.book_id, bound.as_ref())
        .await?
        .map(|(label, baseline)| crate::agent_style::drift(&metrics["fingerprint"], &baseline, &label));
    finish_report(req, &profile_id, issues, metrics, drift)
}

fn finish_report(req: &CheckRequest<'_>, profile_id: &str, issues: Vec<Value>, metrics: Value, drift: Option<Value>) -> Result<Value, String> {
    let mut report = json!({"version":1,"artifactId":req.artifact_id,"reviewedRevision":req.reviewed_revision,"reviewedFingerprint":fingerprint(req.text),
        "rulesetId":req.ruleset_id.filter(|r| !r.is_empty()).unwrap_or("none"),"chapterId":req.chapter_id,"profileId":profile_id,
        "issues":issues,"metrics":metrics,"critic":{"status":"not_run"}});
    if let Some(drift) = drift {
        report["styleDrift"] = drift;
    }
    let mut report = crate::agent_contract::canonical_arguments(&report);
    if report["chapterId"].is_null() {
        report.as_object_mut().unwrap().remove("chapterId");
    }
    crate::agent_contract::validate_document("qualityReport", &report)?;
    let blocked = report["issues"].as_array().unwrap().iter().any(|i| i["level"] == "block");
    report["blocked"] = json!(blocked);
    Ok(report)
}

/// 短篇默认画像：不挂作品、也没指定画像时按知乎盐选检查。
pub const SHORT_PROFILE: &str = "zhihu";

/// 去掉引号里的对白，只留叙述。
fn narration(paragraph: &str) -> String {
    let mut out = String::new();
    let mut depth = 0usize;
    for ch in paragraph.chars() {
        match ch {
            '“' | '「' => depth += 1,
            '”' | '」' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(ch),
            _ => {}
        }
    }
    out
}

/// 短篇专项：叙述人称是否中途改变。反转数量、结尾写法这类属于写法判断，不在程序里数。
pub fn short_issues(text: &str, profile: &Value) -> Vec<Value> {
    let paras = paragraphs(text);
    let mut issues = Vec::new();
    let told: Vec<(usize, String)> = paras.iter().enumerate().map(|(i, p)| (i, narration(p))).filter(|(_, n)| n.chars().count() >= 6).collect();
    if told.len() >= 9 {
        let first_person = told.iter().take(3).any(|(_, n)| n.contains('我'));
        let tail = &told[told.len() * 2 / 3..];
        let with_i: Vec<usize> = tail.iter().filter(|(_, n)| n.contains('我')).map(|(i, _)| *i).collect();
        if first_person && with_i.is_empty() {
            issues.push(issue(profile, "short.person", "人称", "开头是第一人称，最后三分之一的叙述里再没有“我”".into(),
                tail.iter().take(3).map(|(i, _)| *i).collect(), vec![], Some("确认叙述者没有中途换成第三人称")));
        } else if !first_person && with_i.len() * 10 >= tail.len() * 3 {
            issues.push(issue(profile, "short.person", "人称", "开头是第三人称，后段叙述里频繁出现“我”".into(),
                with_i.iter().take(3).copied().collect(), vec![], Some("统一叙述人称")));
        }
    }
    issues
}

/// 短篇稿体检：按短篇画像查字数、重复、敏感词与人称；没有长篇的状态疑点。
pub async fn check_short(db: &mut SqliteConnection, req: &CheckRequest<'_>) -> Result<Value, String> {
    let profile_id = req.profile_id.filter(|p| !p.is_empty()).unwrap_or(SHORT_PROFILE).to_owned();
    let profile = canon::platform_profile(db, &profile_id).await?;
    let sensitive = custom_sensitive(db).await?;
    let (metrics, mut issues) = rule_issues(req.text, &profile, &sensitive, req.target_words);
    if metrics["words"] != 0 {
        issues.extend(short_issues(req.text, &profile));
    }
    finish_report(req, &profile_id, issues, metrics, None)
}

/// 会话成果体检的统一入口：短篇稿按短篇规则，章节候选按作品画像与状态层。
pub async fn check_artifact(db: &mut SqliteConnection, artifact: &Value, text: &str, artifact_id: &str, revision: i64,
    ruleset_id: Option<&str>) -> Result<Value, String> {
    let short = artifact["kind"] == "short";
    let book_id = artifact["bookId"].as_str().unwrap_or("");
    let req = CheckRequest {
        book_id,
        text,
        chapter_id: if short { None } else { artifact["chapterId"].as_str() },
        artifact_id,
        reviewed_revision: revision,
        ruleset_id,
        profile_id: if short { artifact["profileId"].as_str() } else { None },
        target_words: artifact["targetWords"].as_u64().map(|v| v as usize),
    };
    let mut report = if short { check_short(db, &req).await? } else { check(db, &req).await? };
    if let Some(fingerprint)=crate::agent_writing_basis::fingerprint(artifact) { report["writingBasisFingerprint"]=json!(fingerprint); }
    Ok(report)
}

/// 可以体检、定向修订的正文类成果。
pub fn is_prose(artifact: &Value) -> bool {
    artifact["kind"] == "chapter" || artifact["kind"] == "short"
}

/// 便于 UI 与工具使用的摘要：各级别数量。
pub fn summary(report: &Value) -> Value {
    let mut counts = json!({"block":0,"suspect":0,"notice":0});
    for i in report["issues"].as_array().into_iter().flatten() {
        let level = i["level"].as_str().unwrap_or("notice");
        counts[level] = json!(counts[level].as_u64().unwrap_or(0) + 1);
    }
    let mut out = json!({"counts":counts,"blocked":report["blocked"],"words":report["metrics"]["words"],
        "artifactId":str_field(report,"artifactId").unwrap_or(""),"reviewStatus":report["critic"]["status"],
        "message":if report["critic"]["status"] == "available" { "基础检查与编辑评审已完成，请结合具体建议判断，不代表质量保证。" } else { "仅有基础检查，编辑评审未完成，不能据此判断语言质量。" }});
    if report["styleDrift"].is_object() {
        out["driftFlagged"] = report["styleDrift"]["flagged"].clone();
    }
    out
}
