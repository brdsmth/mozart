# Mozart Roadmap

Mozart is a learning project: build up, layer by layer, a working understanding of how to orchestrate hundreds of thousands of agents operating on a single codebase. Each step below is a small, fully-working increment — later steps are not started until the current one is understood end-to-end.

## Step 1 — Foundations (done)

Goal: understand the underlying data model and get one task, one agent, one turn working end-to-end with every process inspectable by hand.

- Single Rust CLI (`cli/`)
- No database — all state on the filesystem under `~/.mozart/cli/`
- Sessions: tmux sessions (`mozart-<uuid>`), one per agent
- Turns: dispatched via `tmux send-keys` → `claude -p`; output redirected to `~/.mozart/cli/runs/<run-id>/`; `run.done` sentinel signals completion
- `mozart wait` polls the sentinel; `mozart cancel` sends C-c and writes the sentinel itself

## Step 2 — Ergonomics (done)

Quality-of-life features on top of the core process model.

- `mozart status` — high-level view across all sessions (busy / idle / new, elapsed time, token usage)
- `mozart cost` — total API spend across all runs, broken down by day
- `mozart repo set/ls/use` — save a default working directory, toggle between repos
- `mozart session ls/use` — toggle between sessions without passing UUIDs
- Optional args on `send`, `wait`, `attach`, `cancel` — fall back to active session when omitted
- `run.done` busy guard on `send` — refuses to dispatch while the previous turn is still running
- Wait digest: turns, wall-clock, cost, denied tool calls
- Token usage (input ↑ / output ↓) per session and as an aggregate header total in `mozart status`

## Step 2.5 — Planning (done)

Bridge toward concurrency: decompose a high-level goal into isolated tasks, each dispatchable to a separate agent session.

- `mozart plan new "<goal>"` — one-shot claude call (direct subprocess, not tmux) streams a JSON task list to `~/.mozart/cli/plans/<plan-id>/`; repo path snapshotted at creation time
- `mozart plan ls/show` — inspect plans on disk
- `mozart plan dispatch <id> <n>` — send a task to a session (creates one if omitted, sets it as active)
- `mozart plan dispatch-all <id>` — dispatch every task concurrently, one new session each
- `mozart plan review <id>` — post-dispatch review: exit codes, errors, cost per task
- Task `depends_on` field — tasks carry 1-indexed dependency lists used by the queue system
- Conventional commit instructions baked into every dispatched task message (not delegated to CLAUDE.md)
- Teaches the contrast between one-shot blocking subprocess (planner) and persistent tmux sessions (agent workers)

## Step 2.6 — Queue System (done)

Dependency-aware execution layer on top of planning: enforces `depends_on` ordering by dispatching tasks wave by wave and polling for completions.

- `mozart queue new <plan-id>` — create a queue from a plan's tasks, print the queue ID
- `mozart queue ls` — list queues with goal and progress summary
- `mozart queue show <queue-id>` — show items with live status (pending / running / done / failed)
- `mozart queue run <queue-id>` — blocking event loop: poll completions → unblock ready tasks → dispatch → repeat every 10s
- Resumable: re-running after Ctrl-C skips already-done items and continues from where it left off
- Teaches the fundamental orchestration pattern: react to completions, trigger new work

## Step 3 — Concurrency

Run several sessions/agents at once and observe and manage them together. The first taste of "many agents at once" instead of one at a time. Natural extension of planning: dispatch all tasks from a plan concurrently rather than one at a time.

## Step 4 — Pluggable backends

Add a second agent backend (Codex CLI or a local/open-source model) to prove the pluggable design holds for a real second backend, not just in theory.

## Step 5 — Scaling patterns

Worker pools, queuing, sharding tasks across workers, resource limits — the path from a handful of agents toward hundreds of thousands of agents on one codebase.
