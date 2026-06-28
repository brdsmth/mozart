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

The first turn uses `--session-id`; all subsequent turns use `--resume`. This is tracked by the presence of `~/.mozart/cli/sessions/<session-id>` (whose contents are the latest run ID).

**One turn at a time per session.** A session is a single tmux pane (one shell), so `send` refuses to dispatch while that session's previous run is still going — otherwise the second turn would silently queue behind the first. You'll get:
```
error: session <id> is busy — run <run-id> hasn't finished
  wait:    mozart wait <run-id>
  cancel:  mozart cancel <id>
```
`wait` for it to finish, or `cancel` to abandon it (see below). For genuine parallelism, use separate sessions.

---

### `wait <run-id> [--json]`
Block until the run finishes, then print the agent's reply (stdout) followed by a
one-glance digest (stderr): turns, wall-clock, cost, and any tool calls that were
denied.
```
· polling ~/.mozart/cli/runs/<run-id>/run.done ...
· done  exit 0

<reply>

─────────────────────────────────
 8 turns · 5m07s · $1.21
 ⚠ 13 tool calls DENIED: Write×13
   (session is in plan mode — re-send with --bypass to allow)
─────────────────────────────────
```
The reply goes to stdout and the digest to stderr, so `$(mozart wait $RUN)` still
captures only the agent's text. Denials only happen in plan mode, so seeing them is
the cue that the turn needed `--bypass`. The digest's stats/denials rows are each
omitted when absent.

`--json` prints the full raw JSON payload to stdout instead (pipeable into `jq`),
skipping the reply/digest formatting.

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

### `kill-all`
Kill every active mozart tmux session and remove all session marker files.
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
Send `C-c` to interrupt whatever is running in the session, then finalize the
in-flight run: the `C-c` aborts the `claude ...; touch run.done` chain before the
sentinel is written, so `cancel` writes it (with exit code `130`) itself. Without
this, `mozart wait` would block forever and the session would stay permanently
"busy" to `send`.

---

### `cat <run-id>`
Print a run's raw output file without waiting for it to finish.

---

### `guide`
Print a workflow cheatsheet in the terminal.

## Where state lives

| Path | What it is |
|------|------------|
| `~/.mozart/cli/sessions/<id>` | Marker file — presence means the session has had ≥1 turn (switches `--session-id` → `--resume`); contents are the latest run ID (used by the `send` busy guard and by `cancel`) |
| `~/.mozart/cli/runs/<run-id>/run.out` | claude stdout (JSON) |
| `~/.mozart/cli/runs/<run-id>/run.err` | claude stderr |
| `~/.mozart/cli/runs/<run-id>/run.exit` | exit code |
| `~/.mozart/cli/runs/<run-id>/run.done` | sentinel — appears when the run is complete |
| tmux session `mozart-<session-id>` | the live process hosting the agent |
