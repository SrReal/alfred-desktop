CREATE TABLE clipboard_state(id INTEGER PRIMARY KEY CHECK(id=1), generation INTEGER NOT NULL DEFAULT 0);
INSERT INTO clipboard_state(id) VALUES(1);
CREATE TABLE clipboard_history(
 id TEXT PRIMARY KEY, sha256 TEXT NOT NULL UNIQUE, text TEXT NOT NULL,
 sequence INTEGER NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL, expires_at TEXT NOT NULL
);
CREATE INDEX clipboard_history_order ON clipboard_history(updated_at DESC,id);
CREATE TRIGGER clipboard_setting_insert AFTER INSERT ON settings WHEN NEW.key IN ('clipboard_history','clipboard_history_consent','privacy')
 BEGIN UPDATE clipboard_state SET generation=generation+1 WHERE id=1; END;
CREATE TRIGGER clipboard_setting_update AFTER UPDATE ON settings WHEN NEW.key IN ('clipboard_history','clipboard_history_consent','privacy')
 BEGIN UPDATE clipboard_state SET generation=generation+1 WHERE id=1; END;
CREATE TRIGGER clipboard_setting_delete AFTER DELETE ON settings WHEN OLD.key IN ('clipboard_history','clipboard_history_consent','privacy')
 BEGIN UPDATE clipboard_state SET generation=generation+1 WHERE id=1; END;
CREATE TRIGGER clipboard_module_update AFTER UPDATE OF enabled ON module_configs WHEN NEW.module_id='clipboard'
 BEGIN UPDATE clipboard_state SET generation=generation+1 WHERE id=1; END;
PRAGMA user_version=11;
