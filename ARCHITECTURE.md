# abtop — Architecture

`abtop` is a single-binary, terminal-based observability tool for AI coding agents (Claude Code, OpenAI Codex CLI, pi-go, and Cursor Agent). It works like `htop`/`btop` but tracks live agent sessions, token usage, context windows, rate limits, child processes, open ports and git status — all read-only from the local filesystem and from `ps`/`lsof`.

This document describes the runtime structure, components, data flow and key design decisions. For end-user docs see `README.md`; for an annotated layout walkthrough see `CLAUDE.md`.

---

## 1. High-level overview

```
                       ┌────────────────────────────────────────────┐
                       │                  main.rs                   │
                       │  flag parsing · terminal setup · event loop│
                       └────────────────────┬───────────────────────┘
                                            │
                                ┌───────────▼───────────┐
                                │        app::App       │
                                │  state · tick · keys  │
                                └─┬──────────┬──────────┘
                                  │          │
                ┌─────────────────┘          └─────────────────┐
                │                                              │
       ┌────────▼────────┐                            ┌────────▼────────┐
       │   collector::   │                            │       ui::      │
       │ MultiCollector  │                            │  ratatui draw   │
       └────────┬────────┘                            └────────┬────────┘
                │                                              │
   ┌────────────┼──────────────┬──────────────┐       ┌────────┴────────────────┐
   │            │              │              │       │ header context quota    │
┌──▼──┐ ┌───────▼──────┐ ┌─────▼────┐ ┌───────▼─────┐ │ tokens projects ports   │
│Claude│ │   Codex     │ │  pi-go   │ │   Cursor    │ │ sessions footer         │
│Coll. │ │   Coll.     │ │  Coll.   │ │   Coll.     │ └─────────────────────────┘
└──┬──┘ └───────┬──────┘ └────┬─────┘ └──────┬──────┘
   │            │             │              │
   ▼            ▼             ▼              ▼
~/.claude   ~/.codex      ~/.pi-go       ~/.cursor
sessions    sessions      sessions       projects/<enc>/
+ JSONL     rollout        (meta.json +   agent-transcripts/
transcripts JSONL           events.jsonl)  <uuid>/<uuid>.jsonl
                                     + ps / lsof / git
                                       (shared by all)
```

Three concerns are kept strictly separated:

- **collector** — read-only data acquisition (filesystem + Unix tools).
- **app** — pure state held in `App`, advanced once per *tick*.
- **ui** — stateless render of `App` into a `ratatui::Frame` each frame.

The render loop polls keyboard input every 500 ms (smooth animation), and re-collects data every 2 s. Slow I/O (port scan, git, rate-limit file) is staggered to every 5 ticks (~10 s).

---

## 2. Source tree

