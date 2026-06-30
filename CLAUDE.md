# Mozart

## What this is

Mozart is a hands-on learning project for understanding agent orchestration at scale — specifically, how to manage hundreds of thousands of agents working on a single codebase. It's being built deliberately layer by layer: each step is a small, fully-working increment, not a leap toward the end state. See [ROADMAP.md](./ROADMAP.md) for the full step list and current progress.

## Current architecture

The entire project is a Rust CLI at `cli/`. All state is on the filesystem under `~/.mozart/cli/` — no database, no UI.

- **Sessions:** each session is a `tmux` session named `mozart-<uuid>`. Created by `mozart new`, which writes a `.repo` sidecar (`~/.mozart/cli/sessions/<id>.repo`) recording the working directory.
- **Turns:** dispatched via `tmux send-keys`. Each turn is a `claude` CLI invocation (`claude -p --session-id` for first turn, `claude -p --resume` for subsequent). First turn is detected by the presence of `~/.mozart/cli/sessions/<id>` (the marker file).
- **Runs:** each turn produces a run directory at `~/.mozart/cli/runs/<run-id>/` with `run.out`, `run.err`, `run.exit`, and a `run.done` sentinel. `mozart wait` polls for the sentinel.
- **Config:** `~/.mozart/cli/config.json` stores saved repos (list + active index) and the active session UUID.
- **Plans:** `mozart plan new "<goal>"` runs claude as a direct blocking subprocess (not tmux) to decompose a goal into a JSON task list. Stored at `~/.mozart/cli/plans/<plan-id>/` with `goal.txt`, `tasks.json`, `repo.txt` (snapshotted at creation), and `sessions.json` (dispatch records). Each task carries a `depends_on` array of 1-indexed task numbers.
- **Queues:** `mozart queue run` is a blocking event loop that reads a plan's tasks, enforces `depends_on` ordering by dispatching wave by wave, polls for completions, and advances. State lives at `~/.mozart/cli/queues/<queue-id>/` as `meta.json` and `items.json` (live status per item). Resumable after interruption.

## Why these design choices

- **No database.** Every piece of state is a plain file: readable with `cat`, inspectable with `ls`, debuggable with `tail -f`. This is the right foundation before adding persistence complexity.
- **tmux, not child processes.** Each agent runs in a long-lived attachable pane — turns are cancellable (C-c into the pane), the session survives CLI restarts, and you can watch the agent work live with `mozart attach`.
- **`run.done` sentinel over process handles.** Polling a file works whether the process was started in this shell session or a previous one. It's what makes `mozart wait` reliable across restarts.
- **Concurrency and multiple backends are deferred on purpose**, so each layer can be understood in isolation before the next one is added.

## For other agents working in this repo

- Read this file and [ROADMAP.md](./ROADMAP.md) before making structural changes.
- All CLI logic lives in `cli/src/main.rs`.
- Don't jump ahead to a later roadmap step (concurrency, multi-backend, scaling) while working on an earlier one, unless explicitly asked to.
- Update ROADMAP.md's step status as steps are completed.
- After any code change: `cd cli && cargo install --path .` to rebuild the binary.
