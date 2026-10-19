//! 文风档案：写作时真正给写作模型看的东西是"范例原文"，不是形容词，也不是数字。
//!
//! 一份档案三层：① 指纹（程序算的 15 项统计，只给人看）② 范例库（按场景类型挑出的原文段落，写作时取几段）
//! ③ 写法卡（模型写、程序核对每条都指向一段真实范例；只给规划模型、编辑评审和作者看，不给写作模型）。
//! 档案物理上是一份 `kind: style` 技能里的 `references/examples.json`，沿用技能的安装、启停、绑定与候选流程。
//! 旧的只有 SKILL.md 正文的文风技能没有这份文件：写作时就没有范例可取，只能靠前文续写。
use crate::agent_store::NS;
use crate::agent_store::get;
use serde_json::{json, Value};
use sqlx::SqliteConnection;

/// 范例文件在技能目录里的位置。
pub const EXAMPLES_PATH: &str = "references/examples.json";
/// 场景类型：范例按它分类，规划模型给每个场景标一个，宿主按它挑范例。
pub const SCENE_KINDS: &[&str] = &["开章", "对白", "打斗", "心理", "过渡", "收尾"];
/// 每段范例的字数范围、每类上限、一次写作取几段、总字数上限。
pub const EXAMPLE_MIN_CHARS: usize = 300;
pub const EXAMPLE_MAX_CHARS: usize = 600;
pub const EXAMPLES_PER_KIND: usize = 6;
pub const EXAMPLES_PER_SCENE: usize = 5;
pub const EXAMPLE_CHARS_PER_SCENE: usize = 2600;

/// Evidence excerpts may omit intervening paragraphs; each retained paragraph
/// must still occur verbatim and in source order.
pub fn source_matches(source: &str, excerpt: &str) -> bool {
    let mut cursor = 0;
    let mut found = false;
    for part in excerpt.lines().map(str::trim).filter(|p|!p.is_empty()) {
        let Some(offset) = source[cursor..].find(part) else { return false; };
        cursor += offset + part.len();
        found = true;
    }
    found
}

/// 技能记录里的档案；没有 examples.json 返回 None。
pub fn from_skill_draft(draft: &Value) -> Option<Value> {
    let file = draft["files"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|f| f["path"] == EXAMPLES_PATH)?;
    let content = file["content"].as_str()?;
    let profile: Value = serde_json::from_str(content).ok()?;
    profile["examples"].is_array().then_some(profile)
}

