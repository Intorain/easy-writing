//! 作者风格档案与表达示例。统计只描述样本，不能自动成为写作要求。
//! 样本不足仅影响统计可信度；短样本仍可作为表达示例，作者锁定的要点始终优先。
use crate::agent_canon::{AUTHOR_NS, CANON_NS};
use crate::agent_store::{err, get, now, put};
use serde_json::{json, Value};
use sqlx::{Row, SqliteConnection};
use std::collections::BTreeMap;

/// Resource bounds, not literary criteria. The planner chooses examples by meaning;
/// lexical overlap only provides a bounded shortlist when no choice is supplied.
pub const EXAMPLE_LIMIT: usize = 2;
pub const EXAMPLE_CATALOG_LIMIT: usize = 8;

fn terms(text: &str) -> std::collections::BTreeSet<String> {
    let chars = text.chars().collect::<Vec<_>>();
    chars.windows(2).filter(|w| w.iter().all(|c| c.is_alphanumeric()))
        .map(|w| w.iter().collect()).collect()
}

pub async fn examples(db: &mut SqliteConnection, book_id: &str, query: &str) -> Result<Vec<Value>, String> {
    let mut examples: Vec<Value> = serde_json::from_str(include_str!("../resources/prose-examples.json")).map_err(err)?;
    let rows = sqlx::query("SELECT chapterId,seq,text,sourceRevision,sceneKind FROM chapter_chunks WHERE bookId=? AND provenance='author_confirmed' ORDER BY chapterId,seq")
        .bind(book_id).fetch_all(&mut *db).await.map_err(err)?;
    let mut fingerprints = BTreeMap::new();
    for row in rows {
        let cid: String = row.get("chapterId");
        if !fingerprints.contains_key(&cid) {
            fingerprints.insert(cid.clone(), crate::agent_indexer::chapter_fingerprint(db, book_id, &cid).await?);
        }
        let fingerprint: String = row.get("sourceRevision");
        if fingerprints.get(&cid).and_then(|v| v.as_deref()) != Some(fingerprint.as_str()) { continue; }
        let seq: i64 = row.get("seq");
        let text: String = row.get("text");
        examples.push(json!({"id":format!("author:{book_id}:{cid}:{seq}:{fingerprint}"),"useFor":"作者确认的正文；按当前场景语义选择",
            "principle":"只参照表达和人物回应的组织方式，不能借用示例事件；不能从词频自行推导作者规则。", "text":text,
            "source":{"type":"sample","bookId":book_id,"chapterId":cid,"seq":seq,"fingerprint":fingerprint}}));
    }
    let query_terms = terms(query);
    examples.sort_by_cached_key(|example| {
        let searchable = format!("{} {} {}", example["useFor"].as_str().unwrap_or(""), example["principle"].as_str().unwrap_or(""), example["text"].as_str().unwrap_or(""));
        let overlap = terms(&searchable).intersection(&query_terms).count();
        (std::cmp::Reverse(overlap), example["source"]["type"] != "sample", example["id"].as_str().unwrap_or("").to_owned())
    });
    Ok(examples)
}

pub async fn select_examples(db: &mut SqliteConnection, book_id: &str, query: &str, ids: &[String]) -> Result<Vec<Value>, String> {
    let available = examples(db, book_id, query).await?;
    if ids.is_empty() {
        // Personal samples are an author opt-in. Generic demonstrations require
        // an explicit task-level choice; do not impose their voice by default.
        return Ok(available.into_iter().filter(|e|e["source"]["type"]=="sample").take(EXAMPLE_LIMIT).collect());
    }
    if ids.len() > EXAMPLE_LIMIT { return Err("表达示例超出上下文数量限制".into()); }
    let mut selected = Vec::new();
    for id in ids {
        let example = available.iter().find(|e| e["id"] == *id).ok_or("表达示例已变化或不属于本书，请重新 read_style_profile 并选择")?;
        if !selected.iter().any(|e: &Value| e["id"] == *id) { selected.push(example.clone()); }
    }
    Ok(selected)
}

/// One representation is shared by chapter and independent short-story inputs.
pub fn example_block(example: &Value) -> Value {
    let text=format!("用途：{}\n参考：{}\n{}", example["useFor"].as_str().unwrap_or(""),example["principle"].as_str().unwrap_or(""),example["text"].as_str().unwrap_or(""));
    let mut source=example["source"].clone();source["exampleId"]=example["id"].clone();
    json!({"label":"表达示例（只参照写法，人物与事件不属于本书）","tokens":crate::agent_context::estimate_tokens(&text),"text":text,"source":source})
}

