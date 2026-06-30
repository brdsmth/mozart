use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, SystemTime};
use uuid::Uuid;

#[derive(Parser)]
#[command(name = "mozart", about = "Bare-metal claude session manager", version)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Mint a session ID and start its tmux session. Prints the ID.
    New {
        /// Working directory for the agent (defaults to current directory)
        #[arg(default_value = ".")]
        working_dir: String,
    },
    /// Dispatch one message turn. Prints the run ID.
    ///
    /// Two forms:
    ///   mozart send <session-id> <message>   — explicit session
    ///   mozart send <message>                — uses active session (set via: mozart session use <n>)
    Send {
        /// Session ID, or — when no message follows — the message itself (uses active session)
        session_id: String,
        /// Message to send. If omitted, session_id is treated as the message and the
        /// active session from config is used.
        message: Option<String>,
        /// Allow the agent to edit files and run commands
        #[arg(long)]
        bypass: bool,
    },
    /// Block until a run finishes, then print its output
    Wait {
        /// Run ID to wait for. If omitted, waits for the active session's latest run.
        run_id: Option<String>,
        /// Print the full raw JSON payload instead of the reply + digest
        #[arg(long)]
        json: bool,
    },
    /// Attach your terminal to a session's tmux pane
    Attach {
        /// Session ID. If omitted, attaches to the active session.
        session_id: Option<String>,
    },
    /// Send C-c to interrupt whatever is running in a session
    Cancel {
        /// Session ID. If omitted, cancels the active session.
        session_id: Option<String>,
    },
    /// Print a run's raw output file without waiting
    Cat {
        run_id: String,
    },
    /// List all active mozart tmux sessions and known session IDs
    Ls,
    /// Kill a session's tmux session and remove its state
    Kill {
        session_id: String,
    },
    /// Kill all active mozart tmux sessions and remove all session state
    KillAll,
    /// Show total API cost across all runs
    Cost,
    /// Print a high-level view of all sessions and their current state
    Status {
        /// Show full session and run IDs instead of truncated ones
        #[arg(long)]
        full: bool,
        /// Only show busy sessions
        #[arg(long)]
        busy: bool,
        /// Only show idle sessions
        #[arg(long)]
        idle: bool,
    },
    /// Print a workflow cheatsheet
    Guide,
    /// Manage saved target repos
    Repo {
        #[command(subcommand)]
        action: RepoCmd,
    },
    /// Manage the active session (use without explicit session IDs)
    Session {
        #[command(subcommand)]
        action: SessionCmd,
    },
    /// Decompose a goal into isolated tasks and manage task plans
    Plan {
        #[command(subcommand)]
        action: PlanCmd,
    },
    /// Execute a plan's tasks in dependency order, wave by wave
    Queue {
        #[command(subcommand)]
        action: QueueCmd,
    },
}

#[derive(Subcommand)]
enum SessionCmd {
    /// List all sessions (* = active), with repo path and status
    Ls,
    /// Set the active session by number (see: mozart session ls)
    Use {
        n: usize,
    },
}

#[derive(Subcommand)]
enum RepoCmd {
    /// List saved repos
    Ls,
    /// Add a repo path (or re-activate it if already saved)
    Set {
        path: String,
    },
    /// Switch the active repo by number (see: mozart repo ls)
    Use {
        n: usize,
    },
}

#[derive(Subcommand)]
enum PlanCmd {
    /// Call claude to decompose a goal into tasks and write to disk
    New {
        /// The high-level goal to decompose into tasks
        goal: String,
    },
    /// List all plans
    Ls,
    /// Show numbered task list for a plan
    Show {
        /// Plan ID (from `mozart plan ls`)
        plan_id: String,
    },
    /// Dispatch one task to a session (creates a new session if none given)
    Dispatch {
        /// Plan ID
        plan_id: String,
        /// Task number (1-indexed, matching `mozart plan show` output)
        task_num: usize,
        /// Session ID to dispatch to. If omitted, a new session is created.
        session_id: Option<String>,
    },
    /// Dispatch all tasks at once, each to its own new session
    DispatchAll {
        /// Plan ID
        plan_id: String,
    },
    /// Review results of all dispatched tasks (exit codes, errors, cost)
    Review {
        /// Plan ID
        plan_id: String,
    },
}

#[derive(Subcommand)]
enum QueueCmd {
    /// Create a queue from a plan's tasks (respects depends_on ordering)
    New {
        /// Plan ID to build the queue from
        plan_id: String,
    },
    /// List all queues with goal and progress
    Ls,
    /// Show queue items and their current status
    Show {
        /// Queue ID (from `mozart queue ls`)
        queue_id: String,
    },
    /// Dispatch tasks wave by wave in dependency order, blocking until all complete
    Run {
        /// Queue ID
        queue_id: String,
    },
}

// ~/.mozart/cli/ is the root for all CLI-managed state.
// Sessions live at ~/.mozart/cli/sessions/<session-id> (presence = has had a turn)
// Runs live at    ~/.mozart/cli/runs/<run-id>/{run.out, run.err, run.exit, run.done}

fn cli_home() -> PathBuf {
    dirs::home_dir()
        .expect("no home dir")
        .join(".mozart")
        .join("cli")
}

