use rusqlite::Row;
use serde::Serialize;

// Task.status — driven entirely by commands::set_task_status, called from
// send_message in commands.rs:
//
//   ┌──────┐  send_message starts   ┌─────────┐
//   │ idle │ ─────────────────────► │ running │
//   └──┬───┘                        └────┬────┘
//      ▲                                 │
//      │         agent call Ok           │ agent call Err
//      └─────────────────────────────────┤
//                                        ▼
//                                   ┌───────┐
//                                   │ error │
//                                   └───────┘
//
// `error` has no outgoing transition shown above — the next send_message call
// on that task moves it straight back to `running`, same as from `idle`.
#[derive(Serialize, Clone)]
pub struct Task {
    pub id: String,
    pub parent_id: Option<String>,
    pub name: String,
    pub working_dir: String,
    pub backend: String,
    pub status: String,
    pub permission_mode: String,
    pub created_at: String,
    pub updated_at: String,
}

impl Task {
    pub fn from_row(row: &Row) -> rusqlite::Result<Self> {
        Ok(Task {
            id: row.get("id")?,
            parent_id: row.get("parent_id")?,
            name: row.get("name")?,
            working_dir: row.get("working_dir")?,
            backend: row.get("backend")?,
            status: row.get("status")?,
            permission_mode: row.get("permission_mode")?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }
}

#[derive(Serialize, Clone, Debug)]
pub struct Message {
    pub id: i64,
    pub task_id: String,
    pub role: String,
    pub content: String,
    pub created_at: String,
}

impl Message {
    pub fn from_row(row: &Row) -> rusqlite::Result<Self> {
        Ok(Message {
            id: row.get("id")?,
            task_id: row.get("task_id")?,
            role: row.get("role")?,
            content: row.get("content")?,
            created_at: row.get("created_at")?,
        })
    }
}

// Run.status — one row per send_message call, written in commands.rs:
//
//   ┌─────────┐  insert   ┌─────────┐
//   │ pending │ ────────► │ running │
//   └─────────┘           └────┬────┘
//                               │
//             ┌─────────────┬──┴──────────────┐
//             │ agent Ok    │ agent Err        │ cancel_run (Step 2)
//             ▼             ▼                  ▼
//      ┌───────────┐  ┌────────┐        ┌───────────┐
//      │ succeeded │  │ failed │        │ cancelled │
//      └───────────┘  └────────┘        └───────────┘
//
// `pending` is the column DEFAULT; in practice every row is inserted directly
// as `running` (commands.rs never inserts a run before starting the agent
// call), so `pending` is currently unreachable in this codebase.
//
// Not yet queried outside tests: Step 1's UI inspects `runs` via `sqlite3`
// directly (see CLAUDE.md). A `list_runs` command lands in a later step.
#[allow(dead_code)]
#[derive(Serialize, Clone)]
pub struct Run {
    pub id: i64,
    pub task_id: String,
    pub message_id: Option<i64>,
    pub command: String,
    pub status: String,
    pub exit_code: Option<i32>,
    pub error: Option<String>,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub tmux_session: Option<String>,
}

#[allow(dead_code)]
impl Run {
    pub fn from_row(row: &Row) -> rusqlite::Result<Self> {
        Ok(Run {
            id: row.get("id")?,
            task_id: row.get("task_id")?,
            message_id: row.get("message_id")?,
            command: row.get("command")?,
            status: row.get("status")?,
            exit_code: row.get("exit_code")?,
            error: row.get("error")?,
            started_at: row.get("started_at")?,
            finished_at: row.get("finished_at")?,
            tmux_session: row.get("tmux_session")?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::{params, Connection};

    fn test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!("schema.sql")).unwrap();
        conn
    }

    #[test]
    fn task_tree_via_parent_id() {
        let conn = test_db();

        conn.execute(
            "INSERT INTO tasks (id, parent_id, name, working_dir, backend, status, created_at, updated_at)
             VALUES ('root', NULL, 'root task', '/tmp', 'claude-cli', 'idle', 't0', 't0')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tasks (id, parent_id, name, working_dir, backend, status, created_at, updated_at)
             VALUES ('child', 'root', 'spawned sub-task', '/tmp', 'claude-cli', 'idle', 't1', 't1')",
            [],
        )
        .unwrap();

        let root_only: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM tasks WHERE parent_id IS NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(root_only, 1, "only the root task should have no parent");

        let child = conn
            .query_row("SELECT * FROM tasks WHERE id = 'child'", [], Task::from_row)
            .unwrap();
        assert_eq!(child.parent_id, Some("root".to_string()));
    }

    #[test]
    fn message_and_run_roundtrip() {
        let conn = test_db();
        conn.execute(
            "INSERT INTO tasks (id, parent_id, name, working_dir, backend, status, created_at, updated_at)
             VALUES ('t', NULL, 'task', '/tmp', 'claude-cli', 'idle', 't0', 't0')",
            [],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO messages (task_id, role, content, created_at) VALUES ('t', 'user', 'hi', 't0')",
            [],
        )
        .unwrap();
        let message_id = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO runs (task_id, message_id, command, status, started_at) VALUES (?1, ?2, 'claude -p ...', 'running', 't0')",
            params!["t", message_id],
        )
        .unwrap();

        let run = conn
            .query_row("SELECT * FROM runs WHERE task_id = 't'", [], Run::from_row)
            .unwrap();
        assert_eq!(run.message_id, Some(message_id));
        assert_eq!(run.status, "running");

        let message = conn
            .query_row(
                "SELECT * FROM messages WHERE id = ?1",
                params![message_id],
                Message::from_row,
            )
            .unwrap();
        assert_eq!(message.content, "hi");
    }
}
