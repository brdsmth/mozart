# mozart CLI

Bare-metal claude session manager. No database — state lives entirely on the filesystem under `~/.mozart/cli/`.

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

## Commands

### `new [working-dir]`
Mint a session ID and start its tmux session. Prints the UUID.
```bash
SESSION=$(cargo run -- new ~/projects/myrepo)
```
Defaults to current directory if `working-dir` is omitted.

---

### `send <session-id> <message> [--bypass]`
Dispatch one message turn. Prints the run ID.
```bash
RUN=$(cargo run -- send $SESSION "what does this repo do?")
```
`--bypass` sets `--permission-mode bypassPermissions` so the agent can edit files and run commands. Default is `plan` (read-only).

---

### `wait <run-id>`
Block until the run finishes, then print the agent's reply.
```bash
cargo run -- wait $RUN
```
Exits non-zero if claude errored.

---

### `attach <session-id>`
Hand your terminal over to the session's tmux pane (live output).
```bash
cargo run -- attach $SESSION
```
Detach with `Ctrl-b d`.

---

### `cancel <session-id>`
Send `C-c` to interrupt whatever is running in the session.
```bash
cargo run -- cancel $SESSION
```

---

### `cat <run-id>`
Print a run's raw output file without waiting for it to finish.
```bash
cargo run -- cat $RUN
```

## Typical flow

```bash
cd cli

SESSION=$(cargo run -- new ~/projects/myrepo)
RUN=$(cargo run -- send $SESSION "explain what this repo does in 2 sentences")
cargo run -- wait $RUN

# follow-up turn (automatically uses --resume)
RUN=$(cargo run -- send $SESSION "what is the entry point?")
cargo run -- wait $RUN

# watch it run live in another terminal
cargo run -- attach $SESSION
```

## Where state lives

| Path | What it is |
|------|------------|
| `~/.mozart/cli/sessions/<id>` | Marker file — presence means the session has had ≥1 turn (switches `--session-id` → `--resume`) |
| `~/.mozart/cli/runs/<run-id>/run.out` | claude stdout (JSON) |
| `~/.mozart/cli/runs/<run-id>/run.err` | claude stderr |
| `~/.mozart/cli/runs/<run-id>/run.exit` | exit code |
| `~/.mozart/cli/runs/<run-id>/run.done` | sentinel — appears when the run is complete |
| tmux session `mozart-<session-id>` | the live process hosting the agent |