#[derive(Serialize, Deserialize, Default)]
struct Config {
    repos: Vec<String>,
    active: usize,
    #[serde(default)]
    active_session: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct Task {
    title: String,
    description: String,
    context: String,
    #[serde(default)]
    depends_on: Vec<usize>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct DispatchRecord {
    task_num: usize,
    title: String,
    session_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "lowercase")]
enum QueueStatus { Pending, Running, Done, Failed }

#[derive(Serialize, Deserialize, Debug, Clone)]
struct QueueItem {
    item_num: usize,
    plan_id: String,
    task_num: usize,
    title: String,
    #[serde(default)]
    depends_on: Vec<usize>,
    status: QueueStatus,
    #[serde(default)]
    session_id: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct QueueMeta {
    plan_id: String,
    goal: String,
    repo: String,
}

fn config_path() -> PathBuf {
    cli_home().join("config.json")
}

fn queues_dir() -> PathBuf { cli_home().join("queues") }
fn queue_dir(id: &str) -> PathBuf { queues_dir().join(id) }
fn queue_items_path(id: &str) -> PathBuf { queue_dir(id).join("items.json") }
fn queue_meta_path(id: &str) -> PathBuf { queue_dir(id).join("meta.json") }

fn load_queue_items(id: &str) -> anyhow::Result<Vec<QueueItem>> {
    Ok(serde_json::from_str(&fs::read_to_string(queue_items_path(id))?)?)
}

fn save_queue_items(id: &str, items: &[QueueItem]) -> anyhow::Result<()> {
    fs::write(queue_items_path(id), serde_json::to_string_pretty(items)?)?;
    Ok(())
}

fn load_queue_meta(id: &str) -> anyhow::Result<QueueMeta> {
    Ok(serde_json::from_str(&fs::read_to_string(queue_meta_path(id))?)?)
}

fn plan_sessions_path(plan_id: &str) -> PathBuf {
    plan_dir(plan_id).join("sessions.json")
}

fn load_dispatch_records(plan_id: &str) -> Vec<DispatchRecord> {
    fs::read_to_string(plan_sessions_path(plan_id))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_dispatch_record(plan_id: &str, record: DispatchRecord) -> anyhow::Result<()> {
    let mut records = load_dispatch_records(plan_id);
    if let Some(existing) = records.iter_mut().find(|r| r.task_num == record.task_num) {
        *existing = record;
    } else {
        records.push(record);
    }
    records.sort_by_key(|r| r.task_num);
    fs::write(plan_sessions_path(plan_id), serde_json::to_string_pretty(&records)?)?;
    Ok(())
}

fn plans_dir() -> PathBuf {
    cli_home().join("plans")
}

fn plan_dir(plan_id: &str) -> PathBuf {
    plans_dir().join(plan_id)
}

fn load_config() -> Config {
    let path = config_path();
    fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_config(cfg: &Config) -> anyhow::Result<()> {
    fs::create_dir_all(cli_home())?;
    fs::write(config_path(), serde_json::to_string_pretty(cfg)?)?;
    Ok(())
}

fn active_repo(cfg: &Config) -> Option<&str> {
    cfg.repos.get(cfg.active).map(|s| s.as_str())
}

fn active_session_id(cfg: &Config) -> Option<&str> {
    cfg.active_session.as_deref()
}

fn sessions_all_ids() -> anyhow::Result<Vec<String>> {
    let dir = cli_home().join("sessions");
    if !dir.exists() {
        return Ok(vec![]);
    }
    let mut ids: std::collections::BTreeSet<String> = Default::default();
    for entry in fs::read_dir(&dir)?.filter_map(|e| e.ok()) {
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(base) = name.strip_suffix(".repo") {
            ids.insert(base.to_string());
        } else {
            ids.insert(name);
        }
    }
    Ok(ids.into_iter().collect())
}

fn session_marker(session_id: &str) -> PathBuf {
    cli_home().join("sessions").join(session_id)
}

fn session_repo_file(session_id: &str) -> PathBuf {
    cli_home().join("sessions").join(format!("{session_id}.repo"))
}

fn session_repo(session_id: &str) -> Option<String> {
    fs::read_to_string(session_repo_file(session_id))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn run_dir(run_id: &str) -> PathBuf {
    cli_home().join("runs").join(run_id)
}

/// The most recent run id dispatched for a session, stored as the contents of
/// the session marker file. `None` for a session that has never had a turn, or
/// one created before run ids were tracked (marker exists but is empty).
fn session_latest_run(session_id: &str) -> Option<String> {
    fs::read_to_string(session_marker(session_id))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Whether the session has a dispatched run that hasn't produced its `run.done`
/// sentinel yet. A session is a single tmux pane (one shell, one foreground
/// process), so a run is in flight exactly when its latest run is unfinished.
fn session_busy_run(session_id: &str) -> Option<String> {
    let active = session_latest_run(session_id)?;
    let dir = run_dir(&active);
    (dir.exists() && !dir.join("run.done").exists()).then_some(active)
}

fn tmux_name(session_id: &str) -> String {
    format!("mozart-{}", session_id)
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn tmux_session_exists(name: &str) -> bool {
    Command::new("tmux")
        .args(["has-session", "-t", name])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn ensure_tmux_session(name: &str, working_dir: &str) -> anyhow::Result<()> {
    if !tmux_session_exists(name) {
        eprintln!("→ tmux new-session -d -s {name} -c {working_dir}");
        let status = Command::new("tmux")
            .args(["new-session", "-d", "-s", name, "-c", working_dir])
            .status()?;
        if !status.success() {
            anyhow::bail!("tmux new-session failed for {}", name);
        }
    } else {
        eprintln!("· tmux session {name} already exists");
    }
    Ok(())
}

fn tmux_send(name: &str, command: &str) -> anyhow::Result<()> {
    let status = Command::new("tmux")
        .args(["send-keys", "-t", name, command, "Enter"])
        .status()?;
    if !status.success() {
        anyhow::bail!("tmux send-keys failed for {}", name);
    }
    Ok(())
}

fn create_session(working_dir: &str) -> anyhow::Result<String> {
    let session_id = Uuid::new_v4().to_string();
    let tmux = tmux_name(&session_id);

    let working_dir = if working_dir == "." {
        let cfg = load_config();
        if let Some(repo) = active_repo(&cfg) {
            eprintln!("· using active repo: {repo}");
            repo.to_string()
        } else {
            std::env::current_dir()?.to_string_lossy().into_owned()
        }
    } else {
        working_dir.to_string()
    };

    ensure_tmux_session(&tmux, &working_dir)?;

    fs::create_dir_all(cli_home().join("sessions"))?;
    fs::write(session_repo_file(&session_id), &working_dir)?;

    eprintln!();
    eprintln!("  repo:    {working_dir}");
    eprintln!("  attach:  tmux attach -t {tmux}");
    eprintln!("  kill:    tmux kill-session -t {tmux}");
    eprintln!();

    Ok(session_id)
}

fn cmd_new(working_dir: &str) -> anyhow::Result<()> {
    let session_id = create_session(working_dir)?;
    // stdout only — this is what SESSION=$(...) captures
    println!("{}", session_id);
    Ok(())
}

fn cmd_send(session_id: &str, message: &str, bypass: bool) -> anyhow::Result<()> {
    let tmux = tmux_name(session_id);

    if !tmux_session_exists(&tmux) {
        anyhow::bail!("no tmux session for {} — did you run `mozart new`?", session_id);
    }

    // A session is one tmux pane = one shell = one foreground process. A second
    // turn dispatched before the first finishes would silently buffer in the
    // pane and run only once the first returns to the prompt — turning `wait`
    // into a much longer block than expected. Refuse instead, with a clear way
    // out. `cancel` finalizes the in-flight run, so it's the escape hatch.
    if let Some(busy) = session_busy_run(session_id) {
        anyhow::bail!(
            "session {session_id} is busy — run {busy} hasn't finished\n  \
             wait:    mozart wait {busy}\n  \
             cancel:  mozart cancel {session_id}"
        );
    }

    let is_first = !session_marker(session_id).exists();
    let session_flag = if is_first { "--session-id" } else { "--resume" };
    let permission_mode = if bypass { "bypassPermissions" } else { "plan" };

    let run_id = Uuid::new_v4().to_string();
    let dir = run_dir(&run_id);
    fs::create_dir_all(&dir)?;

    let out_path  = dir.join("run.out");
    let err_path  = dir.join("run.err");
    let exit_path = dir.join("run.exit");
    let done_path = dir.join("run.done");

    let invocation = format!(
        "claude -p {flag} {sid} --output-format json --permission-mode {mode} {msg}",
        flag = session_flag,
        sid  = shell_quote(session_id),
        mode = permission_mode,
        msg  = shell_quote(message),
    );
    let wrapped = format!(
        "{inv} > {out} 2> {err}; echo $? > {exit}; touch {done}",
        inv  = invocation,
        out  = shell_quote(&out_path.to_string_lossy()),
        err  = shell_quote(&err_path.to_string_lossy()),
        exit = shell_quote(&exit_path.to_string_lossy()),
        done = shell_quote(&done_path.to_string_lossy()),
    );

    if is_first {
        eprintln!("→ first turn  (claude will name the conversation using --session-id)");
    } else {
        eprintln!("→ resuming session");
    }
    if let Some(repo) = session_repo(session_id) {
        eprintln!("· repo:        {repo}");
    }
    eprintln!("→ dispatching into {tmux}:");
    eprintln!("  {invocation}");
    eprintln!();
    eprintln!("  run dir: {}", dir.display());
    eprintln!("  stream:  tail -f {}", out_path.display());
    eprintln!("  watch:   tmux attach -t {tmux}");
    eprintln!();

    tmux_send(&tmux, &wrapped)?;

    // Record this run as the session's latest: its presence marks the session
    // as having had a turn (so the next send uses --resume), and its contents
    // let the busy guard above and `cancel` below find the in-flight run.
    fs::create_dir_all(cli_home().join("sessions"))?;
    fs::write(session_marker(session_id), &run_id)?;

    // stdout only — this is what RUN=$(...) captures
    println!("{}", run_id);
    Ok(())
}

fn cmd_wait(run_id: &str, json: bool) -> anyhow::Result<()> {
    let dir = run_dir(run_id);
    if !dir.exists() {
        anyhow::bail!("run {} not found at {}", run_id, dir.display());
    }

    let done_path = dir.join("run.done");
    eprintln!("· polling {} ...", done_path.display());

    while !done_path.exists() {
        thread::sleep(Duration::from_millis(500));
    }

    let exit_code: Option<i32> = fs::read_to_string(dir.join("run.exit"))
        .ok()
        .and_then(|s| s.trim().parse().ok());
    let stdout = fs::read_to_string(dir.join("run.out")).unwrap_or_default();
    let stderr = fs::read_to_string(dir.join("run.err")).unwrap_or_default();

    eprintln!("· done  exit {:?}", exit_code.unwrap_or(-1));
    eprintln!();

    if exit_code != Some(0) {
        eprintln!("claude exited non-zero");
        if !stderr.is_empty() {
            eprintln!("{}", stderr.trim());
        }
        std::process::exit(1);
    }

    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .map_err(|e| anyhow::anyhow!("failed to parse claude output: {e}\nraw: {stdout}"))?;

    // `--json` hands back the untouched payload (still on stdout, so it stays
    // pipeable into jq); the error signal is preserved via the exit code.
    if json {
        println!("{}", stdout.trim_end());
        if parsed["is_error"].as_bool() == Some(true) {
            std::process::exit(1);
        }
        return Ok(());
    }

    if parsed["is_error"].as_bool() == Some(true) {
        eprintln!("claude error: {}", parsed["result"]);
        std::process::exit(1);
    }

    // stdout stays just the reply, so `$(mozart wait $RUN)` captures only the
    // agent's text; the digest goes to stderr.
    println!("{}", parsed["result"].as_str().unwrap_or(&stdout));
    print_run_digest(&parsed);
    Ok(())
}

fn fmt_duration(ms: f64) -> String {
    let total_secs = (ms / 1000.0).round() as u64;
    if total_secs >= 60 {
        format!("{}m{:02}s", total_secs / 60, total_secs % 60)
    } else {
        format!("{total_secs}s")
    }
}

/// Prints a one-glance footer to stderr after a successful turn: turns,
/// wall-clock, cost, and — most importantly — any tool calls the run tried
/// and was denied. Denials only happen in plan mode (the default), so their
/// presence is exactly the signal that the turn needed `--bypass`. Without
/// this, a turn whose every write was blocked looks indistinguishable from a
/// turn that simply chose not to write anything.
fn print_run_digest(parsed: &serde_json::Value) {
    let mut stats: Vec<String> = Vec::new();
    if let Some(t) = parsed["num_turns"].as_u64() {
        stats.push(format!("{t} turn{}", if t == 1 { "" } else { "s" }));
    }
    if let Some(d) = parsed["duration_ms"].as_f64() {
        stats.push(fmt_duration(d));
    }
    if let Some(c) = parsed["total_cost_usd"].as_f64() {
        stats.push(format!("${c:.2}"));
    }

    // Group denials by tool name for a compact `Write×13, Bash×2` breakdown.
    let mut by_tool: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    if let Some(arr) = parsed["permission_denials"].as_array() {
        for d in arr {
            let tool = d["tool_name"].as_str().unwrap_or("unknown").to_string();
            *by_tool.entry(tool).or_insert(0) += 1;
        }
    }
    let total_denials: usize = by_tool.values().sum();

    if stats.is_empty() && total_denials == 0 {
        return;
    }

    let rule = "─────────────────────────────────";
    eprintln!();
    eprintln!("{rule}");
    if !stats.is_empty() {
        eprintln!(" {}", stats.join(" · "));
    }
    if total_denials > 0 {
        let breakdown = by_tool
            .iter()
            .map(|(tool, n)| format!("{tool}×{n}"))
            .collect::<Vec<_>>()
            .join(", ");
        eprintln!(
            " ⚠ {total_denials} tool call{} DENIED: {breakdown}",
            if total_denials == 1 { "" } else { "s" }
        );
        eprintln!("   (session is in plan mode — re-send with --bypass to allow)");
    }
    eprintln!("{rule}");
}

fn cmd_attach(session_id: &str) -> anyhow::Result<()> {
    let tmux = tmux_name(session_id);
    eprintln!("→ tmux attach-session -t {tmux}");
    // Replace the current process with tmux attach so the terminal is fully
    // handed over — no wrapper process sitting in between.
    let err = std::os::unix::process::CommandExt::exec(
        Command::new("tmux").args(["attach-session", "-t", &tmux]),
    );
    Err(err.into())
}

fn cmd_cancel(session_id: &str) -> anyhow::Result<()> {
    let tmux = tmux_name(session_id);
    eprintln!("→ tmux send-keys -t {tmux} C-c");
    Command::new("tmux")
        .args(["send-keys", "-t", &tmux, "C-c", ""])
        .status()?;

    // The C-c aborts the whole `claude ...; echo $? ...; touch run.done` chain,
    // so the done sentinel never gets written on its own. Without finalizing it
    // here, `mozart wait` would block forever and the busy guard in `send`
    // would consider the session permanently busy. Record a SIGINT exit code
    // (130) and drop the sentinel so both unstick.
    if let Some(active) = session_busy_run(session_id) {
        let dir = run_dir(&active);
        let _ = fs::write(dir.join("run.exit"), "130");
        let _ = fs::write(dir.join("run.done"), "");
        eprintln!("· finalized run {active} (cancelled)");
    }
    Ok(())
}

fn cmd_cat(run_id: &str) -> anyhow::Result<()> {
    let out = run_dir(run_id).join("run.out");
    if !out.exists() {
        anyhow::bail!("run {} not found", run_id);
    }
    eprintln!("· {}", out.display());
    eprintln!();
    print!("{}", fs::read_to_string(&out)?);
    Ok(())
}

fn cmd_ls() -> anyhow::Result<()> {
    // Active tmux sessions whose name starts with "mozart-"
    eprintln!("tmux sessions:");
    let tmux_out = Command::new("tmux")
        .args(["ls", "-F", "#{session_name}"])
        .output();

    let mozart_sessions: Vec<String> = match tmux_out {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| l.starts_with("mozart-"))
            .map(|l| l.to_string())
            .collect(),
        _ => vec![],
    };

    if mozart_sessions.is_empty() {
        eprintln!("  (none)");
    } else {
        for name in &mozart_sessions {
            eprintln!("  {name}");
            eprintln!("    attach:  tmux attach -t {name}");
            eprintln!("    kill:    tmux kill-session -t {name}");
        }
    }

    // Known sessions on disk (have had at least one turn)
    eprintln!();
    eprintln!("sessions with turns  (~/.mozart/cli/sessions/):");
    let sessions_dir = cli_home().join("sessions");
    if sessions_dir.exists() {
        let mut ids: Vec<String> = fs::read_dir(&sessions_dir)?
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| !name.ends_with(".repo"))
            .collect();
        ids.sort();
        if ids.is_empty() {
            eprintln!("  (none)");
        } else {
            for id in &ids {
                let tmux = tmux_name(id);
                let alive = tmux_session_exists(&tmux);
                let status = if alive { "tmux alive" } else { "tmux gone" };
                eprintln!("  {id}  [{status}]");
                eprintln!("    kill:  mozart kill {id}");
            }
        }
    } else {
        eprintln!("  (none)");
    }

    Ok(())
}

fn cmd_kill(session_id: &str) -> anyhow::Result<()> {
    let tmux = tmux_name(session_id);

    if tmux_session_exists(&tmux) {
        eprintln!("→ tmux kill-session -t {tmux}");
        Command::new("tmux")
            .args(["kill-session", "-t", &tmux])
            .status()?;
    } else {
        eprintln!("· tmux session {tmux} is not running");
    }

    let marker = session_marker(session_id);
    if marker.exists() {
        eprintln!("→ rm {}", marker.display());
        fs::remove_file(&marker)?;
    }

    let repo_file = session_repo_file(session_id);
    if repo_file.exists() {
        let _ = fs::remove_file(&repo_file);
    }

    eprintln!("· done");
    Ok(())
}

fn cmd_kill_all() -> anyhow::Result<()> {
    let tmux_out = Command::new("tmux")
        .args(["ls", "-F", "#{session_name}"])
        .output();

    let mozart_sessions: Vec<String> = match tmux_out {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| l.starts_with("mozart-"))
            .map(|l| l.to_string())
            .collect(),
        _ => vec![],
    };

    if mozart_sessions.is_empty() {
        eprintln!("· no active mozart tmux sessions");
    } else {
        for name in &mozart_sessions {
            eprintln!("→ tmux kill-session -t {name}");
            Command::new("tmux")
                .args(["kill-session", "-t", name])
                .status()?;
        }
    }

    let sessions_dir = cli_home().join("sessions");
    if sessions_dir.exists() {
        let markers: Vec<_> = fs::read_dir(&sessions_dir)?
            .filter_map(|e| e.ok())
            .collect();
        for entry in &markers {
            eprintln!("→ rm {}", entry.path().display());
            fs::remove_file(entry.path())?;
        }
    }

    eprintln!("· done");
    Ok(())
}

fn cmd_cost() -> anyhow::Result<()> {
    let runs_dir = cli_home().join("runs");
    if !runs_dir.exists() {
        println!("no runs found");
        return Ok(());
    }

    // (date_string, cost)
    let mut by_day: std::collections::BTreeMap<String, f64> = std::collections::BTreeMap::new();
    let mut total = 0f64;
    let mut n_costed = 0usize;
    let mut n_scanned = 0usize;

    for entry in fs::read_dir(&runs_dir)?.filter_map(|e| e.ok()) {
        let out_path = entry.path().join("run.out");
        if !out_path.exists() {
            continue;
        }
        n_scanned += 1;

        let contents = match fs::read_to_string(&out_path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let parsed: serde_json::Value = match serde_json::from_str(&contents) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let cost = match parsed["total_cost_usd"].as_f64() {
            Some(c) => c,
            None => continue,
        };

        // Use mtime of run.out as the date for this run.
        let date = fs::metadata(&out_path)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| {
                let secs = t.duration_since(SystemTime::UNIX_EPOCH).ok()?.as_secs();
                // UTC date from epoch seconds: simple division, no external crate needed.
                // Days since epoch → date via the algorithm from http://howardhinnant.github.io/date_algorithms.html
                let z = (secs / 86400) as i64 + 719468;
                let era = z.div_euclid(146097);
                let doe = (z - era * 146097) as u64;
                let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
                let y = yoe as i64 + era * 400;
                let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
                let mp = (5 * doy + 2) / 153;
                let d = doy - (153 * mp + 2) / 5 + 1;
                let m = if mp < 10 { mp + 3 } else { mp - 9 };
                let y = if m <= 2 { y + 1 } else { y };
                Some(format!("{y:04}-{m:02}-{d:02}"))
            })
            .unwrap_or_else(|| "unknown".to_string());

        *by_day.entry(date).or_insert(0.0) += cost;
        total += cost;
        n_costed += 1;
    }

    println!("runs scanned:  {n_scanned}");
    println!("runs with cost:{n_costed:>3}");
    println!("total cost:    ${total:.4}");
    if !by_day.is_empty() {
        println!();
        println!("by day:");
        for (day, cost) in &by_day {
            println!("  {day}    ${cost:.4}");
        }
    }

    Ok(())
}

fn elapsed_secs(path: &PathBuf) -> Option<u64> {
    fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .map(|d| d.as_secs())
}

fn fmt_elapsed(secs: u64) -> String {
    if secs >= 3600 {
        format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    }
}

fn run_token_usage(run_id: &str) -> Option<(u64, u64)> {
    let stdout = fs::read_to_string(run_dir(run_id).join("run.out")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&stdout).ok()?;
    let input  = v["usage"]["input_tokens"].as_u64()?;
    let output = v["usage"]["output_tokens"].as_u64()?;
    Some((input, output))
}

fn fmt_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        format!("{n}")
    }
}

fn cmd_status(full: bool, only_busy: bool, only_idle: bool) -> anyhow::Result<()> {
    let id_len = if full { usize::MAX } else { 8 };
    let short = |s: &str| -> String {
        if s.len() <= id_len { s.to_string() } else { format!("{}…", &s[..id_len]) }
    };
    // Collect all session IDs known on disk.
    let sessions_dir = cli_home().join("sessions");
    let mut session_ids: Vec<String> = if sessions_dir.exists() {
        fs::read_dir(&sessions_dir)?
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| !name.ends_with(".repo"))
            .collect()
    } else {
        vec![]
    };
    session_ids.sort();

    // Also include any tmux sessions that don't have a marker yet (new, no turns).
    let tmux_out = Command::new("tmux")
        .args(["ls", "-F", "#{session_name}"])
        .output();
    let tmux_names: Vec<String> = match tmux_out {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| l.starts_with("mozart-"))
            .map(|l| l["mozart-".len()..].to_string())
            .collect(),
        _ => vec![],
    };
    for id in &tmux_names {
        if !session_ids.contains(id) {
            session_ids.push(id.clone());
        }
    }

