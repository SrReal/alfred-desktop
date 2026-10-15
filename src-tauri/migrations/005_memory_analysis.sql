ALTER TABLE memories ADD COLUMN metadata_json TEXT NOT NULL DEFAULT '{}';
CREATE TABLE memory_candidates(id TEXT PRIMARY KEY,conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,project_id TEXT,normalized_key TEXT NOT NULL,body_json TEXT NOT NULL,state TEXT NOT NULL DEFAULT 'pending' CHECK(state IN ('pending','accepted','rejected')),memory_id TEXT REFERENCES memories(id) ON DELETE SET NULL,created_at TEXT NOT NULL,updated_at TEXT NOT NULL);
CREATE INDEX memory_candidates_scope ON memory_candidates(normalized_key,project_id,state);
CREATE TABLE conversation_analysis(conversation_id TEXT PRIMARY KEY REFERENCES conversations(id) ON DELETE CASCADE,last_message_rowid INTEGER NOT NULL DEFAULT 0,summary_json TEXT NOT NULL DEFAULT '{}',status TEXT NOT NULL DEFAULT 'idle',error_code TEXT,updated_at TEXT NOT NULL);
PRAGMA user_version=5;