/// 按技能编号读档案。
pub async fn load(db: &mut SqliteConnection, skill_id: &str) -> Result<Option<Value>, String> {
    let Some(record) = get(db, NS, &format!("skill:{skill_id}")).await? else {
        return Ok(None);
    };
    if record["deletedAt"].is_number() || record["kind"] != "style" {
        return Ok(None);
    }
    let draft = crate::agent_skill_files::read_draft(&record)?;
    if let Some(profile) = from_skill_draft(&draft) {
        let examples = profile["examples"].as_array().unwrap();
        if examples.is_empty() || examples.iter().any(|e| !e["text"].as_str().is_some_and(|t|!t.trim().is_empty())) {
            return Err("文风档案没有完整的原文范例，未将其标记为已应用".into());
        }
        for example in examples.iter().filter(|e|e["source"]["type"] == "reference") {
            let source = &example["source"];
            let body = crate::agent_breakdown::chapter(db,crate::agent_store::str_field(source,"projectId")?,crate::agent_store::str_field(source,"chapterId")?).await?;
            if !source_matches(body["textContent"].as_str().unwrap_or(""),example["text"].as_str().unwrap()) {
                return Err("文风范例与参考书原文不一致，请重新准备档案".into());
            }
        }
        return Ok(Some(profile));
    }
    // Legacy evidence is converted from verified source text, never from the
    // old prose instructions (which can describe plot devices rather than style).
    let mut examples = Vec::new();
    let mut chapters = std::collections::BTreeMap::<String, Value>::new();
    let mut source = json!({"type":"reference","title":record["name"],"chapters":[]});
    for file in draft["files"].as_array().into_iter().flatten() {
        if file["path"] == "references/coverage.json" {
            if let Ok(coverage) = serde_json::from_str::<Value>(file["content"].as_str().unwrap_or("")) {
                source["title"] = coverage["title"].clone();
            }
        }
        if !file["path"].as_str().is_some_and(|p|p.starts_with("references/evidence-") && p.ends_with(".json")) { continue; }
        let evidence: Value = serde_json::from_str(file["content"].as_str().unwrap_or(""))
            .map_err(|_|"旧文风技能的原文证据格式损坏，未启用不完整范例".to_owned())?;
        for item in evidence.as_object().into_iter().flat_map(|m|m.values()) {
            let (Some(pid), Some(cid), Some(quote)) = (item["projectId"].as_str(), item["chapterId"].as_str(), item["quote"].as_str()) else { continue; };
            let key = format!("{pid}:{cid}");
            if !chapters.contains_key(&key) {
                chapters.insert(key.clone(), crate::agent_breakdown::chapter(db,pid,cid).await?);
            }
            let body = chapters[&key]["textContent"].as_str().unwrap_or("");
            if !source_matches(body,quote) {
                return Err("旧文风证据与参考书原文不一致，未静默应用失效范例".into());
            }
            if quote.chars().count() < 80 { continue; }
            source["projectId"] = json!(pid);
            let entries = source["chapters"].as_array_mut().unwrap();
            if !entries.iter().any(|v|v["chapterId"] == cid) { entries.push(json!({"chapterId":cid,"no":item["chapterNo"]})); }
            examples.push(json!({"id":format!("legacy:{skill_id}:{}",item["id"].as_str().unwrap_or("")),
                "kind":"未分类","text":quote.trim(),"source":{"type":"reference","projectId":pid,"chapterId":cid,
                "fingerprint":crate::agent_quality::fingerprint(body),"evidenceId":item["id"]}}));
        }
    }
    if examples.is_empty() { return Ok(None); }
    let profile = json!({"version":1,"migrationVersion":1,"source":source,"examples":examples,"notes":[],"narrative":[],
        "legacySkillRevision":draft["revision"],"migrationNote":"已从旧版证据恢复原文范例，未把剧情套路转换成写法卡。"});
    let key = format!("style-profile:{skill_id}");
    if get(db,NS,&key).await?.as_ref() != Some(&profile) { crate::agent_store::put(db,NS,&key,&profile).await?; }
    Ok(Some(profile))
}

/// 规则集绑定的文风档案。
pub async fn bound(db: &mut SqliteConnection, ruleset: &Value) -> Result<Option<Value>, String> {
    match ruleset["styleSkillId"].as_str().filter(|s| !s.is_empty()) {
        Some(id) => match load(db, id).await? {
            Some(profile) => Ok(Some(profile)),
            None => Err("所选文风技能缺少可核实的原文范例，请补充范例或重新蒸馏；本次未假定文风已生效".into()),
        },
        None => Ok(None),
    }
}

/// 档案来源的全部原文（章节标签 + 正文），供逐字重合检查；来源不是参考书时为空。
pub async fn reference_texts(db: &mut SqliteConnection, profile: &Value) -> Result<Vec<(String, String)>, String> {
    let source = &profile["source"];
    let Some(pid) = source["projectId"].as_str().filter(|_| source["type"] == "reference") else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for chapter in source["chapters"].as_array().into_iter().flatten() {
        let Some(cid) = chapter["chapterId"].as_str() else { continue };
        let Ok(body) = crate::agent_breakdown::chapter(db, pid, cid).await else { continue };
        let label = format!("{}《{}》第 {} 章 {}", source["title"].as_str().unwrap_or(""), source["title"].as_str().unwrap_or(""),
            chapter["no"].as_u64().unwrap_or(0), body["title"].as_str().unwrap_or(""));
        out.push((label, body["textContent"].as_str().unwrap_or("").to_owned()));
    }
    Ok(out)
}

