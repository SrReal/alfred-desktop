CREATE TABLE undo_records(
 id TEXT PRIMARY KEY,kind TEXT NOT NULL,path TEXT NOT NULL,destination TEXT,checkpoint TEXT,
 expected_hash TEXT NOT NULL,checkpoint_hash TEXT,state TEXT NOT NULL DEFAULT 'prepared',created_at TEXT NOT NULL,updated_at TEXT NOT NULL
);
PRAGMA user_version=3;
