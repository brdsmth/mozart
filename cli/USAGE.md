# mozart CLI

Bare-metal claude session manager. No database — state lives entirely on the filesystem under `~/.mozart/cli/`.

Run `mozart guide` for an in-terminal cheatsheet at any time.

## Build

```bash
cd cli
cargo build --release
# binary at: cli/target/release/mozart
```

Or run without installing:
```bash
cargo run -- <command>
```

## Typical flow

```bash
cd cli

# 1. Start a session — prints a UUID (your handle for everything)
SESSION=$(cargo run -- new ~/workspace/repos/personal/mozart)

# 2. Dispatch a turn — prints a run ID
RUN=$(cargo run -- send $SESSION "what does this repo do?")

# 3. Wait for the reply
cargo run -- wait $RUN

# 4. Follow-up turns automatically switch to --resume
RUN=$(cargo run -- send $SESSION "what is the entry point?")
cargo run -- wait $RUN
```

## Commands

### `new [working-dir]`
Mint a session ID, start its tmux session, print the UUID.
```
→ tmux new-session -d -s mozart-<id> -c /path/to/dir

  attach:  tmux attach -t mozart-<id>
  kill:    tmux kill-session -t mozart-<id>

<uuid>
```
Defaults to current directory if omitted.

---

### `send <session-id> <message> [--bypass]`
Dispatch one message turn. Prints the run ID.
```
→ first turn  (claude will name the conversation using --session-id)
→ dispatching into mozart-<id>:
  claude -p --session-id <id> --output-format json --permission-mode plan 'message'

  run dir: ~/.mozart/cli/runs/<run-id>/
  stream:  tail -f ~/.mozart/cli/runs/<run-id>/run.out
  watch:   tmux attach -t mozart-<id>

<run-uuid>
```
`--bypass` sets `--permission-mode bypassPermissions` so the agent can edit files and run commands. Default is `plan` (read-only).

The first turn uses `--session-id`; all subsequent turns use `--resume`. This is tracked by the presence of `~/.mozart/cli/sessions/<session-id>`.

---

### `wait <run-id>`
Block until the run finishes, then print the agent's reply.
```
· polling ~/.mozart/cli/runs/<run-id>/run.done ...
· done  exit 0

<reply>
```
Exits non-zero if claude errored.

---

### `ls`
List active mozart tmux sessions and all known session IDs with their tmux status.
```
tmux sessions:
  mozart-<id>
    attach:  tmux attach -t mozart-<id>
    kill:    tmux kill-session -t mozart-<id>

sessions with turns  (~/.mozart/cli/sessions/):
  <id>  [tmux alive]
    kill:  mozart kill <id>
```

---

### `kill <session-id>`
Kill the tmux session and remove the session marker file.
```
→ tmux kill-session -t mozart-<id>
→ rm ~/.mozart/cli/sessions/<id>
· done
```

---

### `attach <session-id>`
Hand your terminal over to the session's tmux pane (live output).
Detach with `Ctrl-b d`.

---

### `cancel <session-id>`
Send `C-c` to interrupt whatever is running in the session.

---

### `cat <run-id>`
Print a run's raw output file without waiting for it to finish.

---

### `guide`
Print a workflow cheatsheet in the terminal.

## Where state lives

| Path | What it is |
|------|------------|
| `~/.mozart/cli/sessions/<id>` | Marker file — presence means the session has had ≥1 turn (switches `--session-id` → `--resume`) |
| `~/.mozart/cli/runs/<run-id>/run.out` | claude stdout (JSON) |
| `~/.mozart/cli/runs/<run-id>/run.err` | claude stderr |
| `~/.mozart/cli/runs/<run-id>/run.exit` | exit code |
| `~/.mozart/cli/runs/<run-id>/run.done` | sentinel — appears when the run is complete |
| tmux session `mozart-<session-id>` | the live process hosting the agent |
