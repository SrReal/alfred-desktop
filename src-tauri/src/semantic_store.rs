use crate::{
    policy::contains_secret,
    storage::{self, Store},
};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};

pub const INDEX_REVISION: &str =
    "e8f8c211226b894fcb81acc59f3b34ba3efd5f42-tokenizers0.2-ort1.30-mean-l2-128-v1";
pub const DIMENSIONS: usize = 384;
pub const DOCUMENT_LIMIT: usize = 10000;
pub const DOCUMENT_SQL: &str = "SELECT id,'memory' AS kind,type,title,content,project_id,importance,confidence,pinned,updated_at,use_count FROM memories WHERE disabled=0 AND superseded_by IS NULL UNION ALL SELECT id,'summary','conversation_summary',title,summary,project_id,0.5,1.0,0,updated_at,0 FROM conversations WHERE no_memory=0 AND length(trim(summary))>0";

#[derive(Clone)]
pub struct Document {
    pub row: Value,
    pub text: String,
    pub hash: String,
}
impl Document {
    pub fn from_row(row: Value) -> Option<Self> {
        let title = row["title"].as_str()?;
        let content = row["content"].as_str()?;
        if content.trim().is_empty() || contains_secret(title) || contains_secret(content) {
            return None;
        }
        let source = json!([
            row["kind"],
            row["id"],
            row["type"],
            title,
            content,
            row["project_id"]
        ]);
        let hash = format!("{:x}", Sha256::digest(source.to_string().as_bytes()));
        let text = format!("{title}\n{content}").chars().take(4096).collect();
        Some(Self { row, text, hash })
    }
    pub fn key(&self) -> String {
        format!(
            "{}:{}",
            self.row["kind"].as_str().unwrap_or(""),
            self.row["id"].as_str().unwrap_or("")
        )
    }
}
pub fn blocked(store: &Store) -> bool {
    store.setting("privacy") == true || store.setting("no_memory") == true
}
pub fn documents(store: &Store) -> Result<Vec<Document>, String> {
    if blocked(store) {
        return Err("SEMANTIC_PRIVACY_OR_NO_MEMORY".into());
    }
    let rows = store.query(&format!("{DOCUMENT_SQL} LIMIT {}", DOCUMENT_LIMIT + 1), &[])?;
    let rows = rows.as_array().ok_or("SEMANTIC_DOCUMENTS_INVALID")?;
    if rows.len() > DOCUMENT_LIMIT {
        return Err("SEMANTIC_DOCUMENT_LIMIT".into());
    }
    Ok(rows
        .iter()
        .cloned()
        .filter_map(Document::from_row)
        .collect())
}
pub fn valid_vector(vector: &[f32]) -> bool {
    vector.len() == DIMENSIONS
        && vector.iter().all(|n| n.is_finite())
        && (vector.iter().map(|n| n * n).sum::<f32>() - 1.0).abs() < 0.01
}
pub fn pending(store: &Store) -> Result<Vec<Document>, String> {
    let cache = store.query(
        "SELECT entity_kind,entity_id,text_sha256,index_revision,vector_json FROM semantic_embeddings",
        &[],
    )?;
    let hashes: HashMap<String, String> = cache
        .as_array()
        .ok_or("SEMANTIC_CACHE_INVALID")?
        .iter()
        .filter(|r| r["index_revision"] == INDEX_REVISION)
        .filter(|r| {
            r["vector_json"]
                .as_str()
                .and_then(|s| serde_json::from_str::<Vec<f32>>(s).ok())
                .is_some_and(|v| valid_vector(&v))
        })
        .map(|r| {
            (
                format!(
                    "{}:{}",
                    r["entity_kind"].as_str().unwrap_or(""),
                    r["entity_id"].as_str().unwrap_or("")
                ),
                r["text_sha256"].as_str().unwrap_or("").into(),
            )
        })
        .collect();
    Ok(documents(store)?
        .into_iter()
        .filter(|d| hashes.get(&d.key()) != Some(&d.hash))
        .collect())
}
// Source identity and privacy are checked inside the same transaction as the
// write. A deleted, edited, disabled or moved source never receives a stale vector.
pub fn persist(store: &Store, document: &Document, vector: &[f32]) -> Result<bool, String> {
    if !valid_vector(vector) {
        return Err("SEMANTIC_VECTOR_INVALID".into());
    }
    let mut db = store.0.lock().map_err(|_| "DB_LOCK")?;
    let tx = db.transaction().map_err(|_| "SEMANTIC_TRANSACTION")?;
    let denied: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM settings WHERE key IN ('privacy','no_memory') AND json_value='true') OR NOT EXISTS(SELECT 1 FROM settings WHERE key='semantic_memory' AND json_value='true')", [], |r| r.get(0)).map_err(|_| "SEMANTIC_SETTINGS")?;
    if denied {
        return Ok(false);
    }
    let row: Option<Value> = tx.query_row(&format!("SELECT * FROM ({DOCUMENT_SQL}) WHERE kind=? AND id=?"), params![document.row["kind"].as_str(), document.row["id"].as_str()], |r| Ok(json!({
        "id":r.get::<_,String>(0)?,"kind":r.get::<_,String>(1)?,"type":r.get::<_,String>(2)?,"title":r.get::<_,String>(3)?,"content":r.get::<_,String>(4)?,"project_id":r.get::<_,Option<String>>(5)?
    }))).optional().map_err(|_| "SEMANTIC_SOURCE_CHECK")?;
    if row
        .and_then(Document::from_row)
        .is_none_or(|d| d.hash != document.hash)
    {
        return Ok(false);
    }
    tx.execute("INSERT INTO semantic_embeddings VALUES(?,?,?,?,?,?) ON CONFLICT(entity_kind,entity_id) DO UPDATE SET index_revision=excluded.index_revision,text_sha256=excluded.text_sha256,vector_json=excluded.vector_json,updated_at=excluded.updated_at", params![document.row["kind"].as_str(),document.row["id"].as_str(), INDEX_REVISION, document.hash, serde_json::to_string(vector).map_err(|_| "SEMANTIC_VECTOR_JSON")?,storage::now()]).map_err(|_| "SEMANTIC_CACHE_WRITE")?;
    tx.commit().map_err(|_| "SEMANTIC_COMMIT")?;
    Ok(true)
}
pub fn retrieve(
    store: &Store,
    query: &str,
    project: Option<&str>,
    vector: Option<&[f32]>,
) -> Result<Value, String> {
    retrieve_scoped(store, query, project, vector, false)
}
pub fn retrieve_all(store: &Store, query: &str, vector: Option<&[f32]>) -> Result<Value, String> {
    retrieve_scoped(store, query, None, vector, true)
}
fn retrieve_scoped(
    store: &Store,
    query: &str,
    project: Option<&str>,
    vector: Option<&[f32]>,
    all_scopes: bool,
) -> Result<Value, String> {
    if blocked(store) {
        return Ok(json!([]));
    }
    if vector.is_some_and(|v| !valid_vector(v)) {
        return Err("SEMANTIC_VECTOR_INVALID".into());
    }
    let fts = storage::fts_query(query);
    let mut lexical = HashMap::new();
    if !fts.is_empty() {
        for (kind, sql) in [
            ("memory", "SELECT m.id FROM memories m JOIN memories_fts f ON m.id=f.id WHERE memories_fts MATCH ? AND m.disabled=0 AND m.superseded_by IS NULL AND (m.project_id IS NULL OR m.project_id=? OR ?) ORDER BY bm25(memories_fts),m.id LIMIT 64"),
            ("summary", "SELECT c.id FROM conversations c JOIN conversations_fts f ON c.id=f.id WHERE conversations_fts MATCH ? AND c.no_memory=0 AND length(trim(c.summary))>0 AND (c.project_id IS NULL OR c.project_id=? OR ?) ORDER BY bm25(conversations_fts),c.id LIMIT 32")
        ] {
            let rows = store.query(sql, &[json!(fts), json!(project),json!(all_scopes)])?;
            for (rank, row) in rows.as_array().ok_or("SEMANTIC_FTS")?.iter().enumerate() {
                lexical.insert(format!("{kind}:{}", row["id"].as_str().unwrap_or("")), 1.0/(1.0+rank as f64/8.0));
            }
        }
    }
    let cache = if vector.is_some() {
        store.query(
            "SELECT * FROM semantic_embeddings WHERE index_revision=? LIMIT 10000",
            &[json!(INDEX_REVISION)],
        )?
    } else {
        json!([])
    };
    let cache: HashMap<String, &Value> = cache
        .as_array()
        .ok_or("SEMANTIC_CACHE")?
        .iter()
        .map(|r| {
            (
                format!(
                    "{}:{}",
                    r["entity_kind"].as_str().unwrap_or(""),
                    r["entity_id"].as_str().unwrap_or("")
                ),
                r,
            )
        })
        .collect();
    // Query only lexical candidates and cached semantic candidates; a large
    // unindexed archive still has complete lexical search without a full scan.
    let keys = lexical.keys().chain(cache.keys()).collect::<HashSet<_>>();
    let mut memory_ids = Vec::new();
    let mut summary_ids = Vec::new();
    for key in keys {
        if let Some((kind, id)) = key.split_once(':') {
            if kind == "memory" {
                memory_ids.push(id);
            } else if kind == "summary" {
                summary_ids.push(id);
            }
        }
    }
    let rows=store.query(&format!("SELECT * FROM ({DOCUMENT_SQL}) WHERE (project_id IS NULL OR project_id=? OR ?) AND ((kind='memory' AND id IN (SELECT value FROM json_each(?))) OR (kind='summary' AND id IN (SELECT value FROM json_each(?))))"),&[json!(project),json!(all_scopes),json!(json!(memory_ids).to_string()),json!(json!(summary_ids).to_string())])?;
    let candidates = rows
        .as_array()
        .ok_or("SEMANTIC_DOCUMENTS_INVALID")?
        .iter()
        .cloned()
        .filter_map(Document::from_row);
    let mut ranked = Vec::new();
    for document in candidates {
        if !all_scopes
            && !document.row["project_id"].is_null()
            && document.row["project_id"].as_str() != project
        {
            continue;
        }
        let key = document.key();
        let fts_score = lexical.get(&key).copied().unwrap_or(0.0);
        let similarity = cache
            .get(&key)
            .filter(|r| r["text_sha256"] == document.hash)
            .and_then(|r| serde_json::from_str::<Vec<f32>>(r["vector_json"].as_str()?).ok())
            .filter(|v| valid_vector(v))
            .zip(vector)
            .map(|(v, q)| v.iter().zip(q).map(|(a, b)| f64::from(a * b)).sum::<f64>())
            .unwrap_or(0.0);
        if fts_score == 0.0 && similarity < 0.30 {
            continue;
        }
        let mut row = document.row;
        let age = chrono::DateTime::parse_from_rfc3339(row["updated_at"].as_str().unwrap_or(""))
            .map(|at| {
                (chrono::Utc::now() - at.with_timezone(&chrono::Utc))
                    .num_days()
                    .max(0) as f64
            })
            .unwrap_or(365.0);
        let scope_bonus = if project.is_some() && row["project_id"].as_str() == project {
            0.1
        } else {
            0.0
        };
        let score = (if vector.is_some() { 0.30 } else { 0.5 }) * fts_score
            + 0.4 * similarity.max(0.0)
            + 0.2 * row["importance"].as_f64().unwrap_or(0.5)
            + 0.1 / (1.0 + age / 30.0)
            + scope_bonus
            + 0.05 * row["confidence"].as_f64().unwrap_or(0.0)
            + if row["pinned"] == 1 { 0.05 } else { 0.0 };
        row["ranking_score"] = json!(score);
        row["semantic_score"] = json!(similarity);
        row["fts_score"] = json!(fts_score);
        row["source_hash"] = json!(document.hash);
        ranked.push(row);
    }
    ranked.sort_by(|a, b| {
        b["ranking_score"]
            .as_f64()
            .unwrap_or(0.0)
            .total_cmp(&a["ranking_score"].as_f64().unwrap_or(0.0))
            .then(a["id"].as_str().cmp(&b["id"].as_str()))
    });
    let mut chosen = Vec::new();
    let mut deferred = Vec::new();
    let mut types = HashMap::new();
    for row in ranked {
        let count = types
            .entry(row["type"].as_str().unwrap_or("fact").to_string())
            .or_insert(0usize);
        if *count < 2 && chosen.len() < 6 {
            *count += 1;
            chosen.push(row);
        } else {
            deferred.push(row);
        }
    }
    chosen.extend(deferred.into_iter().take(6 - chosen.len()));
    if blocked(store) {
        return Ok(json!([]));
    }
    finalize(store, chosen, project, all_scopes)
}