    if session_ids.is_empty() {
        println!("no sessions");
        return Ok(());
    }

    struct Row {
        // 0 = busy, 1 = idle, 2 = new — controls sort group
        group: u8,
        // busy: elapsed secs (lower = newer start, sort ascending within group)
        // idle: secs since done (lower = more recent, sort ascending within group)
        sort_key: u64,
        line: String,
    }

    let mut rows: Vec<Row> = Vec::new();
    let mut n_busy = 0usize;
    let mut n_idle = 0usize;
    let mut total_input: u64 = 0;
    let mut total_output: u64 = 0;

    for id in &session_ids {
        let id_s = short(id);
        let tmux_alive = tmux_session_exists(&tmux_name(id));

        if let Some(run_id) = session_busy_run(id) {
            n_busy += 1;
            if only_idle { continue; }
            let run_s = short(&run_id);
            let out_path = run_dir(&run_id).join("run.out");
            let timer_path = if out_path.exists() { out_path } else { run_dir(&run_id) };
            let secs = elapsed_secs(&timer_path).unwrap_or(0);
            let elapsed = fmt_elapsed(secs);
            rows.push(Row {
                group: 0,
                sort_key: secs,
                line: format!("  [busy]  {id_s}  run {run_s}  {elapsed} elapsed"),
            });
        } else if let Some(run_id) = session_latest_run(id) {
            n_idle += 1;
            // Accumulate tokens for the header total regardless of filter flags.
            let tok_tag = if let Some((inp, out)) = run_token_usage(&run_id) {
                total_input  += inp;
                total_output += out;
                format!("  {}↑ {}↓", fmt_tokens(inp), fmt_tokens(out))
            } else {
                String::new()
            };
            if only_busy { continue; }
            let run_s = short(&run_id);
            let done_path = run_dir(&run_id).join("run.done");
            let secs = elapsed_secs(&done_path).unwrap_or(u64::MAX);
            let ago = if secs == u64::MAX {
                String::new()
            } else {
                format!("  {}  ago", fmt_elapsed(secs))
            };
            let tmux_tag = if tmux_alive { "" } else { "  [tmux gone]" };
            rows.push(Row {
                group: 1,
                sort_key: secs,
                line: format!("  [idle]  {id_s}  last run {run_s}{ago}{tok_tag}{tmux_tag}"),
            });
        } else {
            if only_busy || only_idle { continue; }
            rows.push(Row {
                group: 2,
                sort_key: 0,
                line: format!("  [new]   {id_s}  (no turns yet)"),
            });
        }
    }

