CREATE TABLE IF NOT EXISTS tasks (
  id TEXT PRIMARY KEY,
  parent_id TEXT REFERENCES tasks(id),
  name TEXT NOT NULL,
  working_dir TEXT NOT NULL,
  backend TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'idle',
  -- Claude CLI's --permission-mode for this task's agent calls. 'plan' (the
  -- safe default) never touches the working directory; 'bypassPermissions'
  -- lets the agent actually edit files and run commands. Headless `claude -p`
  -- has no TTY to answer interactive prompts, so the only two modes that work
  -- non-interactively are these two.
  permission_mode TEXT NOT NULL DEFAULT 'plan',
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS messages (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  task_id TEXT NOT NULL REFERENCES tasks(id),
  role TEXT NOT NULL,
  content TEXT NOT NULL,
  created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS runs (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  task_id TEXT NOT NULL REFERENCES tasks(id),
  message_id INTEGER REFERENCES messages(id),
  command TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'pending',
  exit_code INTEGER,
  error TEXT,
  started_at TEXT NOT NULL,
  finished_at TEXT,
  -- The tmux session name supervising this run (always `mozart-<task_id>`
  -- today, stored explicitly rather than reconstructed everywhere in case
  -- the naming scheme ever changes). NULL for runs from before Step 2.
  tmux_session TEXT
);

-- `runs.status` / `tasks.status` also accept 'cancelled' as of Step 2,
-- alongside the existing pending/running/succeeded/failed/idle/error
-- strings — still no CHECK constraint, same loose-string convention as
-- Step 1.

-- `CREATE TABLE IF NOT EXISTS` above won't add this column to a database
-- that already exists on disk from before Step 2 — see the migration in
-- db.rs::open(), which ALTERs it in for existing ~/.mozart/mozart.db files.
