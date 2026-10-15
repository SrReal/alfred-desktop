ALTER TABLE undo_records ADD COLUMN metadata_json TEXT NOT NULL DEFAULT '{}';
CREATE TABLE file_cleanup_plans (
 id TEXT PRIMARY KEY,
 snapshot_json TEXT NOT NULL,
 state TEXT NOT NULL DEFAULT 'prepared',
 next_index INTEGER NOT NULL DEFAULT 0,
 error TEXT,
 created_at TEXT NOT NULL,
 updated_at TEXT NOT NULL
);
PRAGMA user_version=8;
