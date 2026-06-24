use crate::backend::{self, AgentBackend, AgentReply, RunHandle, SendOutcome};
use crate::models::{Message, Task};
use chrono::Utc;
use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use tauri::{AppHandle, Manager, State};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub struct Db(pub Mutex<Connection>);

/// Tracks the cancellation handle for whichever run is currently in flight
/// for a given task, keyed by `task_id`. `send_message` inserts a token
/// before calling the backend and removes it once the call settles (success,
/// failure, or cancellation); `cancel_run` looks a token up to interrupt that
/// in-flight call early. Startup reconciliation re-populates this for any
/// run resumed after a restart, so Cancel still works on it.
pub struct RunRegistry(pub Mutex<HashMap<String, CancellationToken>>);

/// Tauri serializes command `Err`s straight to the frontend, so this just
/// needs to carry a message. `#[serde(transparent)]` keeps the wire format
/// an unwrapped string, matching what the frontend already expects.
#[derive(Debug, serde::Serialize)]
#[serde(transparent)]
pub struct CmdError(String);

impl From<rusqlite::Error> for CmdError {
    fn from(e: rusqlite::Error) -> Self {
        CmdError(e.to_string())
    }
}

impl<T> From<PoisonError<T>> for CmdError {
    fn from(e: PoisonError<T>) -> Self {
        CmdError(e.to_string())
    }
}

impl From<anyhow::Error> for CmdError {
    fn from(e: anyhow::Error) -> Self {
        CmdError(e.to_string())
    }
}

impl From<String> for CmdError {
    fn from(s: String) -> Self {
        CmdError(s)
    }
}

/// `current_dir()` does no shell-style expansion, so a literal "~/foo" would
/// fail to spawn the agent process. Expand it ourselves at creation time so
/// every other layer can assume `working_dir` is already an absolute path.
fn expand_working_dir(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest).to_string_lossy().into_owned();
        }
    } else if path == "~" {
        if let Some(home) = dirs::home_dir() {
            return home.to_string_lossy().into_owned();
        }
    }
    path.to_string()
}

fn set_task_status(conn: &Connection, task_id: &str, status: &str, now: &str) -> Result<(), CmdError> {
    conn.execute(
        "UPDATE tasks SET status = ?1, updated_at = ?2 WHERE id = ?3",
        params![status, now, task_id],
    )?;
    Ok(())
}

fn create_task_impl(
    conn: &Connection,
    name: &str,
    working_dir: &str,
    parent_id: Option<&str>,
    permission_mode: Option<&str>,
) -> Result<Task, CmdError> {
    let id = Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();
    let working_dir = expand_working_dir(working_dir);
    let permission_mode = permission_mode.unwrap_or("plan");

    conn.execute(
        "INSERT INTO tasks (id, parent_id, name, working_dir, backend, status, permission_mode, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, 'claude-cli', 'idle', ?5, ?6, ?6)",
        params![id, parent_id, name, working_dir, permission_mode, now],
    )?;

    Ok(conn.query_row("SELECT * FROM tasks WHERE id = ?1", params![id], Task::from_row)?)
}

#[tauri::command]
pub fn create_task(
    db: State<Db>,
    name: String,
    working_dir: String,
    parent_id: Option<String>,
    permission_mode: Option<String>,
) -> Result<Task, CmdError> {
    let conn = db.0.lock()?;
    create_task_impl(&conn, &name, &working_dir, parent_id.as_deref(), permission_mode.as_deref())
}

fn list_tasks_impl(conn: &Connection) -> Result<Vec<Task>, CmdError> {
    let mut stmt = conn.prepare("SELECT * FROM tasks WHERE parent_id IS NULL ORDER BY updated_at DESC")?;
    let tasks = stmt.query_map([], Task::from_row)?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(tasks)
}

#[tauri::command]
pub fn list_tasks(db: State<Db>) -> Result<Vec<Task>, CmdError> {
    let conn = db.0.lock()?;
    list_tasks_impl(&conn)
}

