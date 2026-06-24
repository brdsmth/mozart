//! Thin wrapper around the `tmux` CLI — mirrors how `backend::claude_cli`
//! wraps the `claude` CLI: spawn the binary, check the exit status, surface
//! stderr on failure. No tmux-specific parsing beyond that.

use tokio::process::Command;

/// The naming convention is centralized here rather than reconstructed at
/// every call site, so it only needs to change in one place if it ever does.
pub fn session_name(task_id: &str) -> String {
    format!("mozart-{task_id}")
}

pub async fn session_exists(name: &str) -> bool {
    Command::new("tmux")
        .args(["has-session", "-t", name])
        .output()
        .await
        .map(|output| output.status.success())
        .unwrap_or(false)
}

pub async fn ensure_session(name: &str, working_dir: &str) -> anyhow::Result<()> {
    if session_exists(name).await {
        return Ok(());
    }

    let output = Command::new("tmux")
        .args(["new-session", "-d", "-s", name, "-c", working_dir])
        .output()
        .await?;

    if !output.status.success() {
        anyhow::bail!(
            "tmux new-session failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

pub async fn send_command(name: &str, command: &str) -> anyhow::Result<()> {
    let output = Command::new("tmux")
        .args(["send-keys", "-t", name, command, "Enter"])
        .output()
        .await?;

    if !output.status.success() {
        anyhow::bail!(
            "tmux send-keys failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

pub async fn interrupt(name: &str) -> anyhow::Result<()> {
    let output = Command::new("tmux")
        .args(["send-keys", "-t", name, "C-c"])
        .output()
        .await?;

    if !output.status.success() {
        anyhow::bail!(
            "tmux send-keys C-c failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn unique_session_name(prefix: &str) -> String {
        format!("{prefix}-{}", uuid::Uuid::new_v4())
    }

    async fn kill_session(name: &str) {
        let _ = Command::new("tmux").args(["kill-session", "-t", name]).output().await;
    }

    #[tokio::test]
    async fn session_exists_false_for_unknown_session() {
        let name = unique_session_name("mozart-test-missing");
        assert!(!session_exists(&name).await);
    }

    #[tokio::test]
    async fn ensure_session_creates_and_is_idempotent() {
        let name = unique_session_name("mozart-test-ensure");
        let working_dir = std::env::temp_dir().to_string_lossy().into_owned();

        ensure_session(&name, &working_dir).await.unwrap();
        assert!(session_exists(&name).await);

        // Calling again on an already-running session must be a no-op, not
        // an error — `send_message` calls this on every turn, not just the
        // first.
        ensure_session(&name, &working_dir).await.unwrap();
        assert!(session_exists(&name).await);

        kill_session(&name).await;
    }

    #[tokio::test]
    async fn send_command_types_into_the_pane() {
        let name = unique_session_name("mozart-test-send");
        let working_dir = std::env::temp_dir().to_string_lossy().into_owned();
        let marker = std::env::temp_dir().join(format!("mozart-tmux-test-{}", uuid::Uuid::new_v4()));

        ensure_session(&name, &working_dir).await.unwrap();
        send_command(&name, &format!("touch {}", marker.display()))
            .await
            .unwrap();

        for _ in 0..50 {
            if marker.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(marker.exists(), "expected the pane's shell to run the sent command");

        let _ = std::fs::remove_file(&marker);
        kill_session(&name).await;
    }

    #[tokio::test]
    async fn interrupt_does_not_error_on_an_idle_pane() {
        let name = unique_session_name("mozart-test-interrupt");
        let working_dir = std::env::temp_dir().to_string_lossy().into_owned();

        ensure_session(&name, &working_dir).await.unwrap();
        interrupt(&name).await.unwrap();

        kill_session(&name).await;
    }
}