    // busy first (longest-running at top), then idle (most recent at top), then new
    rows.sort_by_key(|r| (r.group, r.sort_key));

    let total = session_ids.len();
    print!("{total} session{}", if total == 1 { "" } else { "s" });
    if n_busy > 0 || n_idle > 0 {
        print!("  ({n_busy} busy, {n_idle} idle)");
    }
    if total_input > 0 || total_output > 0 {
        print!("  · {}↑ {}↓ tokens", fmt_tokens(total_input), fmt_tokens(total_output));
    }
    println!();
    println!();
    for row in &rows {
        println!("{}", row.line);
    }

    Ok(())
}

fn cmd_repo_ls() -> anyhow::Result<()> {
    let cfg = load_config();
    if cfg.repos.is_empty() {
        println!("no repos saved — use: mozart repo set <path>");
        return Ok(());
    }
    for (i, repo) in cfg.repos.iter().enumerate() {
        let marker = if i == cfg.active { "* " } else { "  " };
        println!("{marker}{}: {repo}", i + 1);
    }
    Ok(())
}

fn cmd_repo_set(path: &str) -> anyhow::Result<()> {
    let canonical = std::fs::canonicalize(path)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.to_string());

    let mut cfg = load_config();
    let idx = if let Some(pos) = cfg.repos.iter().position(|r| r == &canonical) {
        pos
    } else {
        cfg.repos.push(canonical.clone());
        cfg.repos.len() - 1
    };
    cfg.active = idx;
    save_config(&cfg)?;
    eprintln!("· active repo set to: {canonical}");
    Ok(())
}

fn cmd_repo_use(n: usize) -> anyhow::Result<()> {
    let mut cfg = load_config();
    if n == 0 || n > cfg.repos.len() {
        anyhow::bail!("no repo #{n} — run `mozart repo ls` to see options");
    }
    cfg.active = n - 1;
    save_config(&cfg)?;
    eprintln!("· active repo: {}", cfg.repos[cfg.active]);
    Ok(())
}

fn cmd_session_ls() -> anyhow::Result<()> {
    let cfg = load_config();
    let ids = sessions_all_ids()?;
    if ids.is_empty() {
        println!("no sessions — run: mozart new");
        return Ok(());
    }
    for (i, id) in ids.iter().enumerate() {
        let marker = if cfg.active_session.as_deref() == Some(id.as_str()) { "*" } else { " " };
        let status = if session_busy_run(id).is_some() {
            "[busy]"
        } else if session_latest_run(id).is_some() {
            "[idle]"
        } else {
            "[new] "
        };
        let repo = session_repo(id).unwrap_or_else(|| "(no repo)".to_string());
        println!("{marker} {}: {}…  {}  {}", i + 1, &id[..8], status, repo);
    }
    Ok(())
}

fn cmd_session_use(n: usize) -> anyhow::Result<()> {
    let ids = sessions_all_ids()?;
    if n == 0 || n > ids.len() {
        anyhow::bail!("no session #{n} — run `mozart session ls` to see options");
    }
    let session_id = ids[n - 1].clone();
    let mut cfg = load_config();
    cfg.active_session = Some(session_id.clone());
    save_config(&cfg)?;
    let repo = session_repo(&session_id).unwrap_or_else(|| "(no repo)".to_string());
    eprintln!("· active session: {}…  {}", &session_id[..8], repo);
    Ok(())
}