fn list_messages_impl(conn: &Connection, task_id: &str) -> Result<Vec<Message>, CmdError> {
    let mut stmt = conn.prepare("SELECT * FROM messages WHERE task_id = ?1 ORDER BY id")?;
    let messages = stmt
        .query_map(params![task_id], Message::from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(messages)
}

#[tauri::command]
pub fn list_messages(db: State<Db>, task_id: String) -> Result<Vec<Message>, CmdError> {
    let conn = db.0.lock()?;
    list_messages_impl(&conn, &task_id)
}

fn begin_send(conn: &Connection, task_id: &str, content: &str, now: &str) -> Result<(Task, bool, i64), CmdError> {
    conn.execute(
        "INSERT INTO messages (task_id, role, content, created_at) VALUES (?1, 'user', ?2, ?3)",
        params![task_id, content, now],
    )?;
    let user_message_id = conn.last_insert_rowid();

    let run_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM runs WHERE task_id = ?1",
        params![task_id],
        |row| row.get(0),
    )?;

    let task = conn.query_row("SELECT * FROM tasks WHERE id = ?1", params![task_id], Task::from_row)?;

    Ok((task, run_count == 0, user_message_id))
}

fn start_run(conn: &Connection, task_id: &str, user_message_id: i64, now: &str) -> Result<i64, CmdError> {
    let tmux_session = crate::tmux::session_name(task_id);
    conn.execute(
        "INSERT INTO runs (task_id, message_id, command, status, started_at, tmux_session) VALUES (?1, ?2, '', 'running', ?3, ?4)",
        params![task_id, user_message_id, now, tmux_session],
    )?;
    set_task_status(conn, task_id, "running", now)?;
    Ok(conn.last_insert_rowid())
}

fn finish_run_ok(conn: &Connection, task_id: &str, run_id: i64, reply: &AgentReply, now: &str) -> Result<Message, CmdError> {
    conn.execute(
        "UPDATE runs SET status = 'succeeded', exit_code = 0, finished_at = ?1, command = ?2 WHERE id = ?3",
        params![now, reply.command, run_id],
    )?;

    conn.execute(
        "INSERT INTO messages (task_id, role, content, created_at) VALUES (?1, 'agent', ?2, ?3)",
        params![task_id, reply.content, now],
    )?;
    let agent_message_id = conn.last_insert_rowid();

    set_task_status(conn, task_id, "idle", now)?;

    Ok(conn.query_row(
        "SELECT * FROM messages WHERE id = ?1",
        params![agent_message_id],
        Message::from_row,
    )?)
}

fn finish_run_err(conn: &Connection, task_id: &str, run_id: i64, error_message: &str, now: &str) -> Result<(), CmdError> {
    conn.execute(
        "UPDATE runs SET status = 'failed', finished_at = ?1, error = ?2 WHERE id = ?3",
        params![now, error_message, run_id],
    )?;
    set_task_status(conn, task_id, "error", now)?;
    Ok(())
}

