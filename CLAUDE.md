# abtop

AI agent monitor for your terminal. Like btop++, but for AI coding agents.

Currently supports Claude Code. Codex planned for v0.2.

## Architecture

```
src/
├── main.rs                 # Entry, terminal setup, event loop
├── app.rs                  # App state, tick logic, key handling
├── ui/
│   ├── mod.rs              # btop-style 6-panel layout
│   ├── rate_limit.rs       # Panel ¹: rate limit sparkline + context bars
│   ├── tokens.rs           # Panel ²: token stats + sparkline
│   ├── projects.rs         # Panel  : project git status
│   ├── ports.rs            # Panel ³: open ports + conflict detection
│   └── sessions.rs         # Panel ⁴: session list + children + detail
├── collector/
│   ├── mod.rs              # Collector trait, 2s polling loop
│   ├── claude.rs           # Claude Code: sessions, transcripts, processes
│   ├── process.rs          # Child process tree + open ports (lsof)
│   └── git.rs              # Git branch/status per cwd
├── model/
│   ├── session.rs          # AgentSession, SessionStatus
│   ├── transcript.rs       # TranscriptEntry, Usage, ToolUse
│   └── process.rs          # ChildProcess, OpenPort
└── utils.rs                # Token formatting, path encoding, time helpers
```

## Layout (btop 1:1 mapping)

```
┌─ ¹rate limit + context ──────────────────────────────────────────────┐
│                                                                      │
│  5h usage sparkline (history)              SESSION CONTEXT            │
│  ░░▒▒▓▓██████████░░░░                      S1 abtop       ████████ 82%│
│                                            S2 prediction  █████████91%⚠│
│                                            S3 api-server  ███      22%│
│  5h ████████░░ 72%  resets 1h23m           sessions: 3               │
└──────────────────────────────────────────────────────────────────────┘
┌─ ²tokens ────┐┌─ projects ───┐┌─ ⁴sessions (tall, right half) ─────┐
│ Total  1.2M  ││ abtop        ││ Pid  Project    Status Model CTX Tok│
│ Input  402k  ││  main +3 ~18 ││►7336 abtop     ● Work opus  82% 45k│
│ Output  89k  ││              ││      └─ Edit src/collector/claude.rs │
│ Cache  710k  ││ prediction   ││ 8840 prediction ◌ Wait sonn  91% 120k│
│              ││  feat/x +1~2 ││      └─ waiting for input            │
│ ▁▃▅▇█▇▅▃▁▃▅ ││              ││ 9102 api-server ● Work haiku 42% 8k │
│ tokens/turn  ││ api-server   ││      └─ Bash npm run dev             │
│              ││  main ✓clean ││                                      │
│ Turns: 48    ││              ││ CHILDREN (►7336 · abtop)             │
│ Avg: 25k/t   ││              ││  7401 cargo build        342M       │
└──────────────┘└──────────────┘│  7455 cargo test          28M       │
┌─ ³ports ─────────────────────┐│                                      │
│ PORT  SESSION      CMD   PID ││ SUBAGENTS                            │
│ :3000 api-server   node 9150 ││  Agent explore-data  ✓ 12k          │
│ :3001 api-server   node 9178 ││  Agent run-tests     ● 8k           │
│ :5433 api-server   pg   9203 ││                                      │
│ :8080 prediction   cargo 8901││ MEM 4 files · 12/200 lines          │
│ :8080 abtop        cargo 7401││ v2.1.86 · 47m · 12 turns            │
│                    ⚠ conflict││                                      │
└──────────────────────────────┘└──────────────────────────────────────┘
```

Panel mapping:
- **¹cpu → ¹rate limit + context**: Left = 5h/7d sparkline history. Right = per-session context % bars with compact warning.
- **²mem → ²tokens**: Total token breakdown (in/out/cache) + per-turn sparkline.
- **disks → projects**: Per-project git branch + change summary.
- **³net → ³ports**: Agent-spawned open ports + conflict detection.
- **⁴proc → ⁴sessions**: Session list with inline current task, children, subagents, memory status.

## Data Sources (Claude Code)

All read-only from local filesystem + `ps` + `lsof`. No API calls, no auth.

### 1. Session discovery: `~/.claude/sessions/{PID}.json`
```json
{ "pid": 7336, "sessionId": "2f029acc-...", "cwd": "/Users/graykode/abtop", "startedAt": 1774715116826, "kind": "interactive", "entrypoint": "cli" }
```
- ~170 bytes. Created on start, deleted on exit.
- Scan all files, verify PID alive with `kill(pid, 0)`.

