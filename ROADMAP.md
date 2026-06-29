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

- `mozart status` — high-level view across all sessions (busy / idle / new, elapsed time)
- `mozart cost` — total API spend across all runs
- `mozart repo set/ls/use` — save a default working directory, toggle between repos
- `mozart session ls/use` — toggle between sessions without passing UUIDs
- Optional args on `send`, `wait`, `attach`, `cancel` — fall back to active session when omitted
- `run.done` busy guard on `send` — refuses to dispatch while the previous turn is still running
- Wait digest: turns, wall-clock, cost, denied tool calls

## Step 3 — Concurrency

Run several sessions/agents at once and observe and manage them together. The first taste of "many agents at once" instead of one at a time.

## Step 4 — Pluggable backends

Add a second agent backend (Codex CLI or a local/open-source model) to prove the pluggable design holds for a real second backend, not just in theory.

## Step 5 — Scaling patterns

Worker pools, queuing, sharding tasks across workers, resource limits — the path from a handful of agents toward hundreds of thousands of agents on one codebase.
