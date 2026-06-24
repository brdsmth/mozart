use super::{AgentReply, RunHandle, SendOutcome};
use crate::models::Task;
use crate::tmux;
use async_trait::async_trait;
use serde::Deserialize;
use std::time::Duration;
use tokio::time::sleep;

pub struct ClaudeCliBackend;

#[derive(Deserialize)]
struct ClaudeResult {
    result: String,
    is_error: bool,
}

const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Single-quotes `s` for safe inclusion in a shell command line. Needed
/// because Step 2 reaches the `claude` invocation via `tmux send-keys`
/// (keystrokes a shell re-parses) instead of Step 1's direct
/// `Command::output()` (argv passed straight through `execve`, no shell
/// involved) — without this, a user message containing `'`, `$`, or `;`
/// could break out of its argument or run as a second shell command.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[async_trait]
impl super::AgentBackend for ClaudeCliBackend {
    async fn send(
        &self,
        task: &Task,
        user_message: &str,
        is_first_turn: bool,
        run: &RunHandle,
    ) -> anyhow::Result<SendOutcome> {
        let session_flag = if is_first_turn {
            "--session-id"
        } else {
            "--resume"
        };

        let display_args = [
            "-p",
            session_flag,
            &task.id,
            "--output-format",
            "json",
            "--permission-mode",
            &task.permission_mode,
            user_message,
        ];
        let command = format!("claude {}", display_args.join(" "));

        let shell_invocation = format!(
            "claude -p {session_flag} {task_id} --output-format json --permission-mode {mode} {message}",
            task_id = shell_quote(&task.id),
            mode = shell_quote(&task.permission_mode),
            message = shell_quote(user_message),
        );

        let run_dir = crate::db::run_dir(run.run_id);
        tokio::fs::create_dir_all(&run_dir).await?;
        let wrapped = format!(
            "{invocation} > {out} 2> {err}; echo $? > {exit}; touch {done}",
            invocation = shell_invocation,
            out = shell_quote(&run_dir.join("run.out").to_string_lossy()),
            err = shell_quote(&run_dir.join("run.err").to_string_lossy()),
            exit = shell_quote(&run_dir.join("run.exit").to_string_lossy()),
            done = shell_quote(&run_dir.join("run.done").to_string_lossy()),
        );

        let session = tmux::session_name(&task.id);
        tmux::ensure_session(&session, &task.working_dir).await?;
        tmux::send_command(&session, &wrapped).await?;

        match self.await_completion(run).await? {
            SendOutcome::Reply(mut reply) => {
                reply.command = command;
                Ok(SendOutcome::Reply(reply))
            }
            cancelled @ SendOutcome::Cancelled => Ok(cancelled),
        }
    }

    async fn await_completion(&self, run: &RunHandle) -> anyhow::Result<SendOutcome> {
        let run_dir = crate::db::run_dir(run.run_id);
        let done_path = run_dir.join("run.done");

        while !done_path.exists() {
            tokio::select! {
                _ = run.cancel.cancelled() => return Ok(SendOutcome::Cancelled),
                _ = sleep(POLL_INTERVAL) => {}
            }
        }

        let exit_code: Option<i32> = tokio::fs::read_to_string(run_dir.join("run.exit"))
            .await
            .ok()
            .and_then(|s| s.trim().parse().ok());
        let stdout = tokio::fs::read_to_string(run_dir.join("run.out")).await.unwrap_or_default();
        let stderr = tokio::fs::read_to_string(run_dir.join("run.err")).await.unwrap_or_default();

        if exit_code != Some(0) {
            anyhow::bail!("claude exited with {:?}: {}", exit_code, stderr);
        }

        let parsed: ClaudeResult = serde_json::from_str(&stdout).map_err(|e| {
            anyhow::anyhow!("failed to parse claude output as JSON: {e}\nraw: {stdout}")
        })?;

        if parsed.is_error {
            anyhow::bail!("claude reported an error: {}", parsed.result);
        }

        // The display-friendly invocation string only exists in `send()`'s
        // stack frame; reconciliation (which calls `await_completion`
        // directly, without going through `send()`) has no way to recover
        // it after a restart, so it's left blank there. `send()` patches
        // this in before returning.
        Ok(SendOutcome::Reply(AgentReply {
            content: parsed.result,
            command: String::new(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::AgentBackend;
    use tokio_util::sync::CancellationToken;

    // Hits the real `claude` CLI and costs a tiny bit of API usage, so it's not
    // run by default. Run with `cargo test -- --ignored` to smoke-test the
    // actual integration (argv building, JSON parsing, --resume continuity,
    // now routed through tmux instead of a direct child process).
    #[ignore]
    #[tokio::test]
    async fn round_trip_with_resume() {
        let task = Task {
            id: uuid::Uuid::new_v4().to_string(),
            parent_id: None,
            name: "smoke-test".into(),
            working_dir: std::env::current_dir().unwrap().to_string_lossy().into_owned(),
            backend: "claude-cli".into(),
            status: "idle".into(),
            permission_mode: "plan".into(),
            created_at: "t0".into(),
            updated_at: "t0".into(),
        };
        let backend = ClaudeCliBackend;

        let run_id_1 = chrono::Utc::now().timestamp_millis();
        let run_1 = RunHandle {
            run_id: run_id_1,
            cancel: CancellationToken::new(),
        };
        let first = match backend
            .send(&task, "Reply with exactly the single word: pong", true, &run_1)
            .await
            .unwrap()
        {
            SendOutcome::Reply(reply) => reply,
            SendOutcome::Cancelled => panic!("unexpected cancellation"),
        };
        assert_eq!(first.content.trim(), "pong");

        let run_2 = RunHandle {
            run_id: run_id_1 + 1,
            cancel: CancellationToken::new(),
        };
        let second = match backend
            .send(&task, "What word did you just reply with?", false, &run_2)
            .await
            .unwrap()
        {
            SendOutcome::Reply(reply) => reply,
            SendOutcome::Cancelled => panic!("unexpected cancellation"),
        };
        assert!(second.content.to_lowercase().contains("pong"));

        let _ = tokio::process::Command::new("tmux")
            .args(["kill-session", "-t", &tmux::session_name(&task.id)])
            .output()
            .await;
    }
}
