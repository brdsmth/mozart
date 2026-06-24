use rusqlite::Connection;
use std::fs;
use std::path::PathBuf;

const SCHEMA: &str = include_str!("schema.sql");

pub fn mozart_home() -> PathBuf {
    dirs::home_dir()
        .expect("could not determine home directory")
        .join(".mozart")
}

/// Where a single run's `claude -p` stdout/stderr/exit-code/sentinel files
/// live — `~/.mozart/runs/<run_id>/`. Shared by `backend::claude_cli` (writes
/// and polls these), `commands::cancel_run` (checks for the done-file race),
/// and startup reconciliation in `lib.rs` (resumes polling after a restart),
/// so the layout only needs to change in one place.
pub fn run_dir(run_id: i64) -> PathBuf {
    mozart_home().join("runs").join(run_id.to_string())
}

/// `CREATE TABLE IF NOT EXISTS` in schema.sql never alters a table that
/// already exists on disk, so columns added after Step 1 need an explicit
/// migration here. Each migration is run unconditionally and "duplicate
/// column name" errors are swallowed — the column already being there is
/// success, not failure. This is the project's first schema change since
/// Step 1; later steps should add to this same function rather than
/// inventing a new migration mechanism.
fn migrate(conn: &Connection) {
    let migrations = ["ALTER TABLE runs ADD COLUMN tmux_session TEXT"];

    for migration in migrations {
        if let Err(e) = conn.execute(migration, []) {
            let message = e.to_string();
            if !message.contains("duplicate column name") {
                panic!("migration failed ({migration}): {e}");
            }
        }
    }
}

pub fn open() -> Connection {
    let dir = mozart_home();
    fs::create_dir_all(&dir).expect("could not create ~/.mozart");

    let conn = Connection::open(dir.join("mozart.db")).expect("could not open mozart.db");
    conn.execute_batch(SCHEMA).expect("could not apply schema");
    migrate(&conn);
    conn
}