/// Drives one full agent turn: insert the user message, start a run, call the
/// agent, then record success, failure, or cancellation. The DB is locked
/// only for the quick reads/writes around each step, never across the
/// `.await` of the agent call, so other tasks' chats stay responsive while
/// this one is "thinking".
///
/// `resolve_backend` is injected (rather than calling `backend::get_backend`
/// directly) so tests can supply a fake backend without hitting the real
/// `claude` CLI.
async fn run_agent_turn(
    db: &Mutex<Connection>,
    registry: &RunRegistry,
    resolve_backend: impl Fn(&str) -> anyhow::Result<Box<dyn AgentBackend>>,
    task_id: &str,
    content: &str,
) -> Result<Message, CmdError> {
    let now = Utc::now().to_rfc3339();
    let (task, is_first_turn, user_message_id) = {
        let conn = db.lock()?;
        begin_send(&conn, task_id, content, &now)?
    };

    let agent = resolve_backend(&task.backend).map_err(CmdError::from)?;

    let now = Utc::now().to_rfc3339();
    let run_id = {
        let conn = db.lock()?;
        start_run(&conn, task_id, user_message_id, &now)?
    };

    let cancel = CancellationToken::new();
    registry.0.lock()?.insert(task_id.to_string(), cancel.clone());

    let run = RunHandle { run_id, cancel };
    let result = agent.send(&task, content, is_first_turn, &run).await;

    // Covers every exit path below in one place — success, failure, and
    // cancellation all reach here exactly once.
    registry.0.lock()?.remove(task_id);

    let now = Utc::now().to_rfc3339();
    match result {
        Ok(SendOutcome::Reply(reply)) => {
            let conn = db.lock()?;
            finish_run_ok(&conn, task_id, run_id, &reply, &now)
        }
        Ok(SendOutcome::Cancelled) => {
            // `cancel_run` already wrote `runs.status = 'cancelled'` /
            // `tasks.status = 'idle'` for this run when it won the race —
            // surface the cancellation to the caller without touching the
            // DB again here, so we don't clobber what it just wrote.
            Err(CmdError::from("Cancelled.".to_string()))
        }
        Err(e) => {
            let conn = db.lock()?;
            let error_message = e.to_string();
            finish_run_err(&conn, task_id, run_id, &error_message, &now)?;
            Err(error_message.into())
        }
    }
}

#[tauri::command]
pub async fn send_message(
    db: State<'_, Db>,
    registry: State<'_, RunRegistry>,
    task_id: String,
    content: String,
) -> Result<Message, CmdError> {
    run_agent_turn(&db.0, &registry, backend::get_backend, &task_id, &content).await
}

