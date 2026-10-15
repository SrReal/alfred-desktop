CREATE TABLE IF NOT EXISTS asset_records(id TEXT PRIMARY KEY,class TEXT NOT NULL,state TEXT NOT NULL,record_json TEXT NOT NULL,created_at TEXT NOT NULL,expires_at TEXT,error TEXT);
CREATE INDEX IF NOT EXISTS asset_expiry ON asset_records(state,expires_at);
PRAGMA user_version=9;
