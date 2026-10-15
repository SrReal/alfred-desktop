CREATE TABLE chat_progress (
    id TEXT PRIMARY KEY,
    conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    message_id TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    data_json TEXT NOT NULL
);
CREATE INDEX chat_progress_conversation ON chat_progress(conversation_id);
PRAGMA user_version=13;