fn terms(text: &str) -> std::collections::BTreeSet<String> {
    let chars = text.chars().collect::<Vec<_>>();
    chars.windows(2).filter(|w| w.iter().all(|c| c.is_alphanumeric())).map(|w| w.iter().collect()).collect()
}

/// 为一个场景挑范例：先同类型，再按要点词面相近补足，总量不超过字数上限。只返回原文和来源，不带任何统计。
pub fn select_examples(profile: &Value, kind: &str, points: &str, limit: usize) -> Vec<Value> {
    let examples = profile["examples"].as_array().cloned().unwrap_or_default();
    let query = terms(points);
    let mut ranked: Vec<(usize, usize, Value)> = examples
        .into_iter()
        .filter(|e| e["text"].as_str().is_some_and(|t| !t.trim().is_empty()))
        .map(|e| {
            let same_kind = (e["kind"] == kind) as usize;
            let overlap = terms(e["text"].as_str().unwrap_or("")).intersection(&query).count();
            (same_kind, overlap, e)
        })
        .collect();
    ranked.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)).then(a.2["id"].as_str().cmp(&b.2["id"].as_str())));
    let mut out = Vec::new();
    let mut chars = 0usize;
    for (_, _, example) in ranked {
        let len = example["text"].as_str().map_or(0, |t| t.chars().count());
        if out.len() >= limit || (chars + len > EXAMPLE_CHARS_PER_SCENE && !out.is_empty()) {
            break;
        }
        chars += len;
        out.push(json!({"id":example["id"],"kind":example["kind"],"text":example["text"],"source":example["source"]}));
    }
    out
}

