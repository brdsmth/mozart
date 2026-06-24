# Mozart Roadmap

Mozart is a learning project: build up, layer by layer, a working understanding of how to orchestrate hundreds of thousands of agents operating on a single codebase. Each step below is a small, fully-working increment — later steps are not started until the current one is understood end-to-end.

## Step 1 — Foundations (done)

Goal: understand the underlying data model. Get one task, one agent, one SQLite database, and one Tauri UI working end-to-end, with every row and every process inspectable by hand.

- Svelte + TypeScript frontend, Tauri (Rust) backend
- SQLite schema: `tasks` (self-referencing `parent_id`, so one agent can later spawn sub-agents as a tree instead of a flat list), `messages`, `runs`
- A single `AgentBackend` trait, implemented first by wrapping the real `claude` CLI (`claude -p`, one process per message, no tmux yet)
- DB lives at `~/.mozart/mozart.db` so it can be inspected directly with `sqlite3` while using the app

## Step 2 — Process supervision via tmux (done)

Replace the one-shot child process with a tmux session per agent, so the agent process is long-lived and attachable/detachable from a terminal, and survives the Tauri UI restarting.

- Each message turn still runs the same one-shot `claude -p --resume ... --output-format json` command from Step 1; it's now dispatched via `tmux send-keys` into a long-lived `mozart-<task_id>` session instead of a directly-owned child process, with output redirected to `~/.mozart/runs/<run_id>/` and a sentinel file signaling completion
- Cancellation: a `RunRegistry` of `CancellationToken`s lets `cancel_run` interrupt an in-flight call (`tmux send-keys C-c`) without killing the tmux session itself
- Startup reconciliation resumes or resolves any run left `running` from a previous session, so a Tauri crash or restart mid-turn doesn't strand it
- UI: a Cancel control on the "thinking…" row, a `tmux attach -t mozart-<task_id>` affordance once a task has sent its first message, and distinct styling/status for cancelled runs

## Step 3 — Concurrency

Run several tasks/agents at once and observe and manage them together. The first taste of "many agents at once" instead of one at a time.

## Step 4 — Pluggable backends

Add a second `AgentBackend` implementation (Codex CLI or a local/open-source model) to prove the pluggable design holds for a real second backend, not just in theory.

## Step 5 — Scaling patterns

Worker pools, queuing, sharding tasks across workers, resource limits — the path from a handful of agents toward hundreds of thousands of agents on one codebase.
