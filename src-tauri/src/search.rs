use crate::{runtime::Core, semantic, semantic_store, storage};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
pub async fn search(c: &Arc<Core>, query: &str) -> Result<Value, String> {
    if query.trim().is_empty() {
        return Ok(json!([]));
    }
    if query.chars().count() > 4096 {
        return Err("SEARCH_QUERY_LIMIT".into());
    }
    let fts = storage::fts_query(query);
    let mut matches = Vec::new();
    if !fts.is_empty() {
        for sql in [
            "SELECT id,'conversation' AS kind,title,snippet(conversations_fts,1,'[',']','…',20) AS snippet FROM conversations_fts WHERE conversations_fts MATCH ? ORDER BY bm25(conversations_fts) LIMIT 50",
            "SELECT m.conversation_id AS id,'conversation' AS kind,c.title,snippet(messages_fts,2,'[',']','…',20) AS snippet FROM messages_fts f JOIN messages m ON f.id=m.id JOIN conversations c ON c.id=m.conversation_id WHERE messages_fts MATCH ? ORDER BY bm25(messages_fts) LIMIT 50",
            "SELECT id,'project' AS kind,name AS title,snippet(projects_fts,2,'[',']','…',20) AS snippet FROM projects_fts WHERE projects_fts MATCH ? ORDER BY bm25(projects_fts) LIMIT 20",
        ] {
            let lexical=c.store.query(sql,&[json!(fts)])?;
            matches.extend(lexical.as_array().ok_or("SEARCH_RESULTS")?.iter().cloned());
        }
    }
    for sql in [
        "SELECT id,'task' AS kind,title,status AS snippet FROM tasks WHERE instr(lower(title),lower(?))>0 LIMIT 25",
        "SELECT id,'macro' AS kind,name AS title,'Macro' AS snippet FROM macros WHERE instr(lower(name),lower(?))>0 LIMIT 25",
    ] {
        let other=c.store.query(sql,&[json!(query)])?;
        matches.extend(other.as_array().ok_or("SEARCH_RESULTS")?.iter().cloned());
    }
    if !semantic_store::blocked(&c.store) {
        let memories = semantic::retrieve_with(
            c,
            query,
            None,
            &tokio_util::sync::CancellationToken::new(),
            true,
        )
        .await?;
        let mut hybrid = Vec::new();
        for row in memories.as_array().ok_or("SEARCH_RESULTS")? {
            hybrid.push(json!({"id":row["id"],"kind":if row["kind"]=="summary" {"conversation"}else{"memory"},"title":row["title"],"snippet":row["content"].as_str().unwrap_or("").chars().take(320).collect::<String>(),"semantic_score":row["semantic_score"],"sources":row["sources"]}));
        }
        matches.splice(0..0, hybrid);
    }
    let mut seen = HashSet::new();
    matches.retain(|r| {
        seen.insert(format!(
            "{}:{}",
            r["kind"].as_str().unwrap_or(""),
            r["id"].as_str().unwrap_or("")
        ))
    });
    let mut selected = Vec::new();
    let mut deferred = Vec::new();
    let mut kinds = HashMap::new();
    for row in matches {
        let count = kinds
            .entry(row["kind"].as_str().unwrap_or("").to_string())
            .or_insert(0usize);
        if *count < 20 && selected.len() < 100 {
            *count += 1;
            selected.push(row);
        } else {
            deferred.push(row);
        }
    }
    selected.extend(deferred.into_iter().take(100 - selected.len()));
    Ok(json!(selected))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn global_search_merges_origins_projects_tasks_and_macros() {
        let root = std::env::temp_dir().join(format!("ap-search-{}", storage::id()));
        let c = Arc::new(
            Core::new(
                root.clone(),
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
            )
            .unwrap(),
        );
        c.store.execute("INSERT INTO projects(id,name,root_path,tags_json,created_at) VALUES('p','MySQL',?,'[]',?)",&[json!(root),json!(storage::now())]).unwrap();
        c.store.execute("INSERT INTO tasks(id,title,status,created_at,updated_at) VALUES('t','Revisar MySQL','pending',?,?)",&[json!(storage::now()),json!(storage::now())]).unwrap();
        c.store
            .execute(
                "INSERT INTO macros(id,name,definition_json) VALUES('m','Copia MySQL','{}')",
                &[],
            )
            .unwrap();
        c.store
            .remember(&json!({"content":"MySQL usa copia diaria","project_id":"other"}))
            .unwrap();
        c.store.0.lock().unwrap().execute_batch("WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<110) INSERT INTO conversations(id,title,created_at,updated_at) SELECT 'crowded-'||x,'MySQL','2026-10-01T00:00:00Z','2026-10-01T00:00:00Z' FROM n;").unwrap();
        let result = search(&c, "MySQL").await.unwrap();
        for kind in ["project", "task", "macro", "memory"] {
            assert!(result.as_array().unwrap().iter().any(|r| r["kind"] == kind));
        }
        c.store.set_setting("privacy", &json!(true)).unwrap();
        assert!(!search(&c, "MySQL")
            .await
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["kind"] == "memory"));
    }
}
