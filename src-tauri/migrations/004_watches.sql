CREATE TABLE watches(id TEXT PRIMARY KEY,owner_module TEXT NOT NULL,kind TEXT NOT NULL,config_json TEXT NOT NULL,enabled INTEGER NOT NULL DEFAULT 1,last_json TEXT,status TEXT NOT NULL DEFAULT 'pending',error_code TEXT,revision INTEGER NOT NULL DEFAULT 0,created_at TEXT NOT NULL,updated_at TEXT NOT NULL);
CREATE TABLE file_plans(id TEXT PRIMARY KEY,root_path TEXT NOT NULL,plan_json TEXT NOT NULL,state TEXT NOT NULL DEFAULT 'preview',created_at TEXT NOT NULL,updated_at TEXT NOT NULL);
PRAGMA user_version=4;
