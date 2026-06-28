use clap::{Parser, Subcommand};
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::thread;
use std::time::Duration;
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
    /// Print a workflow cheatsheet
    Guide,
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

fn session_marker(session_id: &str) -> PathBuf {
    cli_home().join("sessions").join(session_id)
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
        std::env::current_dir()?.to_string_lossy().into_owned()
    } else {
        working_dir.to_string()
    };

    ensure_tmux_session(&tmux, &working_dir)?;

    eprintln!();
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

fn cmd_guide() {
    println!("TYPICAL WORKFLOW");
    println!();
    println!("  SESSION=$(mozart new ~/path/to/repo)");
    println!("  RUN=$(mozart send $SESSION \"your message\")");
    println!("  mozart wait $RUN");
    println!();
    println!("  # follow-up turns automatically use --resume");
    println!("  RUN=$(mozart send $SESSION \"follow-up question\")");
    println!("  mozart wait $RUN");
    println!();
    println!("COMMANDS");
    println!();
    println!("  new [dir]            mint a session ID and start its tmux session");
    println!("  send <id> <msg>      dispatch a turn, print the run ID");
    println!("    --bypass           allow the agent to edit files and run commands");
    println!("  wait <run-id>        block until done, print the agent reply + digest");
    println!("    --json             print the full raw JSON payload instead");
    println!("  attach <id>          drop into the live tmux pane  (detach: Ctrl-b d)");
    println!("  cancel <id>          send C-c to interrupt a running turn");
    println!("  cat <run-id>         print raw output without waiting");
    println!("  ls                   list sessions and their tmux status");
    println!("  kill <id>            kill the tmux session and remove session state");
    println!("  kill-all             kill all mozart tmux sessions and remove all state");
    println!("  guide                print this cheatsheet");
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
        Cmd::Guide                                => { cmd_guide(); Ok(()) },
    };
    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
