CREATE TABLE drive_jobs (
 id TEXT PRIMARY KEY NOT NULL,
 connection_id TEXT NOT NULL,
 kind TEXT NOT NULL CHECK(kind IN ('download','upload')),
 state TEXT NOT NULL,
 revision INTEGER NOT NULL DEFAULT 0,
 record_json TEXT NOT NULL,
 created_at TEXT NOT NULL,
 updated_at TEXT NOT NULL
);
CREATE INDEX drive_jobs_connection ON drive_jobs(connection_id,kind,created_at,id);
PRAGMA user_version = 12;