### 2. Transcript: `~/.claude/projects/{encoded-path}/{sessionId}.jsonl`
Path encoding: `/Users/foo/bar` → `-Users-foo-bar`

Key line types:

**`assistant`** (tokens, model, tools):
```json
{
  "type": "assistant",
  "timestamp": "2026-03-28T15:25:55.123Z",
  "message": {
    "model": "claude-opus-4-6",
    "stop_reason": "end_turn",
    "usage": {
      "input_tokens": 2,
      "output_tokens": 5,
      "cache_read_input_tokens": 11313,
      "cache_creation_input_tokens": 4350
    },
    "content": [
      { "type": "text", "text": "..." },
      { "type": "tool_use", "name": "Edit", "input": { "file_path": "src/main.rs", ... } }
    ]
  }
}
```

**`user`** (prompts, version):
```json
{ "type": "user", "timestamp": "...", "version": "2.1.86", "gitBranch": "main", "message": { "role": "user", "content": "..." } }
```

**`last-prompt`** (session tail marker):
```json
{ "type": "last-prompt", "lastPrompt": "...", "sessionId": "..." }
```

- **Size: 1KB–18MB**. Append-only, new line per message.
- **Reading strategy**: On first discovery, scan full file to build cumulative token totals. Then watch file size — on growth, read only new bytes appended since last read (track file offset). This gives both lifetime totals and real-time updates without re-reading.
- **Partial line handling**: new bytes may end mid-JSON-line. Buffer incomplete lines until next read.
- **File rotation**: if file shrinks (session restart), reset offset to 0 and re-scan.

### 3. Subagents: `~/.claude/projects/{path}/{sessionId}/subagents/`
- `agent-{hash}.jsonl` — same JSONL format as main transcript
- `agent-{hash}.meta.json` — `{ "agentType": "general-purpose", "description": "..." }`

### 4. Process tree: `ps` + `lsof`
```bash
# Find Claude sessions
ps aux | grep '/claude --session-id'
# Extract: PID, RSS, CPU%, --session-id UUID
# Filter out: Claude.app, cmux claude-hook

# Child processes of a Claude session
pgrep -P {claude_pid}
ps -o pid,ppid,rss,command -p {child_pids}

# Open ports by child processes
lsof -i -P -n | grep LISTEN
# Map listening PID → parent Claude PID → session
```

### 5. Git status per project
```bash
git -C {cwd} branch --show-current    # branch name
git -C {cwd} diff --stat HEAD         # changed files summary
git -C {cwd} status --porcelain       # clean/dirty check
```

### 6. Memory status
- Path: `~/.claude/projects/{encoded-path}/memory/`
- Count files in directory
- Count lines in `MEMORY.md` (200 line limit, truncation = memory loss)

### 7. Rate limit
- Statusline JSON (v2.1.80+): `rate_limits.session.used_percentage`, `rate_limits.weekly.used_percentage`
- NOT persisted to disk.
- **MVP approach**: show "—" (unavailable). No sparkline history without persistence.
- **Future**: could write a Claude Code hook that pipes statusline rate_limit data to a local file that abtop reads. This is v0.2 scope.
- This is an account-level metric, shared across all sessions.

### 8. Other files
- `~/.claude/stats-cache.json` — daily aggregates. Only updated on `/stats`, NOT real-time.
- `~/.claude/history.jsonl` — prompt history with sessionId. Can get last prompt for each session.

## Session Status Detection

```
● Working  = PID alive + transcript mtime < 30s ago
◌ Waiting  = PID alive + transcript mtime > 30s ago
✗ Error    = PID alive + last assistant has error content
✓ Done     = PID dead (detected via kill(pid, 0) failure)
```

**Done detection**: session files are deleted on normal exit, but may linger briefly or survive crashes. When PID is dead but file exists, show as Done and clean up on next tick. When file is gone, remove from list entirely.

**PID reuse risk**: verify PID is still a claude process by checking `/proc/{pid}/cmdline` (Linux) or `ps -p {pid} -o command=` (macOS) contains `/claude`. Don't trust PID alone.

Current task (2nd line under each session):
- Working → last `tool_use` name + first arg (e.g. `Edit src/main.rs`)
- Waiting → "waiting for user input"
- Error → last error message (truncated)
- Done → "finished {duration} ago"

**Known limitations** (all heuristic, document in UI):
- Cannot distinguish model-thinking vs tool-executing vs rate-limit-waiting vs permission-prompt
- "Waiting" may be wrong if a long-running tool (cargo build, npm test) is running
- Status is best-effort, not authoritative

