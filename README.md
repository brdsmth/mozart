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

## Planning workflow

```bash
# Decompose a goal into tasks
PLAN=$(mozart plan new "add dark mode and README to the app")
mozart plan show $PLAN

# Option A — dispatch all at once (no dep ordering)
mozart plan dispatch-all $PLAN
mozart status
mozart plan review $PLAN

# Option B — queue run (respects depends_on, wave by wave)
QUEUE=$(mozart queue new $PLAN)
mozart queue show $QUEUE      # preview items and dep order
mozart queue run $QUEUE       # blocking; Ctrl-C safe — resumes from where it left off
mozart queue show $QUEUE      # final status
```

## Key commands

| Command | Description |
|---------|-------------|
| `mozart new [dir]` | Start a new agent session, print its UUID |
| `mozart send [session] <msg> [--bypass]` | Dispatch a turn. Default is read-only `plan` mode; `--bypass` allows file edits |
| `mozart wait [run-id] [--json]` | Block until the run finishes, print the reply and a cost/turn digest |
| `mozart status [--busy\|--idle]` | High-level view: busy / idle / new, elapsed time, token usage per session and total |
| `mozart ls` | List all sessions and their tmux state |
| `mozart attach [session]` | Attach to the live tmux pane |
| `mozart cancel [session]` | Send C-c and finalize the in-flight run |
| `mozart kill <session>` | Kill tmux session and remove marker file |
| `mozart kill-all` | Tear down everything |
| `mozart repo ls/set/use` | Manage saved repo paths |
| `mozart session ls/use` | Manage active session |
| `mozart cost` | Total API spend across all runs, by day |
| `mozart guide` | In-terminal workflow cheatsheet |
| `mozart plan new "<goal>"` | Decompose a goal into tasks (streaming claude call), print plan ID |
| `mozart plan ls/show <id>` | List plans or show a plan's numbered task list with dependencies |
| `mozart plan dispatch <id> <n>` | Send task N to a session (creates one if omitted) |
| `mozart plan dispatch-all <id>` | Dispatch every task concurrently, one new session each |
| `mozart plan review <id>` | Show exit codes, errors, and cost for all dispatched tasks |
| `mozart queue new <plan-id>` | Create a dependency-aware queue from a plan, print queue ID |
| `mozart queue ls` | List queues with goal and done/total progress |
| `mozart queue show <id>` | Show queue items and their current status |
| `mozart queue run <id>` | Blocking: dispatch tasks wave by wave in dependency order |

## State

All state is plain files — inspectable with `ls`, `cat`, `jq`.

| Path | What it is |
|------|------------|
| `~/.mozart/cli/config.json` | Saved repos, active repo index, active session UUID |
| `~/.mozart/cli/sessions/<id>` | Marker file — present once a session has turns; contents = latest run ID |
| `~/.mozart/cli/sessions/<id>.repo` | Working directory the session was created with |
| `~/.mozart/cli/runs/<run-id>/run.out` | Claude stdout (JSON) |
| `~/.mozart/cli/runs/<run-id>/run.done` | Sentinel — appears when the run completes |
| `~/.mozart/cli/plans/<plan-id>/goal.txt` | Original goal string |
| `~/.mozart/cli/plans/<plan-id>/tasks.json` | Task array with titles, descriptions, and `depends_on` |
| `~/.mozart/cli/plans/<plan-id>/repo.txt` | Repo path snapshotted at plan-creation time |
| `~/.mozart/cli/plans/<plan-id>/sessions.json` | Dispatch records: which session got which task |
| `~/.mozart/cli/queues/<queue-id>/meta.json` | Queue goal, plan ID, repo |
| `~/.mozart/cli/queues/<queue-id>/items.json` | Items with live status (pending/running/done/failed) |
| `tmux: mozart-<session-id>` | Live process hosting the agent |

See `mozart guide` for the full in-terminal cheatsheet.