/// 写法卡与叙事模式的可读文本：给规划模型与作者，不含数字。
pub fn planner_text(profile: &Value) -> String {
    let mut sections = Vec::new();
    let notes = profile["notes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|n| n["text"].as_str().map(|t| format!("- {t}")))
        .collect::<Vec<_>>();
    if !notes.is_empty() {
        sections.push(format!("【写法卡】\n{}", notes.join("\n")));
    }
    let narrative = profile["narrative"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|n| n["text"].as_str().map(|t| format!("- {t}")))
        .collect::<Vec<_>>();
    if !narrative.is_empty() {
        sections.push(format!("【叙事模式】\n{}", narrative.join("\n")));
    }
    sections.join("\n\n")
}

/// 作者自己确认过的样本切块，整理成与文风档案同形状的范例库（不含写法卡）。
/// 类型按程序能看见的事实标：章首块是开章、章末块是收尾、对白字符过半是对白，其余标“叙述”，挑范例时按词面相近补。
pub async fn author_profile(db: &mut SqliteConnection, book_id: &str) -> Result<Option<Value>, String> {
    use sqlx::Row;
    if book_id.is_empty() {
        return Ok(None);
    }
    let rows = sqlx::query("SELECT chapterId,seq,text,sourceRevision FROM chapter_chunks WHERE bookId=? AND provenance='author_confirmed' ORDER BY chapterId,seq")
        .bind(book_id)
        .fetch_all(&mut *db)
        .await
        .map_err(crate::agent_store::err)?;
    if rows.is_empty() {
        return Ok(None);
    }
    let mut by_chapter: std::collections::BTreeMap<String, Vec<(i64, String, String)>> = std::collections::BTreeMap::new();
    for row in rows {
        let cid: String = row.get("chapterId");
        by_chapter.entry(cid).or_default().push((row.get("seq"), row.get("text"), row.get("sourceRevision")));
    }
    let mut examples = Vec::new();
    for (cid, chunks) in by_chapter {
        let current = crate::agent_indexer::chapter_fingerprint(db, book_id, &cid).await?;
        let last = chunks.len().saturating_sub(1);
        for (position, (seq, text, revision)) in chunks.into_iter().enumerate() {
            if current.as_deref() != Some(revision.as_str()) || text.chars().count() < EXAMPLE_MIN_CHARS / 2 {
                continue;
            }
            let dialogue: usize = text.split('\n').filter(|p| p.contains('“') || p.contains('"') || p.contains('「')).map(|p| p.chars().count()).sum();
            let kind = if position == 0 { "开章" } else if position == last { "收尾" } else if dialogue * 2 >= text.chars().count() { "对白" } else { "叙述" };
            let body: String = text.chars().take(EXAMPLE_MAX_CHARS).collect();
            examples.push(json!({"id":format!("author:{book_id}:{cid}:{seq}:{revision}"),"kind":kind,"text":body,
                "source":{"type":"sample","bookId":book_id,"chapterId":cid,"seq":seq,"fingerprint":revision}}));
        }
    }
    if examples.is_empty() {
        return Ok(None);
    }
    Ok(Some(json!({"version":1,"source":{"type":"author","bookId":book_id,"title":"作者确认的样本"},"examples":examples,"notes":[],"narrative":[]})))
}

/// 为一个场景从几份档案里挑范例：先绑定的参考文风，再作者自己的样本；总数与总字数都有上限。
pub fn select_for_scene(profiles: &[&Value], kind: &str, points: &str) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    let mut chars = 0usize;
    for profile in profiles {
        for example in select_examples(profile, kind, points, EXAMPLES_PER_SCENE) {
            let len = example["text"].as_str().map_or(0, |t| t.chars().count());
            if out.len() >= EXAMPLES_PER_SCENE || (chars + len > EXAMPLE_CHARS_PER_SCENE && !out.is_empty()) {
                return out;
            }
            if out.iter().any(|e| e["id"] == example["id"]) {
                continue;
            }
            chars += len;
            out.push(example);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn evidence_requires_verbatim_source_order() {
        assert!(source_matches("甲站起身。\n乙把门关上。\n屋里静了。","甲站起身。\n屋里静了。"));
        assert!(!source_matches("甲站起身。\n乙把门关上。","乙把门关上。\n甲站起身。"));
        assert!(!source_matches("甲站起身。","甲坐下了。"));
    }
    #[test]
    fn legacy_style_is_migrated_from_verified_evidence_without_rewriting_instructions() {
        tauri::async_runtime::block_on(async {
            let root=std::env::temp_dir().join(format!("ew-style-migration-{}",crate::agent_store::id()));
            std::fs::create_dir_all(&root).unwrap();
            let mut db=crate::desktop_storage::connect_at(&root).await.unwrap();
            let text="他把门推开，见桌上的灯还亮着，便把带来的东西搁在门边，先去厨房取了一只碗。".repeat(3);
            let imported=crate::agent_breakdown::import_book(&mut db,"参考书",&json!([{"title":"第一章","text":text}]),"",&json!([])).await.unwrap();
            let pid=imported["projectId"].as_str().unwrap();
            let project=crate::agent_breakdown::project(&mut db,pid).await.unwrap();
            let cid=project["chapters"][0]["id"].to_string();
            let draft=crate::agent_skill_files::Draft{id:String::new(),name:"旧文风".into(),description:"旧技能".into(),
                instructions:"旧的剧情套路说明保留原样".into(),kind:Some("style".into()),replaces:None,revision:None,
                files:vec![crate::agent_skill_files::SourceFile{path:"references/evidence-1.json".into(),content:json!({"leaf#0":{
                    "id":"leaf#0","projectId":pid,"chapterId":cid,"chapterNo":1,"quote":text}}).to_string()}]};
            let record=crate::agent_skill_files::save(&mut db,&root.join("skills"),&draft).await.unwrap();
            let skill_id=record["id"].as_str().unwrap();
            let profile=load(&mut db,skill_id).await.unwrap().unwrap();
            assert_eq!(profile["examples"][0]["text"],text);
            assert_eq!(profile["notes"],json!([]));
            assert!(get(&mut db,NS,&format!("style-profile:{skill_id}")).await.unwrap().is_some());
            assert_eq!(crate::agent_skill_files::read_draft(&record).unwrap()["instructions"],draft.instructions);
            drop(db);
            std::fs::remove_dir_all(root).unwrap();
        });
    }
}