```
src/
├── main.rs              Entry point. CLI flag dispatch, terminal setup, event loop.
├── app.rs               Central App struct: state, tick(), key handlers,
│                        background summary generation, kill / jump actions.
├── config.rs            Read/write ~/.config/abtop/config.toml (theme persistence).
├── setup.rs             `abtop --setup`: install Claude StatusLine hook.
├── demo.rs              Offline demo data for screenshots and `--demo` mode.
├── theme.rs             10 themes (btop, dracula, …) + RGB gradients.
│
├── model/
│   ├── mod.rs           Re-exports.
│   └── session.rs       Domain types: AgentSession, SessionStatus,
│                        ChildProcess, OrphanPort, SubAgent, RateLimitInfo.
│
├── collector/
│   ├── mod.rs           AgentCollector trait + MultiCollector orchestrator,
│   │                    SharedProcessData cache, orphan-port detection.
│   ├── claude.rs        Claude Code: scan ~/.claude/sessions, tail JSONL
│   │                    transcripts incrementally, derive tokens / tasks /
│   │                    subagents / context-%.
│   ├── codex.rs         Codex CLI: discover via ps + lsof → open
│   │                    rollout-*.jsonl, parse session_meta / token_count /
│   │                    response_item / rate_limits.
│   ├── pi.rs            pi-go: discover via ps + lsof-cwd, match session
│   │                    dirs in ~/.pi-go/sessions by workDir, incremental
│   │                    events.jsonl parse (ADK session.Event format).
│   ├── cursor.rs        Cursor Agent: scan ~/.cursor/projects/<enc>/
│   │                    agent-transcripts, match to Cursor Helper
│   │                    `extension-host (agent-exec) <basename>` by
│   │                    project basename; tokens/model aren't written to
│   │                    the local transcript so those fields stay blank.
│   ├── process.rs       ps, lsof, git wrappers (ProcInfo, port map,
│   │                    children map, git stats).
│   └── rate_limit.rs    Read ~/.claude/abtop-rate-limits.json (Claude) and
│                        ~/.cache/abtop/codex-rate-limits.json (Codex cache).
│
└── ui/
    ├── mod.rs           Layout engine, gradient maths, braille sparkline /
    │                    area-graph helpers, btop-style block borders.
    ├── header.rs        Top status line (version, time, active counts).
    ├── context.rs       Token-rate sparkline + per-session context-% bars.
    ├── quota.rs         Claude + Codex 5h / 7d rate-limit gauges.
    ├── tokens.rs        Token totals + per-turn sparkline.
    ├── providers.rs     Cross-agent token usage broken down by LLM provider
    │                    (Anthropic / OpenAI / Google / Other) with a stacked legend.
    ├── projects.rs      Per-project git branch + +/~ counts.
    ├── ports.rs         Listening ports + ORPHAN PORTS section.
    ├── sessions.rs      Main session table + selected-session detail.
    └── footer.rs        Key hints + transient status messages.
```

---

## 3. Components

### 3.1 `main.rs` — entry, flags, terminal lifecycle

Responsibilities:
- Parse flags before any TUI setup: `--version`, `--update`, `--setup`, `--theme <name>`, `--demo`, `--once`.
- For `--setup` delegate to `setup::run_setup` and exit.
- For `--once` build an `App`, do one `tick()`, drain summary jobs (up to 30 s deadline) and `print_snapshot` to stdout — ideal for scripts.
- Otherwise, enter alternate screen + raw mode and call `run_app`.
- The render loop:
  - polls key events every 500 ms (`render_interval`),
  - calls `app.tick()` every 2 s when idle (`tick_interval`),
  - dispatches keys to `App` methods (`select_next/prev`, `quit`, `kill_selected`, `kill_orphan_ports`, `cycle_theme`, `jump_to_session`).

Terminal state is always restored, even if the inner loop returns an error.

### 3.2 `app::App` — central state

```rust
pub struct App {
    pub sessions: Vec<AgentSession>,        // current snapshot
    pub selected: usize,
    pub should_quit: bool,
    pub token_rates: VecDeque<f64>,         // 200-pt history for graph
    pub rate_limits: Vec<RateLimitInfo>,
    pub orphan_ports: Vec<OrphanPort>,
    pub summaries: HashMap<String, String>, // LLM-generated session titles
    pub status_msg: Option<(String, Instant)>,
    pub theme: Theme,
    // …private fields for prev token totals, retry counters,
    //   summary mpsc channel, kill-confirm timer, tick counters
    collector: MultiCollector,
}
```

Key behaviours:

- **`tick()`** — fetch sessions via `MultiCollector`, update orphan ports, recompute the per-tick token-rate (sum of `active_tokens` deltas), poll rate limits every 5 ticks, drain summary results, spawn new summary jobs.
- **Summary subsystem** — each session needs a 3–5 word title. Workers run `claude --print` with a 10 s timeout in `std::thread::spawn`, communicate via `mpsc::channel<(sid, prompt, Option<summary>)>`. Bounded to `MAX_SUMMARY_JOBS = 3` concurrent and `MAX_SUMMARY_RETRIES = 2`. Persists to `~/.cache/abtop/summaries.json`.
- **Kill confirmation** — `x` arms a 2 s confirm window per selected index; second `x` actually `kill -9`s the agent process.
- **Orphan kill** — `Shift+X` does a fresh `lsof` scan and verifies PID + listening port + full command string before sending SIGKILL (PID-reuse safety).
- **Jump-to-session** — when running inside `tmux`, walks `tmux list-panes -a` and matches each pane's PID through the `ps` parent chain to find the agent. Switches client + window + pane.

