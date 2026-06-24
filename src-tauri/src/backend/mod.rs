pub mod claude_cli;

use crate::models::Task;
use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

pub struct AgentReply {
    pub content: String,
    pub command: String,
}

/// What an agent call settled into. A plain `Result<AgentReply>` can't
/// distinguish "cancelled" (not a failure — the user asked for it) from
/// "errored", and `cancel_run` needs to treat the two completely
/// differently (it already wrote `runs.status = 'cancelled'` /
/// `tasks.status = 'idle'` itself before this resolves, so callers must
/// *not* re-write those columns on a `Cancelled` outcome).
pub enum SendOutcome {
    Reply(AgentReply),
    Cancelled,
}

/// Carries what a backend needs to supervise one run: which
/// `~/.mozart/runs/<run_id>/` directory to poll, and a token to race against
/// while polling so `cancel_run` can interrupt an in-flight call.
pub struct RunHandle {
    pub run_id: i64,
    pub cancel: CancellationToken,
}

#[async_trait]
pub trait AgentBackend: Send + Sync {
    /// Dispatch a new agent turn and wait for it to settle.
    async fn send(
        &self,
        task: &Task,
        user_message: &str,
        is_first_turn: bool,
        run: &RunHandle,
    ) -> anyhow::Result<SendOutcome>;

    /// Resume waiting on a turn that was already dispatched (e.g. before a
    /// Tauri restart) without re-issuing the underlying command.
    async fn await_completion(&self, run: &RunHandle) -> anyhow::Result<SendOutcome>;
}

pub fn get_backend(name: &str) -> anyhow::Result<Box<dyn AgentBackend>> {
    match name {
        "claude-cli" => Ok(Box::new(claude_cli::ClaudeCliBackend)),
        other => Err(anyhow::anyhow!("unknown agent backend: {other}")),
    }
}
