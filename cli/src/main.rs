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

fn tmux_name(session_id: &str) -> String {
    format!("mozart-{}", session_id)
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn ensure_tmux_session(name: &str, working_dir: &str) -> anyhow::Result<()> {
    let exists = Command::new("tmux")
        .args(["has-session", "-t", name])
        .output()?
        .status
        .success();

    if !exists {
        let status = Command::new("tmux")
            .args(["new-session", "-d", "-s", name, "-c", working_dir])
            .status()?;
        if !status.success() {
            anyhow::bail!("tmux new-session failed for {}", name);
        }
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

    let working_dir = if working_dir == "." {
        std::env::current_dir()?.to_string_lossy().into_owned()
    } else {
        working_dir.to_string()
    };

    ensure_tmux_session(&tmux_name(&session_id), &working_dir)?;
    println!("{}", session_id);
    Ok(())
}

fn cmd_send(session_id: &str, message: &str, bypass: bool) -> anyhow::Result<()> {
    let tmux = tmux_name(session_id);

    // Check that the tmux session actually exists before dispatching.
    let exists = Command::new("tmux")
        .args(["has-session", "-t", &tmux])
        .output()?
        .status
        .success();
    if !exists {
        anyhow::bail!("no tmux session for {} — did you run `mz new`?", session_id);
    }

    // First turn uses --session-id so claude names the conversation with our ID.
    // Subsequent turns use --resume to continue it.
    let is_first = !session_marker(session_id).exists();
    let session_flag = if is_first { "--session-id" } else { "--resume" };

    let permission_mode = if bypass {
        "bypassPermissions"
    } else {
        "plan"
    };

    let run_id = Uuid::new_v4().to_string();
    let dir = run_dir(&run_id);
    fs::create_dir_all(&dir)?;

    let out = shell_quote(&dir.join("run.out").to_string_lossy());
    let err = shell_quote(&dir.join("run.err").to_string_lossy());
    let exit = shell_quote(&dir.join("run.exit").to_string_lossy());
    let done = shell_quote(&dir.join("run.done").to_string_lossy());

    let invocation = format!(
        "claude -p {flag} {sid} --output-format json --permission-mode {mode} {msg}",
        flag = session_flag,
        sid = shell_quote(session_id),
        mode = permission_mode,
        msg = shell_quote(message),
    );
    let wrapped = format!(
        "{inv} > {out} 2> {err}; echo $? > {exit}; touch {done}",
        inv = invocation,
        out = out,
        err = err,
        exit = exit,
        done = done,
    );

    tmux_send(&tmux, &wrapped)?;

    // Record that this session has had at least one turn so the next send
    // uses --resume instead of --session-id.
    if is_first {
        fs::create_dir_all(cli_home().join("sessions"))?;
        fs::write(session_marker(session_id), "")?;
    }

    println!("{}", run_id);
    Ok(())
}

fn cmd_wait(run_id: &str) -> anyhow::Result<()> {
    let dir = run_dir(run_id);
    if !dir.exists() {
        anyhow::bail!("run {} not found at {}", run_id, dir.display());
    }

    let done = dir.join("run.done");
    while !done.exists() {
        thread::sleep(Duration::from_millis(500));
    }

    let exit_code: Option<i32> = fs::read_to_string(dir.join("run.exit"))
        .ok()
        .and_then(|s| s.trim().parse().ok());

    let stdout = fs::read_to_string(dir.join("run.out")).unwrap_or_default();
    let stderr = fs::read_to_string(dir.join("run.err")).unwrap_or_default();

    if exit_code != Some(0) {
        eprintln!("claude exited with {:?}", exit_code);
        if !stderr.is_empty() {
            eprintln!("{}", stderr.trim());
        }
        std::process::exit(1);
    }

    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .map_err(|e| anyhow::anyhow!("failed to parse claude output: {e}\nraw: {stdout}"))?;

    if parsed["is_error"].as_bool() == Some(true) {
        eprintln!("claude error: {}", parsed["result"]);
        std::process::exit(1);
    }

    println!("{}", parsed["result"].as_str().unwrap_or(&stdout));
    Ok(())
}

fn cmd_attach(session_id: &str) -> anyhow::Result<()> {
    let tmux = tmux_name(session_id);
    // Replace the current process with tmux attach so the terminal is fully
    // handed over — no wrapper process sitting in between.
    let err = std::os::unix::process::CommandExt::exec(
        Command::new("tmux").args(["attach-session", "-t", &tmux]),
    );
    // exec() only returns on failure.
    Err(err.into())
}

fn cmd_cancel(session_id: &str) -> anyhow::Result<()> {
    let tmux = tmux_name(session_id);
    Command::new("tmux")
        .args(["send-keys", "-t", &tmux, "C-c", ""])
        .status()?;
    eprintln!("sent C-c to {}", tmux);
    Ok(())
}

fn cmd_cat(run_id: &str) -> anyhow::Result<()> {
    let out = run_dir(run_id).join("run.out");
    if !out.exists() {
        anyhow::bail!("run {} not found", run_id);
    }
    print!("{}", fs::read_to_string(&out)?);
    Ok(())
}

fn main() {
    let cli = Cli::parse();
    let result = match &cli.command {
        Cmd::New { working_dir } => cmd_new(working_dir),
        Cmd::Send { session_id, message, bypass } => cmd_send(session_id, message, *bypass),
        Cmd::Wait { run_id } => cmd_wait(run_id),
        Cmd::Attach { session_id } => cmd_attach(session_id),
        Cmd::Cancel { session_id } => cmd_cancel(session_id),
        Cmd::Cat { run_id } => cmd_cat(run_id),
    };
    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
