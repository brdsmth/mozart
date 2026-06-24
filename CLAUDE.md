# Mozart

## What this is

Mozart is a hands-on learning project for understanding agent orchestration at scale — specifically, how to manage hundreds of thousands of agents working on a single codebase. It's being built deliberately layer by layer: each step is a small, fully-working increment, not a leap toward the end state. See [ROADMAP.md](./ROADMAP.md) for the full step list and current progress.

## Current architecture (Step 2)

- **Frontend:** Svelte + TypeScript, inside a Tauri (Rust) desktop shell.
- **Data store:** SQLite at `~/.mozart/mozart.db` — a fixed, easy-to-inspect path, not an OS-specific app-data directory, so the schema can be inspected directly with `sqlite3` while using the app.
- **Core entities:**
  - `tasks` — the root unit of work. Self-referencing `parent_id` so one agent can later spawn sub-agents/sub-tasks as a tree, not just a flat list. A "session" in the UI is just a root-level task (`parent_id IS NULL`) with a chat view attached.
  - `messages` — the chat log for a task, keyed by `task_id`.
  - `runs` — one physical process execution attempting a task. Kept separate from `tasks`/`messages` because at scale, process lifecycle (start/stop/retry/supervise) needs to be tracked independently of conversation content.
- **Agent backend:** a Rust `AgentBackend` trait, with one implementation so far — `ClaudeCliBackend`. Designed to be pluggable: adding a Codex or open-source-model backend later means implementing the trait and registering a lookup key, not a redesign.
- **Process model:** each task's agent calls run inside a long-lived, attachable `tmux` session (`mozart-<task_id>`, see `tmux.rs`) instead of a one-shot child process. A turn is dispatched via `tmux send-keys`, with stdout/stderr/exit-code/done-sentinel redirected to `~/.mozart/runs/<run_id>/` (see `db.rs::run_dir`) and polled rather than awaited directly on a process handle — this is what makes a turn cancellable (`cancel_run` interrupts the pane without killing the session) and resumable after a Tauri restart (`commands::reconcile_startup_runs` re-attaches to anything still `running` on disk).

## Why these design choices

- **`task`, not `session`, is the root concept.** Orchestrating many agents is fundamentally a task-scheduling/decomposition problem (closer to a CI system or job queue) rather than a chat-app problem — its defining feature is that one agent spawns others to parallelize work. A flat list of chat "sessions" has no slot for that. `tasks.parent_id` exists from Step 1 onward so that capability never requires a schema migration.
- **`runs` is separate from `tasks`/`messages`** so that process execution (one attempt, possibly retried, possibly long-lived) is tracked independently of conversational content — this is exactly what Step 2's polling/reconciliation needed, with no schema rework.
- **Concurrency and multiple backends are still deferred on purpose**, so each layer can be understood in isolation before the next one is added.

## For other agents working in this repo

- Read this file and [ROADMAP.md](./ROADMAP.md) before making structural changes.
- Keep the `tasks` / `messages` / `runs` separation intact — don't collapse `runs` back into `tasks` for convenience; it's load-bearing for later roadmap steps.
- Don't jump ahead to a later roadmap step (concurrency, multi-backend, scaling) while working on an earlier one, unless explicitly asked to.
- Update ROADMAP.md's step status as steps are completed.
