CREATE TABLE semantic_embeddings(
 entity_kind TEXT NOT NULL CHECK(entity_kind IN ('memory','summary')),
 entity_id TEXT NOT NULL,
 index_revision TEXT NOT NULL,
 text_sha256 TEXT NOT NULL,
 vector_json TEXT NOT NULL,
 updated_at TEXT NOT NULL,
 PRIMARY KEY(entity_kind,entity_id)
);
CREATE TRIGGER semantic_memory_update AFTER UPDATE ON memories
 WHEN old.title IS NOT new.title OR old.content IS NOT new.content OR old.type IS NOT new.type OR old.project_id IS NOT new.project_id OR old.disabled IS NOT new.disabled OR old.superseded_by IS NOT new.superseded_by
 BEGIN DELETE FROM semantic_embeddings WHERE entity_kind='memory' AND entity_id=new.id; END;
CREATE TRIGGER semantic_memory_delete AFTER DELETE ON memories
 BEGIN DELETE FROM semantic_embeddings WHERE entity_kind='memory' AND entity_id=old.id; END;
CREATE TRIGGER semantic_summary_update AFTER UPDATE ON conversations
 WHEN old.title IS NOT new.title OR old.summary IS NOT new.summary OR old.project_id IS NOT new.project_id OR old.no_memory IS NOT new.no_memory
 BEGIN DELETE FROM semantic_embeddings WHERE entity_kind='summary' AND entity_id=new.id; END;
CREATE TRIGGER semantic_summary_delete AFTER DELETE ON conversations
 BEGIN DELETE FROM semantic_embeddings WHERE entity_kind='summary' AND entity_id=old.id; END;
CREATE TRIGGER projects_au AFTER UPDATE OF name,tags_json ON projects
 BEGIN DELETE FROM projects_fts WHERE id=old.id;INSERT INTO projects_fts VALUES(new.id,new.name,new.tags_json);END;
PRAGMA user_version=6;
