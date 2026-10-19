//! 方法包：作者的写法资产导出成一个 JSON 文件，换电脑或分享给别人时导入。
//! 内容是“方法级”的：全局写作规则、自建平台画像、拆书归纳的写法配方（本就不带参考书人物名），
//! 以及各写作方法的效果统计（只有比率与次数，不含任何正文与书名）。作品正文、资料、记忆一律不导出。
//! 导入只增不改：重复的规则跳过；与现有画像同编号的另起编号；配方进“导入的配方”，Agent 仿写时可以参考。
use crate::agent_canon as canon;
use crate::agent_store::{id, list, now, put, str_field};
use serde_json::{json, Value};
use sqlx::SqliteConnection;
use std::collections::BTreeSet;

pub const FORMAT: &str = "ew-method-pack";
pub const VERSION: u64 = 1;
/// 导入的配方不属于任何拆书项目，挂在这个伪项目下。
pub const TEMPLATE_PROJECT: &str = "template";
const MAX_ITEMS: usize = 500;

fn normalized(text: &str) -> String {
    text.chars().filter(|c| c.is_alphanumeric()).collect()
}

/// 全部配方（含导入的），不含已删除。
pub async fn all_recipes(db: &mut SqliteConnection) -> Result<Vec<Value>, String> {
    let mut items = list(db, canon::AUTHOR_NS, "recipe:").await?;
    items.retain(|r| !r["deletedAt"].is_number());
    items.sort_by_key(|r| r["createdAt"].as_i64().unwrap_or(0));
    Ok(items)
}

/// 导入的配方（方法包带来的）。
pub async fn template_recipes(db: &mut SqliteConnection) -> Result<Vec<Value>, String> {
    Ok(all_recipes(db).await?.into_iter().filter(|r| r["projectId"] == TEMPLATE_PROJECT).collect())
}

pub async fn export(db: &mut SqliteConnection) -> Result<Value, String> {
    let rules = canon::list_rules(db, None)
        .await?
        .iter()
        .map(|r| json!({"category":r["category"],"text":r["text"]}))
        .collect::<Vec<_>>();
    let profiles = canon::platform_profiles(db)
        .await?
        .into_iter()
        .filter(|p| p["builtin"] != true)
        .map(|mut p| {
            if let Some(map) = p.as_object_mut() {
                map.remove("updatedAt");
            }
            p
        })
        .collect::<Vec<_>>();
    let recipes = all_recipes(db)
        .await?
        .iter()
        .map(|r| json!({"title":r["title"],"steps":r["steps"],"sourceTitle":r["projectTitle"]}))
        .collect::<Vec<_>>();
    let stats = crate::agent_eval::report(db, None).await?;
    Ok(json!({"format":FORMAT,"version":VERSION,"exportedAt":now(),"rules":rules,"profiles":profiles,"recipes":recipes,
        "methodStats":{"byMethod":stats["byMethod"],"abByMethod":stats["abByMethod"]}}))
}

/// 导入方法包；返回各类新增与跳过的条数。
pub async fn import(db: &mut SqliteConnection, pack: &Value) -> Result<Value, String> {
    if pack["format"] != FORMAT {
        return Err("这不是易创的方法包文件".into());
    }
    if pack["version"].as_u64().unwrap_or(0) > VERSION {
        return Err("方法包来自更新版本的易创，请先升级再导入".into());
    }
    let count = |key: &str| pack[key].as_array().map_or(0, |a| a.len());
    if ["rules", "profiles", "recipes"].iter().any(|k| count(k) > MAX_ITEMS) {
        return Err(format!("方法包里的条目过多（每类最多 {MAX_ITEMS} 条）"));
    }
    let mut summary = json!({"rules":{"added":0,"skipped":0},"profiles":{"added":0,"skipped":0},"recipes":{"added":0,"skipped":0}});
    let mut bump = |kind: &str, field: &str| {
        summary[kind][field] = json!(summary[kind][field].as_u64().unwrap_or(0) + 1);
    };
    let mut known: BTreeSet<String> = canon::list_rules(db, None).await?.iter().map(|r| normalized(r["text"].as_str().unwrap_or(""))).collect();
    for rule in pack["rules"].as_array().into_iter().flatten() {
        let text = rule["text"].as_str().unwrap_or("");
        if text.trim().is_empty() || !known.insert(normalized(text)) {
            bump("rules", "skipped");
            continue;
        }
        match canon::save_rule(db, "author", None, rule["category"].as_str().unwrap_or(""), text, "方法包导入").await {
            Ok(_) => bump("rules", "added"),
            Err(_) => bump("rules", "skipped"),
        }
    }
    let mut ids: BTreeSet<String> = canon::platform_profiles(db).await?.iter().filter_map(|p| p["id"].as_str().map(str::to_owned)).collect();
    for profile in pack["profiles"].as_array().into_iter().flatten() {
        let mut profile = profile.clone();
        let base = profile["id"].as_str().unwrap_or("imported").to_owned();
        if ids.contains(&base) {
            // 同编号的画像已存在（内置或你自建的）：另起编号，不覆盖
            let mut n = 1;
            while ids.contains(&format!("{base}-imported{}", if n == 1 { String::new() } else { n.to_string() })) {
                n += 1;
            }
            profile["id"] = json!(format!("{base}-imported{}", if n == 1 { String::new() } else { n.to_string() }));
            profile["name"] = json!(format!("{}（导入）", profile["name"].as_str().unwrap_or(&base)));
        }
        match canon::save_platform_profile(db, &profile).await {
            Ok(saved) => {
                ids.insert(str_field(&saved, "id")?.to_owned());
                bump("profiles", "added");
            }
            Err(_) => bump("profiles", "skipped"),
        }
    }
    let mut seen: BTreeSet<String> = all_recipes(db).await?.iter().map(|r| normalized(&format!("{}{}", r["title"], r["steps"]))).collect();
    for recipe in pack["recipes"].as_array().into_iter().flatten() {
        let title = recipe["title"].as_str().unwrap_or("").trim();
        let steps_ok = recipe["steps"].as_array().is_some_and(|steps| !steps.is_empty() && steps.iter().all(|st| st["position"].is_string()));
        if title.is_empty() || !steps_ok || !seen.insert(normalized(&format!("\"{title}\"{}", recipe["steps"]))) {
            bump("recipes", "skipped");
            continue;
        }
        let key = format!("recipe:{}:{}", crate::agent_breakdown::book_id(TEMPLATE_PROJECT), id());
        put(db, canon::AUTHOR_NS, &key, &json!({"id":key,"projectId":TEMPLATE_PROJECT,"projectTitle":recipe["sourceTitle"],"title":title,
            "steps":recipe["steps"],"source":"方法包导入","createdAt":now()})).await?;
        bump("recipes", "added");
    }
    Ok(summary)
}