### 3.3 `model::session` — domain types

Pure data, no behaviour beyond formatting:

- **`AgentSession`** — the central record. Fields cover identity (`agent_cli`, `pid`, `session_id`, `cwd`, `project_name`, `started_at`), status, model + effort, tokens (input / output / cache_read / cache_create), context-%, turn count, current tool tasks, RAM, version, git stats, token history, subagents, memory file/line counts, open child processes, raw initial prompt + first assistant text.
- **`SessionStatus`** — `Working | Waiting | Done`.
- **`ChildProcess`**, **`SubAgent`**, **`OrphanPort`**, **`RateLimitInfo`** — auxiliary records.
- **`SessionFile`** — serde view of `~/.claude/sessions/{pid}.json`.

`AgentSession::total_tokens()` sums everything; `active_tokens()` excludes `cache_read` so the sparkline isn't dominated by cache hits.

### 3.4 `collector` — data acquisition

```
trait AgentCollector {
    fn collect(&mut self, shared: &SharedProcessData) -> Vec<AgentSession>;
    fn live_rate_limit(&self) -> Option<RateLimitInfo> { None }
}
```

Adding a new agent ≈ `impl AgentCollector for MyCollector` and registering it in `MultiCollector::new`. The repo ships four implementations (Claude, Codex, pi-go, Cursor) that each illustrate a different discovery strategy — see §3.4.

#### `SharedProcessData`
Fetched once per tick and passed to every collector to avoid duplicate `ps`/`lsof` calls:
- `process_info: HashMap<pid, ProcInfo>`  (pid, ppid, RSS, %CPU, command)
- `children_map: HashMap<ppid, Vec<pid>>`
- `ports: HashMap<pid, Vec<u16>>`         (LISTEN sockets only)

Ports are reused from cache unless the slow tick fires *or* the live PID set changes (PID reuse safety).

#### `MultiCollector`
- Owns the per-collector boxed trait objects (currently Claude + Codex).
- Drives the slow / fast tick split (`SLOW_POLL_INTERVAL = 5`).
- After collection: enriches each session with cached git stats, drops `Done` sessions, sorts by `started_at`.
- Runs **orphan-port detection**: tracks `(child_pid → port + command + project)` across ticks; when a child PID is no longer attached to a live session but is still listening on its port, it surfaces as an `OrphanPort`. Dead PIDs are pruned from the tracker.

#### `ClaudeCollector`
- Source: `~/.claude/sessions/*.json` (one tiny file per live session).
- Verifies the PID is alive *and* the command name contains `claude`; ignores `--print` invocations (which is what abtop uses for summary generation).
- For each session, finds the JSONL transcript under `~/.claude/projects/<encoded-cwd>/<sessionId>.jsonl`.
- **Incremental parser**: keeps a `transcript_cache` keyed by `session_id` storing the file's `(inode, mtime, len)` and parsed totals. On each tick only the bytes after the last offset are read; partial lines are buffered. If inode/mtime change, the file is re-parsed from scratch.
- Extracts: model + context window, cumulative tokens, last `tool_use` for the "current task" line, subagents directory, memory directory stats, version, git branch, initial prompt + first assistant text.

