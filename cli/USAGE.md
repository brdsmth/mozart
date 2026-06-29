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
# 0. One-time setup: save your target repo
mozart repo set ~/workspace/repos/personal/mozart

# 1. Start a session — prints a UUID (your handle for everything)
SESSION=$(mozart new)

# 2. Dispatch a turn — prints a run ID
RUN=$(mozart send $SESSION "what does this repo do?")

# 3. Wait for the reply
mozart wait $RUN

# 4. Follow-up turns automatically switch to --resume
RUN=$(mozart send $SESSION "what is the entry point?")
mozart wait $RUN

# Toggle to a different repo
mozart repo use 2
SESSION=$(mozart new)

# Toggle between sessions (no UUID needed after session use)
mozart session ls
mozart session use 2
mozart send "follow-up question"   # uses active session
mozart wait                        # uses active session's latest run
```

## Commands

### `new [working-dir]`
Mint a session ID, start its tmux session, print the UUID.
```
→ tmux new-session -d -s mozart-<id> -c /path/to/dir

  repo:    /path/to/dir
  attach:  tmux attach -t mozart-<id>
  kill:    tmux kill-session -t mozart-<id>

<uuid>
```
Resolution order for the working directory:
1. Explicit `working-dir` argument if provided
2. Active repo from `~/.mozart/cli/config.json` (set via `mozart repo set`)
3. Current directory

---

### `send [session-id] <message> [--bypass]`
Dispatch one message turn. Prints the run ID.

Two forms:
- `mozart send <session-id> <message>` — explicit session ID
- `mozart send <message>` — uses the active session (set via `mozart session use <n>`)

```
→ first turn  (claude will name the conversation using --session-id)
· repo:        /path/to/repo
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

### `wait [run-id] [--json]`
Block until the run finishes, then print the agent's reply (stdout) followed by a
one-glance digest (stderr): turns, wall-clock, cost, and any tool calls that were
denied.

Omit `run-id` to wait on the active session's latest run (requires `mozart session use <n>` first).
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

### `status`
High-level view of all sessions and their current state.
```
3 sessions  (1 busy, 2 idle)

  [busy]  abc12345…  run def67890…  2m30s elapsed
  [idle]  789abcde…  last run ghi01234…  5m12s  ago
  [new]   deadbeef…  (no turns yet)
```
States: `busy` = run in flight, `idle` = last run finished, `new` = tmux exists but no turns yet.
Elapsed time for busy runs is measured from when the run started writing output.
Idle runs show how long ago the last run finished. `[tmux gone]` appears if the tmux session was killed manually.

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

### `attach [session-id]`
Hand your terminal over to the session's tmux pane (live output).
Detach with `Ctrl-b d`. Omit `session-id` to attach to the active session.

---

### `cancel [session-id]`
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

---

### `repo ls`
List all saved repos. The active one is marked with `*`.
```
* 1: /path/to/repo-a
  2: /path/to/repo-b
```

---

### `repo set <path>`
Canonicalize and save a repo path, making it the active repo. If the path is already saved, it is just re-activated (no duplicates).
```
· active repo set to: /path/to/repo-a
```

---

### `repo use <n>`
Switch the active repo by number (1-indexed, matching `repo ls` output).
```
· active repo: /path/to/repo-a
```

---

### `session ls`
List all sessions with their status and repo path. The active session is marked with `*`.
```
* 1: abc12345…  [idle]  /path/to/repo-a
  2: def56789…  [busy]  /path/to/repo-b
  3: deadbeef…  [new]   /path/to/repo-a
```
States: `[busy]` = run in flight, `[idle]` = last run finished, `[new]` = no turns yet.

---

### `session use <n>`
Set the active session by number (1-indexed, matching `session ls` output). Subsequent `send`, `wait`, `cancel`, and `attach` calls use this session when no explicit ID is given.
```
· active session: abc12345…  /path/to/repo-a
```

## Where state lives

| Path | What it is |
|------|------------|
| `~/.mozart/cli/config.json` | Saved repos list, active repo index, and active session UUID (managed by `repo`/`session` subcommands) |
| `~/.mozart/cli/sessions/<id>` | Marker file — presence means the session has had ≥1 turn (switches `--session-id` → `--resume`); contents are the latest run ID (used by the `send` busy guard and by `cancel`) |
| `~/.mozart/cli/sessions/<id>.repo` | The working directory the session was created with (displayed by `send`) |
| `~/.mozart/cli/runs/<run-id>/run.out` | claude stdout (JSON) |
| `~/.mozart/cli/runs/<run-id>/run.err` | claude stderr |
| `~/.mozart/cli/runs/<run-id>/run.exit` | exit code |
| `~/.mozart/cli/runs/<run-id>/run.done` | sentinel — appears when the run is complete |
| tmux session `mozart-<session-id>` | the live process hosting the agent |