/// Looks up whichever run is currently `running` for this task. Returns
/// `None` if there isn't one — e.g. the user double-clicked Cancel, or it
/// resolved in the gap between the click and this command running.
fn find_running_run(conn: &Connection, task_id: &str) -> Result<Option<(i64, Option<String>)>, CmdError> {
    let row = conn.query_row(
        "SELECT id, tmux_session FROM runs WHERE task_id = ?1 AND status = 'running' ORDER BY id DESC LIMIT 1",
        params![task_id],
        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?)),
    );
    match row {
        Ok(found) => Ok(Some(found)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

async fn cancel_run_impl(db: &Mutex<Connection>, registry: &RunRegistry, task_id: &str) -> Result<(), CmdError> {
    let found = {
        let conn = db.lock()?;
        find_running_run(&conn, task_id)?
    };
    let Some((run_id, tmux_session)) = found else {
        return Ok(());
    };

    if crate::db::run_dir(run_id).join("run.done").exists() {
        // `claude` finished in the gap between the click and this handler
        // running — let the normal polling-loop resolution path record the
        // real outcome (success or failure) instead of overwriting it with
        // 'cancelled'.
        return Ok(());
    }

    if let Some(cancel) = registry.0.lock()?.remove(task_id) {
        cancel.cancel();
    }
    if let Some(session) = &tmux_session {
        // Stops our polling loop above; this stops the actual `claude`
        // process running in the pane — without it, cancelling our side
        // alone would leave `claude` running headless inside tmux.
        let _ = crate::tmux::interrupt(session).await;
    }

    let now = Utc::now().to_rfc3339();
    let conn = db.lock()?;
    conn.execute(
        "UPDATE runs SET status = 'cancelled', finished_at = ?1 WHERE id = ?2",
        params![now, run_id],
    )?;
    set_task_status(&conn, task_id, "idle", &now)?;
    Ok(())
}

#[tauri::command]
pub async fn cancel_run(db: State<'_, Db>, registry: State<'_, RunRegistry>, task_id: String) -> Result<(), CmdError> {
    cancel_run_impl(&db.0, &registry, &task_id).await
}

/// Reconciles any runs left `status = 'running'` from a previous session —
/// the Tauri process crashed or was closed mid-turn. Called once from
/// `lib.rs`'s `.setup()` hook, after state is managed but before the event
/// loop starts, so it can reach `Db`/`RunRegistry` through the `AppHandle`.
///
/// For each stuck run: if its tmux session is gone, there's nothing left to
/// reconnect to, so the run is marked failed. Otherwise a background task
/// resumes polling via `await_completion` (which resolves immediately if
/// `run.done` is already there, or waits if not) and a fresh
/// `CancellationToken` is registered up front so Cancel still works on it.
pub async fn reconcile_startup_runs(app: &AppHandle) {
    let stuck: Vec<(i64, String, Option<String>, String)> = {
        let db = app.state::<Db>();
        let conn = match db.0.lock() {
            Ok(c) => c,
            Err(_) => return,
        };
        let query = conn.prepare(
            "SELECT runs.id, runs.task_id, runs.tmux_session, tasks.backend
             FROM runs JOIN tasks ON tasks.id = runs.task_id
             WHERE runs.status = 'running'",
        );
        let Ok(mut stmt) = query else { return };
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
            ))
        });
        match rows.and_then(|mapped| mapped.collect::<rusqlite::Result<Vec<_>>>()) {
            Ok(rows) => rows,
            Err(_) => return,
        }
    };

    for (run_id, task_id, tmux_session, backend_name) in stuck {
        let session_alive = match &tmux_session {
            Some(session) => crate::tmux::session_exists(session).await,
            None => false,
        };

        if !session_alive {
            let now = Utc::now().to_rfc3339();
            let db = app.state::<Db>();
            let conn = match db.0.lock() {
                Ok(c) => c,
                Err(_) => continue,
            };
            let _ = finish_run_err(
                &conn,
                &task_id,
                run_id,
                "interrupted: tmux session no longer exists",
                &now,
            );
            continue;
        }

        let cancel = CancellationToken::new();
        let registry = app.state::<RunRegistry>();
        let mut map = match registry.0.lock() {
            Ok(map) => map,
            Err(_) => continue,
        };
        map.insert(task_id.clone(), cancel.clone());
        drop(map);

        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            let Ok(agent) = backend::get_backend(&backend_name) else {
                return;
            };
            let run = RunHandle { run_id, cancel };
            let result = agent.await_completion(&run).await;

            let registry = app.state::<RunRegistry>();
            if let Ok(mut map) = registry.0.lock() {
                map.remove(&task_id);
            }

            let now = Utc::now().to_rfc3339();
            let db = app.state::<Db>();
            let conn = match db.0.lock() {
                Ok(c) => c,
                Err(_) => return,
            };
            match result {
                Ok(SendOutcome::Reply(reply)) => {
                    let _ = finish_run_ok(&conn, &task_id, run_id, &reply, &now);
                }
                Ok(SendOutcome::Cancelled) => {
                    // `cancel_run` already updated the row when it won this race.
                }
                Err(e) => {
                    let _ = finish_run_err(&conn, &task_id, run_id, &e.to_string(), &now);
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::Arc;

    fn test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!("schema.sql")).unwrap();
        conn
    }

    fn test_registry() -> RunRegistry {
        RunRegistry(Mutex::new(HashMap::new()))
    }

    #[test]
    fn expand_working_dir_expands_tilde_slash() {
        let expanded = expand_working_dir("~/projects/foo");
        assert!(!expanded.starts_with('~'));
        assert!(expanded.ends_with("projects/foo"));
    }

    #[test]
    fn expand_working_dir_leaves_absolute_path_untouched() {
        assert_eq!(expand_working_dir("/tmp/foo"), "/tmp/foo");
    }

    #[test]
    fn expand_working_dir_leaves_relative_non_tilde_path_untouched() {
        assert_eq!(expand_working_dir("relative/foo"), "relative/foo");
    }

    #[test]
    fn create_task_defaults_permission_mode_to_plan() {
        let conn = test_db();
        let task = create_task_impl(&conn, "test", "/tmp", None, None).unwrap();
        assert_eq!(task.permission_mode, "plan");
        assert_eq!(task.status, "idle");
    }

    #[test]
    fn create_task_respects_explicit_permission_mode() {
        let conn = test_db();
        let task = create_task_impl(&conn, "test", "/tmp", None, Some("bypassPermissions")).unwrap();
        assert_eq!(task.permission_mode, "bypassPermissions");
    }

    #[test]
    fn list_tasks_orders_by_most_recently_updated_first() {
        let conn = test_db();
        let older = create_task_impl(&conn, "older", "/tmp", None, None).unwrap();
        let newer = create_task_impl(&conn, "newer", "/tmp", None, None).unwrap();
        // Simulate `older` receiving activity after `newer` was created.
        conn.execute(
            "UPDATE tasks SET updated_at = 'z-latest' WHERE id = ?1",
            params![older.id],
        )
        .unwrap();

        let tasks = list_tasks_impl(&conn).unwrap();
        assert_eq!(tasks[0].id, older.id, "most recently updated task should be first");
        assert_eq!(tasks[1].id, newer.id);
    }

    #[test]
    fn list_messages_orders_by_id_ascending() {
        let conn = test_db();
        let task = create_task_impl(&conn, "t", "/tmp", None, None).unwrap();
        conn.execute(
            "INSERT INTO messages (task_id, role, content, created_at) VALUES (?1, 'user', 'first', 't0')",
            params![task.id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO messages (task_id, role, content, created_at) VALUES (?1, 'agent', 'second', 't1')",
            params![task.id],
        )
        .unwrap();

        let messages = list_messages_impl(&conn, &task.id).unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].content, "first");
        assert_eq!(messages[1].content, "second");
    }

    struct FakeBackend {
        outcome: Result<&'static str, &'static str>,
    }

    #[async_trait]
    impl AgentBackend for FakeBackend {
        async fn send(&self, _task: &Task, _user_message: &str, _is_first_turn: bool, _run: &RunHandle) -> anyhow::Result<SendOutcome> {
            match self.outcome {
                Ok(content) => Ok(SendOutcome::Reply(AgentReply {
                    content: content.to_string(),
                    command: "fake".to_string(),
                })),
                Err(e) => Err(anyhow::anyhow!(e.to_string())),
            }
        }

        async fn await_completion(&self, _run: &RunHandle) -> anyhow::Result<SendOutcome> {
            unreachable!("not exercised by these tests")
        }
    }

    #[tokio::test]
    async fn send_message_success_marks_task_idle_and_inserts_agent_reply() {
        let conn = test_db();
        let task = create_task_impl(&conn, "t", "/tmp", None, None).unwrap();
        let db = Mutex::new(conn);
        let registry = test_registry();

        let reply = run_agent_turn(
            &db,
            &registry,
            |_| Ok(Box::new(FakeBackend { outcome: Ok("pong") }) as Box<dyn AgentBackend>),
            &task.id,
            "ping",
        )
        .await
        .unwrap();

        assert_eq!(reply.role, "agent");
        assert_eq!(reply.content, "pong");

        let conn = db.lock().unwrap();
        let updated = conn
            .query_row("SELECT * FROM tasks WHERE id = ?1", params![task.id], Task::from_row)
            .unwrap();
        assert_eq!(updated.status, "idle");
        assert!(registry.0.lock().unwrap().get(&task.id).is_none(), "token should be removed after success");
    }

    #[tokio::test]
    async fn send_message_failure_marks_task_error_and_returns_err() {
        let conn = test_db();
        let task = create_task_impl(&conn, "t", "/tmp", None, None).unwrap();
        let db = Mutex::new(conn);
        let registry = test_registry();

        let result = run_agent_turn(
            &db,
            &registry,
            |_| Ok(Box::new(FakeBackend { outcome: Err("boom") }) as Box<dyn AgentBackend>),
            &task.id,
            "ping",
        )
        .await;

        assert!(result.is_err());

        let conn = db.lock().unwrap();
        let updated = conn
            .query_row("SELECT * FROM tasks WHERE id = ?1", params![task.id], Task::from_row)
            .unwrap();
        assert_eq!(updated.status, "error");
        assert!(registry.0.lock().unwrap().get(&task.id).is_none(), "token should be removed after failure");
    }

    struct RecordingBackend {
        is_first_turn_calls: Arc<Mutex<Vec<bool>>>,
    }

    #[async_trait]
    impl AgentBackend for RecordingBackend {
        async fn send(&self, _task: &Task, _user_message: &str, is_first_turn: bool, _run: &RunHandle) -> anyhow::Result<SendOutcome> {
            self.is_first_turn_calls.lock().unwrap().push(is_first_turn);
            Ok(SendOutcome::Reply(AgentReply {
                content: "ok".to_string(),
                command: "fake".to_string(),
            }))
        }

        async fn await_completion(&self, _run: &RunHandle) -> anyhow::Result<SendOutcome> {
            unreachable!("not exercised by these tests")
        }
    }

    #[tokio::test]
    async fn send_message_marks_first_turn_correctly_across_calls() {
        let conn = test_db();
        let task = create_task_impl(&conn, "t", "/tmp", None, None).unwrap();
        let db = Mutex::new(conn);
        let registry = test_registry();
        let calls = Arc::new(Mutex::new(Vec::new()));

        let calls_for_first = calls.clone();
        run_agent_turn(
            &db,
            &registry,
            move |_| {
                Ok(Box::new(RecordingBackend {
                    is_first_turn_calls: calls_for_first.clone(),
                }) as Box<dyn AgentBackend>)
            },
            &task.id,
            "first message",
        )
        .await
        .unwrap();

        let calls_for_second = calls.clone();
        run_agent_turn(
            &db,
            &registry,
            move |_| {
                Ok(Box::new(RecordingBackend {
                    is_first_turn_calls: calls_for_second.clone(),
                }) as Box<dyn AgentBackend>)
            },
            &task.id,
            "second message",
        )
        .await
        .unwrap();

        let recorded = calls.lock().unwrap();
        assert_eq!(*recorded, vec![true, false], "first call should be first-turn, second should not");
    }

    #[tokio::test]
    async fn cancel_run_is_noop_when_nothing_is_running() {
        let conn = test_db();
        let task = create_task_impl(&conn, "t", "/tmp", None, None).unwrap();
        let db = Mutex::new(conn);
        let registry = test_registry();

        cancel_run_impl(&db, &registry, &task.id).await.unwrap();

        let conn = db.lock().unwrap();
        let updated = conn
            .query_row("SELECT * FROM tasks WHERE id = ?1", params![task.id], Task::from_row)
            .unwrap();
        assert_eq!(updated.status, "idle");
    }

    #[tokio::test]
    async fn cancel_run_cancels_token_and_marks_idle_when_run_is_in_flight() {
        let conn = test_db();
        let task = create_task_impl(&conn, "t", "/tmp", None, None).unwrap();
        conn.execute(
            "INSERT INTO messages (task_id, role, content, created_at) VALUES (?1, 'user', 'hi', 't0')",
            params![task.id],
        )
        .unwrap();
        let user_message_id = conn.last_insert_rowid();
        let run_id = start_run(&conn, &task.id, user_message_id, "t0").unwrap();

        let db = Mutex::new(conn);
        let registry = test_registry();
        let cancel = CancellationToken::new();
        registry.0.lock().unwrap().insert(task.id.clone(), cancel.clone());

        cancel_run_impl(&db, &registry, &task.id).await.unwrap();

        assert!(cancel.is_cancelled(), "cancel_run should cancel the in-flight token");
        assert!(registry.0.lock().unwrap().get(&task.id).is_none());

        let conn = db.lock().unwrap();
        let updated_task = conn
            .query_row("SELECT * FROM tasks WHERE id = ?1", params![task.id], Task::from_row)
            .unwrap();
        assert_eq!(updated_task.status, "idle");

        let updated_status: String = conn
            .query_row("SELECT status FROM runs WHERE id = ?1", params![run_id], |row| row.get(0))
            .unwrap();
        assert_eq!(updated_status, "cancelled");
    }

    #[tokio::test]
    async fn cancel_run_is_noop_when_run_already_finished_on_disk() {
        let conn = test_db();
        let task = create_task_impl(&conn, "t", "/tmp", None, None).unwrap();
        conn.execute(
            "INSERT INTO messages (task_id, role, content, created_at) VALUES (?1, 'user', 'hi', 't0')",
            params![task.id],
        )
        .unwrap();
        let user_message_id = conn.last_insert_rowid();
        let run_id = start_run(&conn, &task.id, user_message_id, "t0").unwrap();

        let run_dir = crate::db::run_dir(run_id);
        std::fs::create_dir_all(&run_dir).unwrap();
        std::fs::write(run_dir.join("run.done"), "").unwrap();

        let db = Mutex::new(conn);
        let registry = test_registry();
        let cancel = CancellationToken::new();
        registry.0.lock().unwrap().insert(task.id.clone(), cancel.clone());

        cancel_run_impl(&db, &registry, &task.id).await.unwrap();

        // The race was already won by natural completion — cancel_run must
        // not touch the token or overwrite the run/task rows.
        assert!(!cancel.is_cancelled());
        let conn = db.lock().unwrap();
        let updated_status: String = conn
            .query_row("SELECT status FROM runs WHERE id = ?1", params![run_id], |row| row.get(0))
            .unwrap();
        assert_eq!(updated_status, "running");

        std::fs::remove_dir_all(&run_dir).unwrap();
    }

    // Hits the real `claude` CLI and a real tmux session — not run by
    // default. Run with `cargo test -- --ignored` to confirm Cancel actually
    // interrupts an in-flight call end-to-end: dispatch a slow real prompt
    // through the same `run_agent_turn` the `send_message` command uses,
    // cancel it mid-flight through the same `cancel_run_impl` the
    // `cancel_run` command uses, and check the run/task land on
    // cancelled/idle while the tmux session itself survives.
    #[ignore]
    #[tokio::test]
    async fn cancel_run_interrupts_a_real_in_flight_claude_call() {
        let conn = test_db();
        let task = create_task_impl(&conn, "cancel-smoke-test", "/tmp", None, None).unwrap();
        let task_id = task.id.clone();
        let db = Mutex::new(conn);
        let registry = test_registry();

        let send_fut = run_agent_turn(
            &db,
            &registry,
            backend::get_backend,
            &task_id,
            "Count slowly from one to fifty, one number per line, with a short narrated aside between each number.",
        );
        let cancel_after_delay = async {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            cancel_run_impl(&db, &registry, &task_id).await
        };

        let (send_result, cancel_result) = tokio::join!(send_fut, cancel_after_delay);

        cancel_result.unwrap();
        match send_result {
            Err(CmdError(message)) => assert_eq!(message, "Cancelled."),
            other => panic!("expected a Cancelled error, got {other:?}"),
        }

        let conn = db.lock().unwrap();
        let updated_task = conn
            .query_row("SELECT * FROM tasks WHERE id = ?1", params![task_id], Task::from_row)
            .unwrap();
        assert_eq!(updated_task.status, "idle");
        drop(conn);

        let session = crate::tmux::session_name(&task_id);
        assert!(
            crate::tmux::session_exists(&session).await,
            "the tmux session itself should survive a cancel — only the claude process inside it is interrupted"
        );

        let _ = tokio::process::Command::new("tmux")
            .args(["kill-session", "-t", &session])
            .output()
            .await;
    }
}