#### `CodexCollector`
- Codex doesn't write a per-session metadata file, so discovery is reversed:
  1. Find live `codex` processes from `SharedProcessData`.
  2. Use `lsof` (per PID) to find the open `rollout-*.jsonl`.
  3. Parse JSONL events: `session_meta` (id / cwd / version / git), `turn_context` (model / effort / context window), `event_msg::token_count` (totals + `rate_limits`), `response_item` (assistant + function calls + outputs).
- "Recently finished" sessions are also surfaced: today's rollouts not owned by any live process appear briefly as Done, then are filtered out by `MultiCollector`.
- Latest `token_count.rate_limits` is exposed via `live_rate_limit()` and persisted to `~/.cache/abtop/codex-rate-limits.json` so the quota gauge survives between sessions.

#### `PiCollector`
- Source: `~/.pi-go/sessions/<uuid>/{meta.json,events.jsonl}`.
- pi-go neither writes a pid file nor keeps `events.jsonl` open (every `AppendEvent` re-opens and closes it), so neither Claude's pid-file trick nor Codex's lsof-on-rollout trick works. Discovery instead goes through *cwd*:
  1. Find live `pi` processes from `SharedProcessData` (`cmd_has_binary(cmd, "pi")` is basename-exact, so `pip` / `ping` / `python` are safely excluded).
  2. Resolve each pi's cwd with `lsof -a -d cwd -Fn -p <pid>`.
  3. Read every `meta.json` under `~/.pi-go/sessions/*/`, sort by `updatedAt` descending, and pair each live pi pid with the newest session whose `workDir` equals its cwd.
- Incremental parser: caches `(inode, mtime)` + `new_offset` per session and only reads appended bytes each tick. pi-go's `UsageMetadata` is per-turn (not cumulative) so `promptTokenCount` / `candidatesTokenCount` are *summed* across events into running totals.
- Extracts: model (from `meta.Model` or `ModelVersion`), per-turn tokens, last `functionCall.{name,args}` → "current task", first user text → `initial_prompt`, first model text → `first_assistant_text`, count of `TurnComplete` model events → `turn_count`.
- Context window is approximated from the model name (`gemini-*` → 1M, `claude-*` → 200k / 1M, `gpt-*` → 128k); `0%` when the model is unknown or blank.
- Sessions not currently owned by a live `pi` process but updated in the last 5 min surface briefly as Done (same UX as Codex), then disappear.

#### `CursorCollector`
- Source: `~/.cursor/projects/<encoded-cwd>/agent-transcripts/<uuid>/<uuid>.jsonl` — one JSONL file per Cursor Agent conversation. Each line is a `{"role":"user"|"assistant","message":{"content":[…]}}` turn.
- Cursor Agent is the fourth discovery pattern: no pid file (Claude), no open rollout (Codex), no cwd-on-process (pi-go). Instead it's discovered by *process basename*:
  1. Enumerate every project dir under `~/.cursor/projects/` whose name looks path-encoded (`Users-…`, `home-…`, …, rejecting purely numeric workspace ids like `1770471792203`).
  2. Pick the most recently modified `<uuid>.jsonl` per project (freshness window: 10 min).
  3. Match each project to a live `Cursor Helper (Plugin): extension-host (agent-exec) <basename>` PID by the basename that appears in both — e.g. `abtop` ↔ `Users-dimetron-p6s-pi-dev-abtop`.
