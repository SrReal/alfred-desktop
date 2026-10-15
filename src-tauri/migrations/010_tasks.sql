CREATE INDEX IF NOT EXISTS tasks_due ON tasks(status,due_at);
CREATE INDEX IF NOT EXISTS task_runs_history ON task_runs(task_id,ended_at,id);
PRAGMA user_version=10;