## Context Window Calculation

Not provided in data files. Derive:
- **Window size**: hardcode by model name
  - `claude-opus-4-6` → 200,000 (default)
  - `claude-opus-4-6[1m]` → 1,000,000
  - `claude-sonnet-4-6` → 200,000
  - `claude-haiku-4-5` → 200,000
- **Current usage**: last `assistant` line's `input_tokens + cache_read_input_tokens + cache_creation_input_tokens`
- **Percentage**: current_usage / window_size * 100
- **Warning**: yellow at 80%, red at 90%, ⚠ icon at 90%+

## Port Conflict Detection

When two child processes (from different sessions) listen on the same port:
- Mark both with `⚠ conflict` in ports panel
- Highlight in red

## Key Bindings

| Key | Action |
|-----|--------|
| `↑`/`↓` or `k`/`j` | Select session in list |
| `Enter` | Jump to session terminal (tmux only, see below) |
| `Tab` | Cycle focus between panels |
| `1`–`4` | Toggle panel visibility (like btop) |
| `q` | Quit |
| `r` | Force refresh |

## Tech Stack

- **Rust** (2021 edition)
- **ratatui** + **crossterm** for TUI
- **serde** + **serde_json** for JSON/JSONL parsing
- **tokio** for async runtime — `ps`, `lsof`, `git` commands must not block the UI thread
- **Polling intervals** (staggered to avoid freezes):
  - Session scan (sessions/*.json): every 2s
  - Transcript tail: every 2s
  - Process tree (ps): every 5s
  - Port scan (lsof): every 10s (lsof is slow on macOS)
  - Git status: every 10s (git can be slow on large repos)

## Commit Convention

```
<type>: <description>
```
Types: `feat`, `fix`, `refactor`, `docs`, `chore`

## Commands

```bash
cargo build                    # Build
cargo run                      # Run TUI
cargo run -- --once            # Print snapshot and exit (debug mode)
cargo test                     # Tests
cargo clippy                   # Lint
```

## Non-Goals (v0.1)

- Codex/Gemini/Cursor support
- Cost estimation
- Remote/SSH monitoring
- Notifications/alerts
- Session control (attach, kill, send input)
- Rate limit history persistence (no disk writes)

## tmux Integration

Session jump (`Enter`) only works when abtop runs inside tmux:
1. On startup, detect if `$TMUX` is set. If not, disable Enter key and show "(no tmux)" in footer.
2. To map PID → tmux pane: `tmux list-panes -a -F '#{pane_pid} #{session_name}:#{window_index}.#{pane_index}'` then walk process tree to find which pane owns the Claude PID.
3. Jump: `tmux select-pane -t {target}`
4. If mapping fails (PID not in any pane), show "pane not found" and do nothing.

## Privacy

abtop reads transcripts, prompts, tool inputs, and memory files. These may contain secrets.
- **`--once` output**: redact file contents from tool_use inputs. Show tool name + file path only, not content.
- **TUI mode**: show tool name + first arg (file path), never show file contents or prompt text in session list.
- **No network**: abtop never sends data anywhere. All local reads.

## Gotchas

- **Transcript size**: 1KB–18MB. On first load, full scan for totals. After that, track file offset and read only new bytes. Buffer partial lines.
- **Session file deletion**: files disappear when Claude exits. Handle `NotFound` between scan and read.
- **stats-cache.json is stale**: only updated on `/stats` command. Don't use for live data.
- **Context window not in data**: must hardcode per model. Will break if Anthropic adds new models.
- **Rate limit is account-level**: shared across all sessions. Don't show per-session.
- **Path encoding**: `/Users/foo/bar` → `-Users-foo-bar`. Used for transcript directory names.
- **lsof can be slow**: on macOS with many open files. Cache results, don't call every tick.
- **Child process tree**: `pgrep -P` only gets direct children. For deep trees, recurse or use `ps -o ppid`.
- **Port detection race**: a port can close between lsof and display. Show stale data gracefully.
- **Subagent directory may not exist**: only created when Agent tool is used. Check existence before scanning.
- **Undocumented internals**: all data sources are Claude Code implementation details, not stable APIs. Schema may change without notice. Defensive parsing with `serde(default)` everywhere. Log unknown fields, don't crash.
- **Terminal size**: minimum 80x24. Below that, hide panels progressively (ports → projects → tokens). Sessions panel always visible.
- **Path encoding collision**: `-Users-foo-bar-baz` could be `/Users/foo/bar-baz` or `/Users/foo-bar/baz`. Use session JSON's `cwd` as source of truth, not directory name.