pub const MIN_SAMPLE_CHARS: usize = 10_000;
const STOP_WORDS: &[&str] = &[
    "的", "了", "是", "在", "我", "他", "她", "这", "那", "就", "也", "不", "都", "和", "与", "着", "过", "一个", "什么", "没有",
    "自己", "他们", "我们", "你们", "这个", "那个", "一下", "一声", "一样", "起来", "出来", "已经", "还是", "但是", "然后", "因为",
    "所以", "如果", "可以", "知道", "看着", "说道", "时候", "地方", "东西", "这么", "那么", "怎么", "只是", "不是", "就是", "有些",
];

fn std_dev(values: &[f64]) -> f64 {
    if values.len() < 2 {
        return 0.0;
    }
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    (values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / values.len() as f64).sqrt()
}

fn round1(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

fn sentences(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        if ch == '\n' {
            if !current.trim().is_empty() {
                out.push(current.trim().to_owned());
            }
            current.clear();
            continue;
        }
        current.push(ch);
        if matches!(ch, '。' | '！' | '？' | '!' | '?' | '…' | '；') {
            if !current.trim().is_empty() {
                out.push(current.trim().to_owned());
            }
            current.clear();
        }
    }
    if !current.trim().is_empty() {
        out.push(current.trim().to_owned());
    }
    out
}

/// 口头禅候选里排除在首尾的虚字：以这些字开头或结尾的窗口多半是跨词切片（“他把”“檐角的”），不是口头禅。
const EDGE_STOP_CHARS: &[char] = &[
    '的', '了', '是', '在', '我', '他', '她', '你', '这', '那', '就', '也', '不', '都', '和', '与', '着', '过', '把', '被', '给', '让', '向',
    '从', '对', '没', '有', '上', '下', '里', '中', '又', '才', '还', '很', '更',
];

/// 高频短语：2–3 字连续片段，去掉含标点、空白、首尾虚字的与常用虚词；出现 3 次以上的按频次排序。
/// 三字短语的频次接近它包含的二字短语时（八成以上），二字多半只是三字的一部分，保留更长的；
/// 否则保留二字（更可能是一个词）。同频次时短的排前面。
pub fn frequent_phrases(text: &str, limit: usize) -> Vec<(String, usize)> {
    let chars: Vec<char> = text.chars().collect();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for width in [2usize, 3] {
        if chars.len() < width {
            continue;
        }
        for window in chars.windows(width) {
            if window.iter().any(|c| !(c.is_alphanumeric()) || c.is_ascii()) {
                continue;
            }
            if EDGE_STOP_CHARS.contains(&window[0]) || EDGE_STOP_CHARS.contains(&window[width - 1]) {
                continue;
            }
            let phrase: String = window.iter().collect();
            if STOP_WORDS.contains(&phrase.as_str()) {
                continue;
            }
            *counts.entry(phrase).or_insert(0) += 1;
        }
    }
    let phrases: Vec<(String, usize)> = counts.into_iter().filter(|(_, n)| *n >= 3).collect();
    let longer: Vec<(String, usize)> = phrases.iter().filter(|(p, _)| p.chars().count() == 3).cloned().collect();
    let mut kept: Vec<(String, usize)> = phrases
        .into_iter()
        .filter(|(p, n)| {
            p.chars().count() == 3 || !longer.iter().any(|(l, ln)| l.contains(p.as_str()) && ln * 10 >= n * 8)
        })
        .collect();
    kept.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.chars().count().cmp(&b.0.chars().count())).then(a.0.cmp(&b.0)));
    kept.truncate(limit);
    kept
}

/// 比喻词：只用来数"每千字几个"，给人看的温度计。不进提示词，不作关卡。
const METAPHOR_MARKERS: &[&str] = &["像", "仿佛", "如同", "宛如", "犹如", "好似", "似的", "一般", "般的"];
/// 短句、长句的分界（字数）；段末短句指段落最后一句不超过短句上限。
const SHORT_SENTENCE: usize = 8;
const LONG_SENTENCE: usize = 30;

