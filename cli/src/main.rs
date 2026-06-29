use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::thread;
use std::time::{Duration, SystemTime};
use uuid::Uuid;

#[derive(Parser)]
#[command(name = "mozart", about = "Bare-metal claude session manager")]
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
    /// Dispatch one message turn into a session. Prints the run ID.
    Send {
        session_id: String,
        message: String,
        /// Allow the agent to edit files and run commands
        #[arg(long)]
        bypass: bool,
    },
    /// Block until a run finishes, then print its output
    Wait {
        run_id: String,
        /// Print the full raw JSON payload instead of the reply + digest
        #[arg(long)]
        json: bool,
    },
    /// Attach your terminal to a session's tmux pane
    Attach {
        session_id: String,
    },
    /// Send C-c to interrupt whatever is running in a session
    Cancel {
        session_id: String,
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
}

fn config_path() -> PathBuf {
    cli_home().join("config.json")
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

fn cmd_new(working_dir: &str) -> anyhow::Result<()> {
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
                line: format!("  [idle]  {id_s}  last run {run_s}{ago}{tmux_tag}"),
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
    println!("COMMANDS");
    println!();
    println!("  new [dir]            mint a session ID and start its tmux session");
    println!("                       uses active repo from config if dir is omitted");
    println!("  send <id> <msg>      dispatch a turn, print the run ID");
    println!("    --bypass           allow the agent to edit files and run commands");
    println!("  wait <run-id>        block until done, print the agent reply + digest");
    println!("    --json             print the full raw JSON payload instead");
    println!("  attach <id>          drop into the live tmux pane  (detach: Ctrl-b d)");
    println!("  cancel <id>          send C-c to interrupt a running turn");
    println!("  cat <run-id>         print raw output without waiting");
    println!("  status               high-level view: busy / idle / new sessions");
    println!("  ls                   list sessions and their tmux status");
    println!("  kill <id>            kill the tmux session and remove session state");
    println!("  kill-all             kill all mozart tmux sessions and remove all state");
    println!("  guide                print this cheatsheet");
    println!("  repo ls              list saved repos (* = active)");
    println!("  repo set <path>      add a repo and make it active");
    println!("  repo use <n>         switch the active repo by number");
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
        Cmd::New { working_dir }                  => cmd_new(working_dir),
        Cmd::Send { session_id, message, bypass } => cmd_send(session_id, message, *bypass),
        Cmd::Wait { run_id, json }                => cmd_wait(run_id, *json),
        Cmd::Attach { session_id }                => cmd_attach(session_id),
        Cmd::Cancel { session_id }                => cmd_cancel(session_id),
        Cmd::Cat { run_id }                       => cmd_cat(run_id),
        Cmd::Ls                                   => cmd_ls(),
        Cmd::Kill { session_id }                  => cmd_kill(session_id),
        Cmd::KillAll                              => cmd_kill_all(),
        Cmd::Cost                                 => cmd_cost(),
        Cmd::Status { full, busy, idle }          => cmd_status(*full, *busy, *idle),
        Cmd::Guide                                => { cmd_guide(); Ok(()) },
        Cmd::Repo { action } => match action {
            RepoCmd::Ls          => cmd_repo_ls(),
            RepoCmd::Set { path} => cmd_repo_set(path),
            RepoCmd::Use { n }   => cmd_repo_use(*n),
        },
    };
    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