- The encoded-cwd → real-path decode is **lossy** (both `/` and `.` become `-`; long paths get a 7-char hex suffix). `decode_project_cwd` tries the naive `-` → `/` substitution, falls back to stripping the hex suffix, and — if neither hits the filesystem — keeps the naive decode for display. Exact path fidelity isn't critical because `git_branch` etc. are keyed off the *existing* cwd paths anyway.
- Cursor writes **no** token counts, no model name and no rate-limit data into the local transcript (all of that lives server-side), so the collector reports `model = "-"`, `tokens = 0` and no rate-limit entry. The UI surfaces everything else: project, status, initial prompt, first assistant text, latest `tool_use` → current task, turn count (one per user→assistant transition, so multi-part assistant replies don't inflate the counter), children of the extension-host and any ports they own.
- Same incremental parse pattern as the other collectors: `(inode, mtime)` + `new_offset`, partial trailing lines buffered for the next tick, Cursor's `<user_query>…</user_query>` wrapper stripped from the initial prompt display.

#### `process.rs`
Thin wrappers over `ps -ww -eo pid,ppid,rss,%cpu,command`, `lsof -i -P -n -sTCP:LISTEN` and `git -C <cwd> status --porcelain`. Provides `cmd_has_binary(cmd, name)` for safe binary-name matching against the first two argv tokens (handles interpreter wrappers like `node /path/to/codex …`).

#### `rate_limit.rs`
- Reads the Claude file written by the StatusLine hook (`~/.claude/abtop-rate-limits.json`); rejects entries older than 10 minutes.
- Codex cache uses an atomic write (write `.tmp` → `rename`) and is *not* staleness-checked because the embedded `resets_at` already conveys validity.

### 3.5 `setup.rs` — Claude StatusLine hook

`abtop --setup` is a one-shot installer:
1. Creates `~/.claude/abtop-statusline.sh` (a small bash wrapper around a Python one-liner) and `chmod +x`.
2. Patches `~/.claude/settings.json` to register the script under the `statusLine` key. Refuses to overwrite an unrelated existing `statusLine` entry.

The hook runs after each Claude turn, receives JSON on stdin, extracts `rate_limits.{five_hour,seven_day}` and writes a small JSON file that `rate_limit::read_rate_limits` later picks up. This is the only way to get account-level Claude rate-limits (they aren't in the transcript JSONL).

### 3.6 `ui` — rendering

`ui::draw` lays out the screen each frame based on terminal size, with explicit minimums (`MIN_WIDTH = 100`, `MIN_HEIGHT = 24`) and a friendly error block when below those.

**Layout priority** (top → bottom):

1. **Sessions** panel — always shown, gets ideal height first (`2 × sessions + 7`).
2. **Mid row** — quota, tokens, projects, ports (minimum 6 rows). Terminals ≥ 160 cols also get a **providers** panel slotted between tokens and projects; the column splits reweight toward the info-dense panels on ≥ 220 cols.
3. **Context** panel (sparkline + per-session bars) — only if sessions are at ideal height *and* surplus ≥ 5 rows.
4. **Header** (1 row) and **Footer** (1 row) — always present.

`ui/mod.rs` also provides shared rendering primitives:
- `make_gradient(start, mid, end) -> [Color; 101]` — btop-faithful linear-RGB 101-step gradient.
- `meter_bar` / `remaining_bar` — square-block meters using `■`.
- `braille_sparkline` — 1-row braille graph (5×5 lookup `BRAILLE_UP`).
- `braille_graph_multirow` — multi-row filled braille area graph (used for the token-rate panel).
- `btop_block(title, number)` — rounded border with notch numbering identical to btop.

**`providers` panel.** Aggregates `total_tokens()` across every live session into four provider buckets (Anthropic, OpenAI, Google, Other). `provider_for_session` uses `agent_cli` as the primary signal — Claude Code always maps to Anthropic, Codex CLI to OpenAI — and falls back to model-name inference for `pi-go` (which can dispatch to any backend). Cursor sessions report no token telemetry locally and sit in "Other" with a session count but zero tokens. The panel renders four per-provider meter bars plus a stacked legend bar at the bottom; the stacked bar is floor-allocated with a remainder pass so it always fills exactly `width` cells regardless of rounding. See `src/ui/providers.rs`.

### 3.7 `theme.rs` + `config.rs`

10 themes (`THEME_NAMES`): `btop`, `dracula`, `catppuccin`, `tokyo-night`, `gruvbox`, `nord`, `high-contrast`, `protanopia`, `deuteranopia`, `tritanopia`. Each is a `Theme` struct of `ratatui::style::Color`s + three `Gradient`s (cpu / proc / used).

`t` cycles themes at runtime; selection is persisted to `~/.config/abtop/config.toml` via a minimal hand-rolled key=value reader/writer (no `toml` crate dependency).

### 3.8 `demo.rs`

Used by `--demo` (and by the GIF tape in `assets/demo.tape`). Populates `App` with deterministic fixtures and fakes the sparkline animation by rotating `token_rates`.

---

## 4. Data flow per tick

```
                  every 2 s                     every 500 ms
                     │                              │
                     ▼                              ▼
              ┌─────────────┐               ┌──────────────┐
              │ App::tick() │               │ event::poll  │
              └──────┬──────┘               └──────┬───────┘
                     │                              │
        ┌────────────┴────────────┐                 ▼
        ▼                         ▼          key handler →
 SharedProcessData        ClaudeCollector   App methods
  (ps, lsof,              CodexCollector            │
   children, ports)              │                  ▼
        │                        ▼          terminal.draw(|f| ui::draw(f, &app))
        └──────► MultiCollector::collect
                          │
                          ▼
              Vec<AgentSession>  +  Vec<OrphanPort>
                          │
                          ▼
                  enrich with git
                  drop Done sessions
                          │
                          ▼
              app.sessions / app.orphan_ports
                          │
                          ▼
              token-rate delta → token_rates
              every 5 ticks  → rate_limits
              drain summary mpsc
              spawn new summary threads (≤3)
```

---

## 5. Cross-cutting concerns

- **Read-only by default.** No API keys. The only network call is the indirect `claude --print` used to summarise session titles, and it's bounded (`10 s` timeout, `≤3` concurrent, `≤2` retries).
- **PID-reuse safety.** All actions on a PID re-verify (a) the process is still alive, (b) the command is still the one we expected. This protects `kill_selected`, `kill_orphan_ports` and the parent-chain walk in `jump_to_session`.
- **I/O cost control.** A two-tier polling cadence (2 s fast / 10 s slow), per-collector caches (transcript bytes, git stats, port map) and a single `SharedProcessData` per tick keep `ps` / `lsof` / `git` invocations bounded.
- **Atomic writes.** The Codex rate-limit cache writes to `*.tmp` and renames so concurrent reads never see a partial file.
- **Graceful degradation.** Each panel hides if the terminal shrinks; rate-limit and summary subsystems all have visible fallback strings instead of panicking when data is missing.

---

## 6. Extension points

| What you want                | Where                                                      |
| ---------------------------- | ---------------------------------------------------------- |
| Add a new agent (e.g. Aider) | `impl AgentCollector` in a new `src/collector/<name>.rs`, register it in `MultiCollector::new`, add a label in `ui/sessions.rs` (`pi.rs` shows a cwd-based discovery strategy; `cursor.rs` shows a basename-matched strategy when no usage telemetry is available locally) |
| Add a new theme              | New constructor on `Theme` + entry in `THEME_NAMES`        |
| Add a new panel              | New file in `src/ui/`, layout slot in `ui::mod::draw`      |
| New context-window model     | Extend the model→size mapping in the Claude / Codex collector |
| New rate-limit source        | New file in `src/collector/rate_limit.rs` style + a call in `App::tick` |
| New CLI flag                 | Add a branch in `main::main` before terminal setup         |

---

## 7. Tech stack

- **Rust 2021** (MSRV 1.88)
- **ratatui 0.29** + **crossterm 0.28** for the TUI
- **serde / serde_json** for JSON / JSONL parsing
- **chrono** for timestamps, **dirs** for HOME / cache / config resolution
- **tokio** is included but the runtime is not used today; concurrency is handled with `std::thread::spawn` + `std::sync::mpsc`
- External tools at runtime: `ps`, `lsof`, `git`, optionally `tmux`, optionally `claude` (for summary generation)
- Distribution via `cargo-dist` (see `dist-workspace.toml`) and the GitHub Releases installer in `README.md`