/// 指纹 15 项：键、界面标签、相对偏差容忍度。容忍度先照人味项目的经验值（两到六成），回放集跑过后再调。
pub const FINGERPRINT_ITEMS: &[(&str, &str, f64)] = &[
    ("sentenceMean", "平均句长", 0.3),
    ("sentenceCv", "句长变异系数", 0.25),
    ("shortSentenceRatio", "短句占比", 0.4),
    ("longSentenceRatio", "长句占比", 0.5),
    ("paragraphMean", "平均段长", 0.4),
    ("singleSentenceParagraphRatio", "单句段占比", 0.4),
    ("paragraphEndShortRatio", "段末短句占比", 0.4),
    ("dialogueRatio", "对白占比", 0.3),
    ("commaPer1k", "逗号每千字", 0.3),
    ("exclamationPer1k", "叹号每千字", 0.6),
    ("questionPer1k", "问号每千字", 0.6),
    ("ellipsisPer1k", "省略号每千字", 0.6),
    ("dashPer1k", "破折号每千字", 0.6),
    ("metaphorPer1k", "比喻词每千字", 0.5),
    ("dePer100", "“的”每百字", 0.25),
];
/// 超过这么多项偏出容忍度，界面提示"形状漂移"。
pub const DRIFT_FLAG_THRESHOLD: usize = 3;

/// 文风指纹：15 项纯统计。量的是"形状"（句子长短起伏、段落收束、标点与比喻密度），看不见模板句和警句式交代。
pub fn style_fingerprint(text: &str) -> Value {
    let paragraphs: Vec<&str> = text.split('\n').map(str::trim).filter(|p| !p.is_empty()).collect();
    let chars = text.chars().filter(|c| !c.is_whitespace()).count();
    let sents = sentences(text);
    let sentence_lengths: Vec<f64> = sents.iter().map(|s| s.chars().count() as f64).collect();
    let paragraph_lengths: Vec<f64> = paragraphs.iter().map(|p| p.chars().count() as f64).collect();
    let mean = |values: &[f64]| if values.is_empty() { 0.0 } else { values.iter().sum::<f64>() / values.len() as f64 };
    let ratio = |part: usize, whole: usize| if whole == 0 { 0.0 } else { part as f64 / whole as f64 };
    let sentence_mean = mean(&sentence_lengths);
    let sentence_cv = if sentence_mean == 0.0 { 0.0 } else { std_dev(&sentence_lengths) / sentence_mean };
    let short = sents.iter().filter(|s| s.chars().count() <= SHORT_SENTENCE).count();
    let long = sents.iter().filter(|s| s.chars().count() >= LONG_SENTENCE).count();
    let single = paragraphs.iter().filter(|p| sentences(p).len() == 1).count();
    let end_short = paragraphs
        .iter()
        .filter(|p| sentences(p).last().is_some_and(|last| last.chars().count() <= SHORT_SENTENCE))
        .count();
    let dialogue_chars: usize = paragraphs
        .iter()
        .filter(|p| p.contains('“') || p.contains('"') || p.contains('「'))
        .map(|p| p.chars().count())
        .sum();
    let per_1k = |count: usize| if chars == 0 { 0.0 } else { count as f64 * 1000.0 / chars as f64 };
    let count_char = |needle: char| text.matches(needle).count();
    let metaphors: usize = METAPHOR_MARKERS.iter().map(|m| text.matches(m).count()).sum();
    let de = count_char('的');
    json!({
        "sentenceMean":round1(sentence_mean),
        "sentenceCv":(sentence_cv*100.0).round()/100.0,
        "shortSentenceRatio":(ratio(short,sents.len())*100.0).round()/100.0,
        "longSentenceRatio":(ratio(long,sents.len())*100.0).round()/100.0,
        "paragraphMean":round1(mean(&paragraph_lengths)),
        "singleSentenceParagraphRatio":(ratio(single,paragraphs.len())*100.0).round()/100.0,
        "paragraphEndShortRatio":(ratio(end_short,paragraphs.len())*100.0).round()/100.0,
        "dialogueRatio":(ratio(dialogue_chars,chars)*100.0).round()/100.0,
        "commaPer1k":round1(per_1k(count_char('，'))),
        "exclamationPer1k":round1(per_1k(count_char('！'))),
        "questionPer1k":round1(per_1k(count_char('？'))),
        "ellipsisPer1k":round1(per_1k(count_char('…'))),
        "dashPer1k":round1(per_1k(count_char('—'))),
        "metaphorPer1k":round1(per_1k(metaphors)),
        "dePer100":round1(if chars==0 {0.0} else {de as f64*100.0/chars as f64}),
    })
}