fn plan_repo(plan_id: &str) -> Option<String> {
    fs::read_to_string(plan_dir(plan_id).join("repo.txt"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn cmd_plan_new(goal: &str) -> anyhow::Result<()> {
    let plan_id = Uuid::new_v4().to_string();
    fs::create_dir_all(plan_dir(&plan_id))?;
    fs::write(plan_dir(&plan_id).join("goal.txt"), goal)?;

    // Snapshot the active repo at plan-creation time so dispatch always uses
    // the right working directory regardless of what's active later.
    let cfg = load_config();
    let repo_path = active_repo(&cfg)
        .map(|s| s.to_string())
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default().to_string_lossy().into_owned());
    fs::write(plan_dir(&plan_id).join("repo.txt"), &repo_path)?;
    eprintln!("· repo: {repo_path}");

    let prompt = format!(
        "Decompose the following goal into small, isolated coding tasks.\n\
         Each task must be independently executable by a single coding agent in its own \
         session with no dependency on any other task completing first.\n\n\
         Goal: {goal}\n\n\
         Rules:\n\
         1. Each task is scoped to a single concern and can be done in isolation.\n\
         2. Tasks must be concrete and actionable — specific enough for a coding agent \
            to start immediately without asking clarifying questions.\n\
         3. Your ENTIRE response must be a JSON array. No markdown, no code fences, \
            no preamble, no trailing text of any kind.\n\
         4. Each element has exactly these fields:\n\
            - \"title\": short task name (5-10 words)\n\
            - \"description\": what to do and how to verify it worked (2-4 sentences)\n\
            - \"context\": background, constraints, or conventions the agent needs\n\
            - \"depends_on\": array of 1-indexed task numbers this task requires completed \
              first (use [] if none)\n\n\
         Output only the raw JSON array, starting with [ and ending with ].",
        goal = goal
    );

    eprintln!("· calling claude...");

    let mut child = Command::new("claude")
        .args(["-p", "--output-format", "stream-json", "--verbose", "--permission-mode", "plan", &prompt])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| anyhow::anyhow!("failed to run claude: {e}"))?;

    let stdout = child.stdout.take().expect("stdout was piped");
    let reader = BufReader::new(stdout);

    let mut full_text = String::new();
    let mut result_meta: Option<serde_json::Value> = None;
    let mut connected = false;

    for line in reader.lines() {
        let line = line?;
        if line.is_empty() { continue; }
        if let Ok(event) = serde_json::from_str::<serde_json::Value>(&line) {
            match event["type"].as_str() {
                Some("system") => {
                    if !connected {
                        eprintln!("· connected, waiting for response...");
                        connected = true;
                    }
                }
                Some("assistant") => {
                    if let Some(content) = event["message"]["content"].as_array() {
                        for block in content {
                            if let Some(text) = block["text"].as_str() {
                                full_text.push_str(text);
                            }
                        }
                    }
                }
                Some("result") => {
                    result_meta = Some(event);
                }
                _ => {}
            }
        }
    }

    let status = child.wait()?;
    if !status.success() {
        anyhow::bail!("claude exited non-zero");
    }

    if let Some(meta) = &result_meta {
        if meta["is_error"].as_bool() == Some(true) {
            anyhow::bail!("claude error: {}", meta["result"]);
        }
    }

    // Fall back to result field if assistant text was empty
    let result_text = if !full_text.is_empty() {
        full_text.clone()
    } else {
        result_meta.as_ref()
            .and_then(|m| m["result"].as_str())
            .unwrap_or("")
            .to_string()
    };

    let tasks: Vec<Task> = serde_json::from_str(&result_text).map_err(|e| {
        let raw_path = plan_dir(&plan_id).join("raw_response.txt");
        let _ = fs::write(&raw_path, &result_text);
        anyhow::anyhow!(
            "claude response was not a valid JSON task array: {e}\n\
             raw response saved to {}\n\
             tip: try rephrasing the goal or re-running `mozart plan new`",
            raw_path.display()
        )
    })?;

    fs::write(plan_dir(&plan_id).join("tasks.json"), serde_json::to_string_pretty(&tasks)?)?;

    eprintln!();
    eprintln!("· {} tasks:", tasks.len());
    for (i, task) in tasks.iter().enumerate() {
        eprintln!("  {}. {}", i + 1, task.title);
    }
    eprintln!();
    if let Some(meta) = &result_meta {
        if let Some(cost) = meta["total_cost_usd"].as_f64() {
            eprintln!("· cost: ${cost:.4}");
        }
    }
    eprintln!();
    // stdout only — this is what PLAN=$(...) captures
    println!("{}", plan_id);
    Ok(())
}

fn cmd_plan_ls() -> anyhow::Result<()> {
    let dir = plans_dir();
    if !dir.exists() {
        println!("no plans — use: mozart plan new \"<goal>\"");
        return Ok(());
    }
    let mut entries: Vec<_> = fs::read_dir(&dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .collect();
    entries.sort_by_key(|e| e.file_name());
    if entries.is_empty() {
        println!("no plans — use: mozart plan new \"<goal>\"");
        return Ok(());
    }
    for entry in entries {
        let plan_id = entry.file_name().to_string_lossy().into_owned();
        let goal = fs::read_to_string(entry.path().join("goal.txt"))
            .unwrap_or_default()
            .trim()
            .to_string();
        let tasks_raw = fs::read_to_string(entry.path().join("tasks.json")).unwrap_or_default();
        let task_count = serde_json::from_str::<Vec<Task>>(&tasks_raw)
            .map(|v| v.len())
            .unwrap_or(0);
        let short_goal = if goal.len() > 60 {
            format!("{}…", &goal[..60])
        } else {
            goal
        };
        println!("  {}  {}  ({} tasks)", plan_id, short_goal, task_count);
    }
    Ok(())
}

fn cmd_plan_show(plan_id: &str) -> anyhow::Result<()> {
    let dir = plan_dir(plan_id);
    if !dir.exists() {
        anyhow::bail!("plan {} not found — run `mozart plan ls`", plan_id);
    }
    let goal = fs::read_to_string(dir.join("goal.txt"))?.trim().to_string();
    let tasks: Vec<Task> = serde_json::from_str(&fs::read_to_string(dir.join("tasks.json"))?)?;
    let short_id = &plan_id[..8.min(plan_id.len())];
    println!("Plan: {short_id}…");
    println!("Goal: {goal}");
    if let Some(repo) = plan_repo(plan_id) {
        println!("Repo: {repo}");
    }
    println!();
    for (i, task) in tasks.iter().enumerate() {
        println!("{}. {}", i + 1, task.title);
        println!("   {}", task.description);
        if !task.context.is_empty() {
            println!("   context: {}", task.context);
        }
        if !task.depends_on.is_empty() {
            let deps: Vec<String> = task.depends_on.iter().map(|n| format!("task {n}")).collect();
            println!("   depends on: {}", deps.join(", "));
        }
        println!();
    }
    Ok(())
}

fn cmd_plan_dispatch(plan_id: &str, task_num: usize, session_id: Option<&str>) -> anyhow::Result<()> {
    let dir = plan_dir(plan_id);
    if !dir.exists() {
        anyhow::bail!("plan {} not found — run `mozart plan ls`", plan_id);
    }
    let tasks: Vec<Task> = serde_json::from_str(&fs::read_to_string(dir.join("tasks.json"))?)?;
    if task_num == 0 || task_num > tasks.len() {
        anyhow::bail!(
            "task {} not found — plan has {} task{} (1-indexed)",
            task_num,
            tasks.len(),
            if tasks.len() == 1 { "" } else { "s" }
        );
    }
    let task = &tasks[task_num - 1];
    let message = build_task_message(task, task_num, &tasks);

    let resolved_sid = match session_id {
        Some(sid) => sid.to_string(),
        None => {
            eprintln!("· no session specified — creating a new one");
            let repo = plan_repo(plan_id).unwrap_or_else(|| ".".to_string());
            let sid = create_session(&repo)?;
            let mut cfg = load_config();
            cfg.active_session = Some(sid.clone());
            save_config(&cfg)?;
            sid
        }
    };

    eprintln!(
        "· dispatching task {} ({}) to session {}…",
        task_num,
        task.title,
        &resolved_sid[..8.min(resolved_sid.len())]
    );
    cmd_send(&resolved_sid, &message, true)?;
    save_dispatch_record(plan_id, DispatchRecord {
        task_num,
        title: task.title.clone(),
        session_id: resolved_sid,
    })?;
    Ok(())
}

fn build_task_message(task: &Task, task_num: usize, all_tasks: &[Task]) -> String {
    let mut msg = format!(
        "Task {task_num}: {}\n\n{}\n\nContext:\n{}",
        task.title, task.description, task.context
    );
    if !task.depends_on.is_empty() {
        msg.push_str("\n\nThis task depends on the following tasks being completed first:");
        for dep in &task.depends_on {
            if let Some(dep_task) = all_tasks.get(dep - 1) {
                msg.push_str(&format!("\n  - Task {dep}: {}", dep_task.title));
            }
        }
    }
    msg.push_str(
        "\n\nWhen complete:\n\
         - Commit your changes using conventional commits.\n\
         - Format: <type>(<optional scope>): <description>\n\
         - Types: feat, fix, chore, refactor, docs, test, style\n\
         - Example: feat(auth): add token refresh on 401 response\n\
         - One commit per logical change; do not bundle unrelated changes."
    );
    msg
}

fn cmd_plan_dispatch_all(plan_id: &str) -> anyhow::Result<()> {
    let dir = plan_dir(plan_id);
    if !dir.exists() {
        anyhow::bail!("plan {} not found — run `mozart plan ls`", plan_id);
    }
    let tasks: Vec<Task> = serde_json::from_str(&fs::read_to_string(dir.join("tasks.json"))?)?;
    if tasks.is_empty() {
        anyhow::bail!("plan has no tasks");
    }

    let repo = plan_repo(plan_id).unwrap_or_else(|| ".".to_string());
    eprintln!("· repo: {repo}");
    eprintln!("· dispatching {} tasks to {} sessions…", tasks.len(), tasks.len());
    eprintln!();

    let tasks_clone = tasks.clone();
    for (i, task) in tasks_clone.iter().enumerate() {
        let task_num = i + 1;
        let session_id = create_session(&repo)?;
        let message = build_task_message(task, task_num, &tasks);
        eprintln!(
            "  task {task_num}  {}…  {}",
            &session_id[..8],
            task.title
        );
        cmd_send(&session_id, &message, true)?;
        save_dispatch_record(plan_id, DispatchRecord {
            task_num,
            title: task.title.clone(),
            session_id,
        })?;
    }

    eprintln!();
    eprintln!("· all tasks dispatched — monitor with: mozart status");
    eprintln!("· review results when done: mozart plan review {plan_id}");
    Ok(())
}

fn cmd_plan_review(plan_id: &str) -> anyhow::Result<()> {
    let dir = plan_dir(plan_id);
    if !dir.exists() {
        anyhow::bail!("plan {} not found — run `mozart plan ls`", plan_id);
    }
    let goal = fs::read_to_string(dir.join("goal.txt"))?.trim().to_string();
    let tasks: Vec<Task> = serde_json::from_str(&fs::read_to_string(dir.join("tasks.json"))?)?;
    let records = load_dispatch_records(plan_id);

    println!("Plan: {}…", &plan_id[..8.min(plan_id.len())]);
    println!("Goal: {goal}");
    println!();

    let mut n_done = 0usize;
    let mut n_busy = 0usize;
    let mut n_error = 0usize;
    let mut n_skipped = 0usize;
    let mut error_details: Vec<(usize, String, String)> = Vec::new(); // (task_num, title, detail)

    for (i, task) in tasks.iter().enumerate() {
        let task_num = i + 1;
        let record = records.iter().find(|r| r.task_num == task_num);

        let Some(rec) = record else {
            n_skipped += 1;
            println!("  [ -    ]  {task_num}. {}", task.title);
            continue;
        };

        let sid = &rec.session_id;
        let short_sid = &sid[..8.min(sid.len())];

        if let Some(_busy_run) = session_busy_run(sid) {
            n_busy += 1;
            println!("  [ busy ]  {task_num}. {}  (session {short_sid}…)", task.title);
            continue;
        }

        let Some(run_id) = session_latest_run(sid) else {
            n_skipped += 1;
            println!("  [ -    ]  {task_num}. {}  (session {short_sid}… — no run)", task.title);
            continue;
        };

        let rdir = run_dir(&run_id);
        let exit_code: Option<i32> = fs::read_to_string(rdir.join("run.exit"))
            .ok()
            .and_then(|s| s.trim().parse().ok());

        let stdout = fs::read_to_string(rdir.join("run.out")).unwrap_or_default();
        let parsed: Option<serde_json::Value> = serde_json::from_str(&stdout).ok();
        let is_error = parsed.as_ref()
            .and_then(|v| v["is_error"].as_bool())
            .unwrap_or(false);
        let cost = parsed.as_ref()
            .and_then(|v| v["total_cost_usd"].as_f64());

        let cost_str = cost.map(|c| format!("  ${c:.4}")).unwrap_or_default();

        if exit_code == Some(0) && !is_error {
            n_done += 1;
            println!("  [ done ]  {task_num}. {}  (session {short_sid}…){cost_str}", task.title);
        } else {
            n_error += 1;
            let stderr = fs::read_to_string(rdir.join("run.err")).unwrap_or_default();
            let result_snippet = parsed.as_ref()
                .and_then(|v| v["result"].as_str())
                .unwrap_or("")
                .lines()
                .take(3)
                .collect::<Vec<_>>()
                .join(" ");
            let detail = if !stderr.trim().is_empty() {
                stderr.lines().last().unwrap_or("").trim().to_string()
            } else if !result_snippet.is_empty() {
                result_snippet
            } else {
                format!("exit {}", exit_code.unwrap_or(-1))
            };
            println!(
                "  [error ]  {task_num}. {}  (session {short_sid}…)  exit {}",
                task.title,
                exit_code.unwrap_or(-1)
            );
            error_details.push((task_num, task.title.clone(), detail));
        }
    }

    println!();
    print!("{n_done} done");
    if n_busy   > 0 { print!("  {n_busy} busy"); }
    if n_error  > 0 { print!("  {n_error} error"); }
    if n_skipped > 0 { print!("  {n_skipped} not dispatched"); }
    println!();

    if !error_details.is_empty() {
        println!();
        println!("ERRORS");
        for (num, title, detail) in &error_details {
            println!();
            println!("  task {num}: {title}");
            println!("  {detail}");
            // show last few lines of stderr for more context
            let rec = records.iter().find(|r| r.task_num == *num);
            if let Some(rec) = rec {
                if let Some(run_id) = session_latest_run(&rec.session_id) {
                    let stderr = fs::read_to_string(run_dir(&run_id).join("run.err"))
                        .unwrap_or_default();
                    let lines: Vec<&str> = stderr.lines()
                        .filter(|l| !l.trim().is_empty())
                        .collect();
                    for line in lines.iter().rev().take(5).rev() {
                        println!("    {line}");
                    }
                }
            }
        }
    }

    Ok(())
}

fn cmd_queue_new(plan_id: &str) -> anyhow::Result<()> {
    let dir = plan_dir(plan_id);
    if !dir.exists() {
        anyhow::bail!("plan {} not found — run `mozart plan ls`", plan_id);
    }
    let goal = fs::read_to_string(dir.join("goal.txt"))?.trim().to_string();
    let repo = fs::read_to_string(dir.join("repo.txt"))
        .unwrap_or_default().trim().to_string();
    let tasks: Vec<Task> = serde_json::from_str(&fs::read_to_string(dir.join("tasks.json"))?)?;

    let queue_id = Uuid::new_v4().to_string();
    fs::create_dir_all(queue_dir(&queue_id))?;

    let meta = QueueMeta { plan_id: plan_id.to_string(), goal: goal.clone(), repo };
    fs::write(queue_meta_path(&queue_id), serde_json::to_string_pretty(&meta)?)?;

    let items: Vec<QueueItem> = tasks.iter().enumerate().map(|(i, t)| QueueItem {
        item_num: i + 1,
        plan_id: plan_id.to_string(),
        task_num: i + 1,
        title: t.title.clone(),
        depends_on: t.depends_on.clone(),
        status: QueueStatus::Pending,
        session_id: None,
    }).collect();
    save_queue_items(&queue_id, &items)?;

    eprintln!("· queue created from plan {}…", &plan_id[..8.min(plan_id.len())]);
    eprintln!("· goal: {goal}");
    eprintln!("· {} items:", items.len());
    for item in &items {
        if item.depends_on.is_empty() {
            eprintln!("  {}. {}", item.item_num, item.title);
        } else {
            let deps: Vec<String> = item.depends_on.iter().map(|n| n.to_string()).collect();
            eprintln!("  {}. {}  → after: {}", item.item_num, item.title, deps.join(", "));
        }
    }
    eprintln!();
    println!("{}", queue_id);
    Ok(())
}

fn cmd_queue_ls() -> anyhow::Result<()> {
    let dir = queues_dir();
    if !dir.exists() {
        println!("no queues — use: mozart queue new <plan-id>");
        return Ok(());
    }
    let mut entries: Vec<_> = fs::read_dir(&dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .collect();
    entries.sort_by_key(|e| e.file_name());
    if entries.is_empty() {
        println!("no queues — use: mozart queue new <plan-id>");
        return Ok(());
    }
    for entry in entries {
        let qid = entry.file_name().to_string_lossy().into_owned();
        let meta = load_queue_meta(&qid).ok();
        let goal = meta.as_ref().map(|m| m.goal.as_str()).unwrap_or("(unknown)");
        let short_goal = if goal.len() > 50 { format!("{}…", &goal[..50]) } else { goal.to_string() };
        let items = load_queue_items(&qid).unwrap_or_default();
        let n_done = items.iter().filter(|i| i.status == QueueStatus::Done).count();
        let n_failed = items.iter().filter(|i| i.status == QueueStatus::Failed).count();
        let total = items.len();
        let progress = if n_failed > 0 {
            format!("{n_done} done, {n_failed} failed / {total}")
        } else {
            format!("{n_done}/{total} done")
        };
        println!("  {}…  {}  ({})", &qid[..8.min(qid.len())], short_goal, progress);
    }
    Ok(())
}

fn cmd_queue_show(queue_id: &str) -> anyhow::Result<()> {
    if !queue_dir(queue_id).exists() {
        anyhow::bail!("queue {} not found — run `mozart queue ls`", queue_id);
    }
    let meta = load_queue_meta(queue_id)?;
    let items = load_queue_items(queue_id)?;

    println!("Queue: {}…", &queue_id[..8.min(queue_id.len())]);
    println!("Goal:  {}", meta.goal);
    println!("Repo:  {}", meta.repo);
    println!();

    for item in &items {
        let status_tag = match item.status {
            QueueStatus::Pending => "[pending]",
            QueueStatus::Running => "[running]",
            QueueStatus::Done    => "[done   ]",
            QueueStatus::Failed  => "[failed ]",
        };
        let deps_str = if item.depends_on.is_empty() {
            String::new()
        } else {
            let deps: Vec<String> = item.depends_on.iter().map(|n| format!("{n}")).collect();
            format!("  → after: {}", deps.join(", "))
        };
        let suffix = match &item.session_id {
            Some(sid) if item.status == QueueStatus::Running => {
                format!("  (session {}…)", &sid[..8.min(sid.len())])
            }
            Some(sid) if item.status == QueueStatus::Done || item.status == QueueStatus::Failed => {
                // show cost or exit code
                let cost_str = session_latest_run(sid)
                    .and_then(|run_id| fs::read_to_string(run_dir(&run_id).join("run.out")).ok())
                    .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                    .and_then(|v| v["total_cost_usd"].as_f64())
                    .map(|c| format!("  ${c:.4}"))
                    .unwrap_or_default();
                cost_str
            }
            _ => String::new(),
        };
        println!("  {status_tag}  {}. {}{}{}", item.item_num, item.title, deps_str, suffix);
    }

    println!();
    let n_done    = items.iter().filter(|i| i.status == QueueStatus::Done).count();
    let n_running = items.iter().filter(|i| i.status == QueueStatus::Running).count();
    let n_pending = items.iter().filter(|i| i.status == QueueStatus::Pending).count();
    let n_failed  = items.iter().filter(|i| i.status == QueueStatus::Failed).count();
    print!("{}/{} done", n_done, items.len());
    if n_running > 0 { print!("  {n_running} running"); }
    if n_pending > 0 { print!("  {n_pending} pending"); }
    if n_failed  > 0 { print!("  {n_failed} failed"); }
    println!();
    Ok(())
}

fn cmd_queue_run(queue_id: &str) -> anyhow::Result<()> {
    use std::collections::HashSet;

    if !queue_dir(queue_id).exists() {
        anyhow::bail!("queue {} not found — run `mozart queue ls`", queue_id);
    }
    let meta = load_queue_meta(queue_id)?;
    eprintln!("· queue: {}", meta.goal);
    eprintln!("· repo:  {}", meta.repo);
    eprintln!();

    // Load all tasks from the plan once (for building messages)
    let all_tasks: Vec<Task> = fs::read_to_string(plan_dir(&meta.plan_id).join("tasks.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();

    let mut wave = 0usize;

    loop {
        let mut items = load_queue_items(queue_id)?;

        // Check running items for completion
        let mut any_completed = false;
        for item in items.iter_mut().filter(|i| i.status == QueueStatus::Running) {
            let Some(sid) = &item.session_id else { continue };
            if session_busy_run(sid).is_some() { continue }  // still running

            // Finished — read exit code
            let exit_code: i32 = session_latest_run(sid)
                .and_then(|run_id| fs::read_to_string(run_dir(&run_id).join("run.exit")).ok())
                .and_then(|s| s.trim().parse().ok())
                .unwrap_or(-1);

            let cost_str = session_latest_run(sid)
                .and_then(|run_id| fs::read_to_string(run_dir(&run_id).join("run.out")).ok())
                .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                .and_then(|v| v["total_cost_usd"].as_f64())
                .map(|c| format!("${c:.4}"))
                .unwrap_or_else(|| format!("exit {exit_code}"));

            if exit_code == 0 {
                item.status = QueueStatus::Done;
                eprintln!("  ✓  task {}: {}  ({})", item.item_num, item.title, cost_str);
            } else {
                item.status = QueueStatus::Failed;
                eprintln!("  ✗  task {}: {}  ({})", item.item_num, item.title, cost_str);
            }
            any_completed = true;
        }
        if any_completed {
            save_queue_items(queue_id, &items)?;
        }

        let n_pending = items.iter().filter(|i| i.status == QueueStatus::Pending).count();
        let n_running = items.iter().filter(|i| i.status == QueueStatus::Running).count();
        let n_done    = items.iter().filter(|i| i.status == QueueStatus::Done).count();
        let n_failed  = items.iter().filter(|i| i.status == QueueStatus::Failed).count();

        if n_pending == 0 && n_running == 0 {
            break;
        }

        // Find items ready to dispatch: pending + all deps are Done
        let done_nums: HashSet<usize> = items.iter()
            .filter(|i| i.status == QueueStatus::Done)
            .map(|i| i.item_num)
            .collect();

        let ready_nums: Vec<usize> = items.iter()
            .filter(|i| i.status == QueueStatus::Pending
                && i.depends_on.iter().all(|d| done_nums.contains(d)))
            .map(|i| i.item_num)
            .collect();

        if ready_nums.is_empty() && n_running == 0 {
            eprintln!();
            eprintln!("· stuck: {n_pending} pending task{} blocked by failed dependencies",
                if n_pending == 1 { "" } else { "s" });
            break;
        }

        if !ready_nums.is_empty() {
            wave += 1;
            eprintln!("  wave {wave}: dispatching {} task{}",
                ready_nums.len(), if ready_nums.len() == 1 { "" } else { "s" });

            for &num in &ready_nums {
                let item = items.iter_mut().find(|i| i.item_num == num).unwrap();
                let task = all_tasks.get(item.task_num - 1);
                let message = match task {
                    Some(t) => build_task_message(t, item.item_num, &all_tasks),
                    None    => format!("Task {}: {}", item.item_num, item.title),
                };
                let session_id = create_session(&meta.repo)?;
                cmd_send(&session_id, &message, true)?;
                eprintln!("  → {}…  task {}: {}", &session_id[..8], item.item_num, item.title);
                item.status = QueueStatus::Running;
                item.session_id = Some(session_id);
            }
            save_queue_items(queue_id, &items)?;
        }

        eprintln!("  [polling — {} running, {} pending, {} done, {} failed]",
            n_running + ready_nums.len(), n_pending.saturating_sub(ready_nums.len()),
            n_done, n_failed);
        thread::sleep(Duration::from_secs(10));
    }

    // Final summary
    let items = load_queue_items(queue_id)?;
    let n_done   = items.iter().filter(|i| i.status == QueueStatus::Done).count();
    let n_failed = items.iter().filter(|i| i.status == QueueStatus::Failed).count();
    let total_cost: f64 = items.iter()
        .filter_map(|i| i.session_id.as_deref())
        .filter_map(|sid| session_latest_run(sid))
        .filter_map(|run_id| fs::read_to_string(run_dir(&run_id).join("run.out")).ok())
        .filter_map(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .filter_map(|v| v["total_cost_usd"].as_f64())
        .sum();

    eprintln!();
    eprintln!("· {n_done}/{} done{}",
        items.len(),
        if n_failed > 0 { format!("  {n_failed} failed") } else { String::new() });
    eprintln!("· total cost: ${total_cost:.4}");
    Ok(())
}

fn cmd_guide() {
    println!("TYPICAL WORKFLOW");
    println!();
    println!("  mozart repo set ~/path/to/repo   # one-time setup");
    println!("  SESSION=$(mozart new)             # uses active repo");
    println!("  RUN=$(mozart send $SESSION \"your message\")");
    println!("  mozart wait $RUN");
    println!();
    println!("  # follow-up turns automatically use --resume");
    println!("  RUN=$(mozart send $SESSION \"follow-up question\")");
    println!("  mozart wait $RUN");
    println!();
    println!("  # toggle between repos");
    println!("  mozart repo ls");
    println!("  mozart repo use 2");
    println!("  SESSION=$(mozart new)");
    println!();
    println!("  # toggle between sessions (no UUID needed after session use)");
    println!("  mozart session ls");
    println!("  mozart session use 2");
    println!("  mozart send \"follow-up question\"   # uses active session");
    println!("  mozart wait                         # uses active session's latest run");
    println!();
    println!("COMMANDS");
    println!();
    println!("  new [dir]            mint a session ID and start its tmux session");
    println!("                       uses active repo from config if dir is omitted");
    println!("  send [id] <msg>      dispatch a turn, print the run ID");
    println!("                       id optional if active session is set");
    println!("    --bypass           allow the agent to edit files and run commands");
    println!("  wait [run-id]        block until done, print the agent reply + digest");
    println!("                       run-id optional if active session is set");
    println!("    --json             print the full raw JSON payload instead");
    println!("  attach [id]          drop into the live tmux pane  (detach: Ctrl-b d)");
    println!("  cancel [id]          send C-c to interrupt a running turn");
    println!("  cat <run-id>         print raw output without waiting");
    println!("  status               high-level view: busy / idle / new sessions");
    println!("  ls                   list sessions and their tmux status");
    println!("  kill <id>            kill the tmux session and remove session state");
    println!("  kill-all             kill all mozart tmux sessions and remove all state");
    println!("  guide                print this cheatsheet");
    println!("  repo ls              list saved repos (* = active)");
    println!("  repo set <path>      add a repo and make it active");
    println!("  repo use <n>         switch the active repo by number");
    println!("  session ls           list all sessions (* = active), with repo and status");
    println!("  session use <n>      set the active session by number");
    println!("  plan new \"<goal>\"     call claude to decompose goal into tasks, print plan ID");
    println!("  plan ls              list all plans");
    println!("  plan show <id>       print numbered task list");
    println!("  plan dispatch <id> <task-num>          send that task to a new session");
    println!("  plan dispatch <id> <task-num> <sid>   send that task to an existing session");
    println!("  plan dispatch-all <id>                 dispatch all tasks, one session each");
    println!("STATE  (~/.mozart/cli/)");
    println!();
    println!("  sessions/<id>        presence means the session has had at least one turn");
    println!("                       first turn uses --session-id, subsequent use --resume");
    println!("  runs/<run-id>/       one directory per turn");
    println!("    run.out            claude stdout (JSON)");
    println!("    run.err            claude stderr");
    println!("    run.exit           exit code");
    println!("    run.done           sentinel — appears when the run is complete");
    println!("  plan review <id>                       show status/errors for dispatched tasks");
    println!("  queue new <plan-id>                    create queue from plan (respects deps)");
    println!("  queue ls                               list queues with progress");
    println!("  queue show <queue-id>                  show items and current status");
    println!("  queue run <queue-id>                   dispatch wave by wave, blocking");
    println!();
    println!("QUEUE WORKFLOW");
    println!();
    println!("  PLAN=$(mozart plan new \"refactor auth module\")");
    println!("  QUEUE=$(mozart queue new $PLAN)        # create queue from plan");
    println!("  mozart queue show $QUEUE               # preview items and dep order");
    println!("  mozart queue run $QUEUE                # blocking: wave-by-wave dispatch");
    println!("  mozart queue show $QUEUE               # review final status");
    println!();
    println!("STATE  (~/.mozart/cli/)");
    println!();
    println!("  sessions/<id>        presence means the session has had at least one turn");
    println!("                       first turn uses --session-id, subsequent use --resume");
    println!("  runs/<run-id>/       one directory per turn");
    println!("    run.out            claude stdout (JSON)");
    println!("    run.err            claude stderr");
    println!("    run.exit           exit code");
    println!("    run.done           sentinel — appears when the run is complete");
    println!("  plans/<plan-id>/     one directory per decomposed goal");
    println!("    goal.txt           the original goal string");
    println!("    repo.txt           repo path snapshotted at plan-creation time");
    println!("    tasks.json         JSON array of tasks from decomposition");
    println!("  queues/<queue-id>/   one directory per queue");
    println!("    meta.json          goal, plan_id, repo");
    println!("    items.json         task items with live status (pending/running/done/failed)");
    println!();
    println!("TMUX");
    println!();
    println!("  each session maps to a tmux session named mozart-<id>");
    println!("  the agent process runs inside it — attach to watch it live");
    println!("  killing the tmux session ends the process; state files remain on disk");
}

fn main() {
    let cli = Cli::parse();
    let result = match &cli.command {
        Cmd::New { working_dir } => cmd_new(working_dir),
        Cmd::Send { session_id, message, bypass } => match message {
            Some(msg) => cmd_send(session_id, msg, *bypass),
            None => {
                let cfg = load_config();
                match active_session_id(&cfg) {
                    Some(sid) => cmd_send(sid, session_id, *bypass),
                    None => {
                        eprintln!("error: no active session\n  set one:  mozart session use <n>\n  list:     mozart session ls");
                        std::process::exit(1);
                    }
                }
            }
        },
        Cmd::Wait { run_id, json } => (|| {
            let rid: String = match run_id {
                Some(id) => id.clone(),
                None => {
                    let cfg = load_config();
                    let sid = active_session_id(&cfg)
                        .ok_or_else(|| anyhow::anyhow!("no active session — set one: mozart session use <n>"))?;
                    session_latest_run(sid)
                        .ok_or_else(|| anyhow::anyhow!("active session has no runs yet"))?
                }
            };
            cmd_wait(&rid, *json)
        })(),
        Cmd::Attach { session_id } => (|| {
            let sid: String = match session_id {
                Some(id) => id.clone(),
                None => {
                    let cfg = load_config();
                    active_session_id(&cfg)
                        .ok_or_else(|| anyhow::anyhow!("no active session — set one: mozart session use <n>"))?
                        .to_string()
                }
            };
            cmd_attach(&sid)
        })(),
        Cmd::Cancel { session_id } => (|| {
            let sid: String = match session_id {
                Some(id) => id.clone(),
                None => {
                    let cfg = load_config();
                    active_session_id(&cfg)
                        .ok_or_else(|| anyhow::anyhow!("no active session — set one: mozart session use <n>"))?
                        .to_string()
                }
            };
            cmd_cancel(&sid)
        })(),
        Cmd::Cat { run_id }              => cmd_cat(run_id),
        Cmd::Ls                          => cmd_ls(),
        Cmd::Kill { session_id }         => cmd_kill(session_id),
        Cmd::KillAll                     => cmd_kill_all(),
        Cmd::Cost                        => cmd_cost(),
        Cmd::Status { full, busy, idle } => cmd_status(*full, *busy, *idle),
        Cmd::Guide                       => { cmd_guide(); Ok(()) },
        Cmd::Repo { action } => match action {
            RepoCmd::Ls           => cmd_repo_ls(),
            RepoCmd::Set { path } => cmd_repo_set(path),
            RepoCmd::Use { n }    => cmd_repo_use(*n),
        },
        Cmd::Session { action } => match action {
            SessionCmd::Ls        => cmd_session_ls(),
            SessionCmd::Use { n } => cmd_session_use(*n),
        },
        Cmd::Plan { action } => match action {
            PlanCmd::New { goal }                                      => cmd_plan_new(goal),
            PlanCmd::Ls                                                => cmd_plan_ls(),
            PlanCmd::Show { plan_id }                                  => cmd_plan_show(plan_id),
            PlanCmd::Dispatch { plan_id, task_num, session_id }        =>
                cmd_plan_dispatch(plan_id, *task_num, session_id.as_deref()),
            PlanCmd::DispatchAll { plan_id }                           =>
                cmd_plan_dispatch_all(plan_id),
            PlanCmd::Review { plan_id }                                =>
                cmd_plan_review(plan_id),
        },
        Cmd::Queue { action } => match action {
            QueueCmd::New  { plan_id }   => cmd_queue_new(plan_id),
            QueueCmd::Ls                 => cmd_queue_ls(),
            QueueCmd::Show { queue_id }  => cmd_queue_show(queue_id),
            QueueCmd::Run  { queue_id }  => cmd_queue_run(queue_id),
        },
    };
    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
