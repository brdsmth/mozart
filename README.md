# mozart

A bare-metal CLI for managing Claude Code agent sessions via tmux. No database — state lives entirely on the filesystem under `~/.mozart/cli/`.

Designed as a learning project for agent orchestration at scale: how do you manage, supervise, and coordinate many agents working on a single codebase?

## Install

```bash
cd cli
cargo install --path .
```

Requires `tmux` and the `claude` CLI in your PATH.

## Quick start

```bash
# Save your target repo once
mozart repo set ~/workspace/repos/my-project

# Start a session
SESSION=$(mozart new)

# Send a message and wait for the reply
RUN=$(mozart send $SESSION "what does this repo do?")
mozart wait $RUN

# Follow-up turns (--resume handled automatically)
RUN=$(mozart send $SESSION "what is the entry point?")
mozart wait $RUN
```

## Session toggle (no UUID juggling)

```bash
mozart session ls          # numbered list with status and repo
mozart session use 2       # set active session

mozart send "follow-up"    # uses active session
mozart wait                # uses active session's latest run
mozart attach              # drops into active session's tmux pane
```

## Key commands

| Command | Description |
|---------|-------------|
| `mozart new [dir]` | Start a new agent session, print its UUID |
| `mozart send [session] <msg> [--bypass]` | Dispatch a turn. Default permission mode is `plan` (read-only); `--bypass` allows edits |
| `mozart wait [run-id] [--json]` | Block until the run finishes, print the reply and a cost/turn digest |
| `mozart status` | High-level view: busy / idle / new across all sessions |
| `mozart ls` | List all sessions and their tmux state |
| `mozart attach [session]` | Attach to the live tmux pane |
| `mozart cancel [session]` | Send C-c and finalize the in-flight run |
| `mozart kill <session>` | Kill tmux session and remove marker file |
| `mozart kill-all` | Tear down everything |
| `mozart repo ls/set/use` | Manage saved repo paths |
| `mozart session ls/use` | Manage active session |
| `mozart cost` | Total API spend across all runs |
| `mozart guide` | In-terminal workflow cheatsheet |

## State

All state is plain files — inspectable with `ls`, `cat`, `jq`.

| Path | What it is |
|------|------------|
| `~/.mozart/cli/config.json` | Saved repos, active repo index, active session UUID |
| `~/.mozart/cli/sessions/<id>` | Marker file — present once a session has turns; contents = latest run ID |
| `~/.mozart/cli/sessions/<id>.repo` | Working directory the session was created with |
| `~/.mozart/cli/runs/<run-id>/run.out` | Claude stdout (JSON) |
| `~/.mozart/cli/runs/<run-id>/run.done` | Sentinel — appears when the run completes |
| `tmux: mozart-<session-id>` | Live process hosting the agent |

See [cli/USAGE.md](cli/USAGE.md) for full command reference.