/// 指纹偏移：生成稿对基线逐项比。基线很小（每千字不到 1 个）时按绝对差 1 算，避免 0.2 对 0.5 也报偏。
/// 只是温度计：结果给界面，不进提示词，不拦保存，不交给评审。
pub fn drift(current: &Value, baseline: &Value, baseline_label: &str) -> Value {
    let mut items = Vec::new();
    let mut flagged = 0usize;
    for (key, label, tolerance) in FINGERPRINT_ITEMS {
        let (Some(value), Some(base)) = (current[key].as_f64(), baseline[key].as_f64()) else { continue };
        let floor = if key.ends_with("Per1k") { 1.0 } else if key.ends_with("Ratio") || *key == "sentenceCv" { 0.05 } else { 0.5 };
        let diff = value - base;
        let ratio = if base.abs() < f64::EPSILON { if value.abs() < f64::EPSILON { 0.0 } else { 1.0 } } else { diff / base };
        let flag = diff.abs() > floor && ratio.abs() > *tolerance;
        flagged += flag as usize;
        items.push(json!({"key":key,"label":label,"value":value,"baseline":base,"ratio":(ratio*100.0).round()/100.0,"flagged":flag}));
    }
    let note = if flagged >= DRIFT_FLAG_THRESHOLD {
        format!("{flagged} 项指标明显偏离基线，句子或段落的形状可能和参照文风不像；这只是统计提示，像不像以通读为准")
    } else {
        String::new()
    };
    json!({"baseline":baseline_label,"items":items,"flagged":flagged,"note":note})
}

/// 纯文本统计；`text` 是全部样本拼接（段落以换行分隔）。
pub fn metrics(text: &str) -> Value {
    let paragraphs: Vec<&str> = text.split('\n').map(str::trim).filter(|p| !p.is_empty()).collect();
    let chars = text.chars().filter(|c| !c.is_whitespace()).count();
    let sentence_lengths: Vec<f64> = sentences(text).iter().map(|s| s.chars().count() as f64).collect();
    let paragraph_lengths: Vec<f64> = paragraphs.iter().map(|p| p.chars().count() as f64).collect();
    let mean = |values: &[f64]| if values.is_empty() { 0.0 } else { values.iter().sum::<f64>() / values.len() as f64 };
    let dialogue_chars: usize = paragraphs
        .iter()
        .filter(|p| p.contains('“') || p.contains('"') || p.contains('「'))
        .map(|p| p.chars().count())
        .sum();
    let per_1k = |needle: char| if chars == 0 { 0.0 } else { text.matches(needle).count() as f64 * 1000.0 / chars as f64 };
    let openings = paragraphs.iter().take(200).fold((0usize, 0usize, 0usize), |acc, p| {
        let first = p.chars().next().unwrap_or(' ');
        if first == '“' || first == '"' || first == '「' {
            (acc.0 + 1, acc.1, acc.2)
        } else if p.chars().count() <= 12 {
            (acc.0, acc.1 + 1, acc.2)
        } else {
            (acc.0, acc.1, acc.2 + 1)
        }
    });
    let mut out = json!({"chars":chars,"paragraphs":paragraphs.len(),"sentenceMean":round1(mean(&sentence_lengths)),"sentenceStd":round1(std_dev(&sentence_lengths)),
        "paragraphMean":round1(mean(&paragraph_lengths)),"paragraphStd":round1(std_dev(&paragraph_lengths)),
        "dialogueRatio":if chars==0 {0.0} else {((dialogue_chars as f64 / chars as f64)*100.0).round()/100.0},
        "exclamationPer1k":round1(per_1k('！')),"questionPer1k":round1(per_1k('？')),"ellipsisPer1k":round1(per_1k('…')),"dashPer1k":round1(per_1k('—')),
        "openings":{"dialogue":openings.0,"short":openings.1,"long":openings.2}});
    out["fingerprint"] = style_fingerprint(text);
    out
}

