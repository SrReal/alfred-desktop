CREATE TABLE questions(id TEXT PRIMARY KEY,question TEXT NOT NULL,options_json TEXT NOT NULL,status TEXT NOT NULL,answer TEXT,created_at TEXT NOT NULL);
PRAGMA user_version=2;
