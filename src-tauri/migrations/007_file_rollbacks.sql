ALTER TABLE undo_records ADD COLUMN staging TEXT;
ALTER TABLE undo_records ADD COLUMN object_id TEXT;
CREATE TABLE file_rollbacks (
 id TEXT PRIMARY KEY,
 tokens_json TEXT NOT NULL,
 next_index INTEGER NOT NULL DEFAULT 0,
 state TEXT NOT NULL DEFAULT 'prepared',
 error TEXT,
 created_at TEXT NOT NULL,
 updated_at TEXT NOT NULL
);
PRAGMA user_version=7;