fn points_text(m: &Value, phrases: &[(String, usize)]) -> String {
    let mut lines = vec![
        format!("句子平均 {} 字（长短差 {}），段落平均 {} 字", m["sentenceMean"], m["sentenceStd"], m["paragraphMean"]),
        format!("对白约占 {}%", (m["dialogueRatio"].as_f64().unwrap_or(0.0) * 100.0).round()),
    ];
    let mut punct = Vec::new();
    if m["exclamationPer1k"].as_f64().unwrap_or(0.0) > 3.0 {
        punct.push("常用叹号");
    }
    if m["ellipsisPer1k"].as_f64().unwrap_or(0.0) > 2.0 {
        punct.push("常用省略号");
    }
    if m["dashPer1k"].as_f64().unwrap_or(0.0) > 1.0 {
        punct.push("常用破折号");
    }
    if m["exclamationPer1k"].as_f64().unwrap_or(0.0) < 0.5 && m["ellipsisPer1k"].as_f64().unwrap_or(0.0) < 0.5 {
        punct.push("很少用叹号和省略号");
    }
    if !punct.is_empty() {
        lines.push(punct.join("，"));
    }
    let o = &m["openings"];
    let total = o["dialogue"].as_u64().unwrap_or(0) + o["short"].as_u64().unwrap_or(0) + o["long"].as_u64().unwrap_or(0);
    if total > 0 {
        lines.push(format!(
            "段落开头：对白 {}%，短句 {}%，长句 {}%",
            o["dialogue"].as_u64().unwrap_or(0) * 100 / total,
            o["short"].as_u64().unwrap_or(0) * 100 / total,
            o["long"].as_u64().unwrap_or(0) * 100 / total
        ));
    }
    if !phrases.is_empty() {
        lines.push(format!("高频短语（口头禅候选）：{}", phrases.iter().take(12).map(|(p, _)| p.as_str()).collect::<Vec<_>>().join("、")));
    }
    lines.join("；")
}

/// 重算风格档案；返回记录。作者锁定的要点优先于自动统计。
pub async fn rebuild(db: &mut SqliteConnection, book_id: &str) -> Result<Value, String> {
    let rows = sqlx::query("SELECT text FROM chapter_chunks WHERE bookId=? AND provenance='author_confirmed' ORDER BY chapterId,seq")
        .bind(book_id)
        .fetch_all(&mut *db)
        .await
        .map_err(err)?;
    let text = rows.iter().map(|r| r.get::<String, _>("text")).collect::<Vec<_>>().join("\n");
    let m = metrics(&text);
    let sample_chars = m["chars"].as_u64().unwrap_or(0) as usize;
    let insufficient = sample_chars < MIN_SAMPLE_CHARS;
    let phrases = if insufficient { Vec::new() } else { frequent_phrases(&text, 15) };
    let lock = get(db, AUTHOR_NS, &format!("style-lock:{book_id}")).await?;
    let locked_points = lock.as_ref().and_then(|l| l["points"].as_str()).filter(|p| !p.trim().is_empty()).map(str::to_owned);
    let auto_points = if insufficient {
        format!("样本不足：已确认 {sample_chars} 字，统计参考阈值为 {MIN_SAMPLE_CHARS} 字；已确认片段仍可作为表达示例，统计不自动成为规则")
    } else {
        points_text(&m, &phrases)
    };
    let key = format!("style:{book_id}");
    let record = json!({"id":key,"bookId":book_id,"sampleChars":sample_chars,"insufficient":insufficient,"metrics":m,
        "phrases":phrases.iter().map(|(p,n)| json!({"phrase":p,"count":n})).collect::<Vec<_>>(),
        "autoPoints":auto_points,"points":locked_points.clone().unwrap_or_default(),
        "locked":locked_points.is_some(),"computedAt":now()});
    put(db, CANON_NS, &key, &record).await?;
    Ok(record)
}

pub async fn lock_points(db: &mut SqliteConnection, book_id: &str, points: &str) -> Result<Value, String> {
    let key = format!("style-lock:{book_id}");
    let record = json!({"id":key,"bookId":book_id,"points":points.trim(),"updatedAt":now()});
    put(db, AUTHOR_NS, &key, &record).await?;
    rebuild(db, book_id).await
}

/// 作者确认样本后：切块的来源标记随之更新（只在 unknown 与 author_confirmed 之间切换，AI 段落不动），再重算。
pub async fn apply_samples(db: &mut SqliteConnection, book_id: &str, chapter_ids: &[String]) -> Result<Value, String> {
    sqlx::query("UPDATE chapter_chunks SET provenance='unknown' WHERE bookId=? AND provenance='author_confirmed'")
        .bind(book_id)
        .execute(&mut *db)
        .await
        .map_err(err)?;
    if !chapter_ids.is_empty() {
        sqlx::query("UPDATE chapter_chunks SET provenance='author_confirmed' WHERE bookId=? AND provenance='unknown' AND chapterId IN (SELECT value FROM json_each(?))")
            .bind(book_id)
            .bind(serde_json::to_string(chapter_ids).map_err(err)?)
            .execute(&mut *db)
            .await
            .map_err(err)?;
    }
    rebuild(db, book_id).await
}