fn finalize(
    store: &Store,
    chosen: Vec<Value>,
    project: Option<&str>,
    all_scopes: bool,
) -> Result<Value, String> {
    let mut db = store.0.lock().map_err(|_| "DB_LOCK")?;
    let tx = db.transaction().map_err(|_| "SEMANTIC_TRANSACTION")?;
    let denied:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM settings WHERE key IN ('privacy','no_memory') AND json_value='true')",[],|r|r.get(0)).map_err(|_|"SEMANTIC_SETTINGS")?;
    if denied {
        return Ok(json!([]));
    }
    let enabled:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM settings WHERE key='semantic_memory' AND json_value='true')",[],|r|r.get(0)).map_err(|_|"SEMANTIC_SETTINGS")?;
    let mut result = Vec::new();
    for mut row in chosen {
        if !enabled && row["fts_score"].as_f64().unwrap_or(0.0) == 0.0 {
            continue;
        }
        let current:Option<Value>=tx.query_row(&format!("SELECT * FROM ({DOCUMENT_SQL}) WHERE kind=? AND id=? AND (project_id IS NULL OR project_id=? OR ?)"),params![row["kind"].as_str(),row["id"].as_str(),project,all_scopes],|r|Ok(json!({"id":r.get::<_,String>(0)?,"kind":r.get::<_,String>(1)?,"type":r.get::<_,String>(2)?,"title":r.get::<_,String>(3)?,"content":r.get::<_,String>(4)?,"project_id":r.get::<_,Option<String>>(5)?}))).optional().map_err(|_|"SEMANTIC_SOURCE_CHECK")?;
        if current
            .and_then(Document::from_row)
            .is_none_or(|d| row["source_hash"] != d.hash)
        {
            continue;
        }
        if row["kind"] == "memory" {
            tx.execute(
                "UPDATE memories SET use_count=use_count+1,last_used_at=? WHERE id=?",
                params![storage::now(), row["id"].as_str()],
            )
            .map_err(|_| "SEMANTIC_USE_COUNT")?;
            row["use_count"] = json!(row["use_count"].as_u64().unwrap_or(0) + 1);
            let mut stmt = tx
                .prepare("SELECT conversation_id,message_id FROM memory_sources WHERE memory_id=?")
                .map_err(|_| "SEMANTIC_SOURCES")?;
            let sources=stmt.query_map(params![row["id"].as_str()],|r|Ok(json!({"conversation_id":r.get::<_,Option<String>>(0)?,"message_id":r.get::<_,Option<String>>(1)?}))).map_err(|_|"SEMANTIC_SOURCES")?.collect::<Result<Vec<_>,_>>().map_err(|_|"SEMANTIC_SOURCES")?;
            row["sources"] = json!(sources);
        } else {
            row["sources"] = json!([{"conversation_id":row["id"]}]);
        }
        if let Some(object) = row.as_object_mut() {
            object.remove("source_hash");
        }
        result.push(row);
    }
    tx.commit().map_err(|_| "SEMANTIC_COMMIT")?;
    Ok(json!(result))
}

pub fn purge_orphans(store: &Store) -> Result<(), String> {
    let valid: HashSet<String> = documents(store)?.iter().map(Document::key).collect();
    let rows = store.query("SELECT entity_kind,entity_id FROM semantic_embeddings", &[])?;
    for row in rows.as_array().ok_or("SEMANTIC_CACHE")? {
        if !valid.contains(&format!(
            "{}:{}",
            row["entity_kind"].as_str().unwrap_or(""),
            row["entity_id"].as_str().unwrap_or("")
        )) {
            store.execute(
                "DELETE FROM semantic_embeddings WHERE entity_kind=? AND entity_id=?",
                &[row["entity_kind"].clone(), row["entity_id"].clone()],
            )?;
        }
    }
    Ok(())
}
