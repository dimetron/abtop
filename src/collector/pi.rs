use super::process::{self, ProcInfo};
use crate::model::{AgentSession, ChildProcess, SessionStatus};
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Collector for the `pi-go` coding agent (binary name: `pi`).
///
/// Session layout on disk: `~/.pi-go/sessions/<uuid>/{meta.json,events.jsonl,branches.json,trajectory.atif.json}`
///
/// - `meta.json` — `{id, appName, userID, workDir, model, createdAt, updatedAt}`.
/// - `events.jsonl` — one ADK `session.Event` per line (append-only). Events
///   carry `Content.parts[].{text|functionCall|functionResponse}`, per-turn
///   `UsageMetadata.{promptTokenCount, candidatesTokenCount}`, `Author`,
///   `Timestamp`, `TurnComplete`, `FinishReason`, etc.
///
/// Discovery strategy (pi-go does not write a pid file, nor does it keep
/// `events.jsonl` open — each append re-opens the file):
/// 1. Find live `pi` processes from shared `ps` data.
/// 2. Resolve each process's cwd via `lsof -a -d cwd -Fn -p <pid>`.
/// 3. Scan `~/.pi-go/sessions/*/meta.json`, match the most-recently-updated
///    session whose `workDir` equals a live pi process cwd.
/// 4. Recently-updated sessions (< 5 min) not owned by any live `pi` are
///    surfaced briefly as Done (same UX as Codex).
pub struct PiCollector {
    sessions_dir: PathBuf,
    /// Incremental parse cache, keyed by `session_id` (the uuid dir name).
    transcript_cache: HashMap<String, TranscriptResult>,
}

impl PiCollector {
    pub fn new() -> Self {
        let base = dirs::home_dir().unwrap_or_default().join(".pi-go");
        Self {
            sessions_dir: base.join("sessions"),
            transcript_cache: HashMap::new(),
        }
    }

    fn collect_sessions(&mut self, shared: &super::SharedProcessData) -> Vec<AgentSession> {
        if !self.sessions_dir.exists() {
            return vec![];
        }

        // 1. Find live `pi` processes and their cwds.
        let pi_pids = Self::find_pi_pids(&shared.process_info);
        let pid_to_cwd = Self::map_pid_to_cwd(&pi_pids);

        // cwd -> (pid, started_at_unix_secs) for picking which session belongs
        // to which pi process (newest-wins when multiple pi instances share a cwd).
        let mut cwd_index: HashMap<String, Vec<u32>> = HashMap::new();
        for (pid, cwd) in &pid_to_cwd {
            cwd_index.entry(cwd.clone()).or_default().push(*pid);
        }

        // 2. Enumerate session dirs. Keep only those whose meta can be read.
        let mut metas: Vec<(PathBuf, MetaFile)> = Vec::new();
        if let Ok(entries) = fs::read_dir(&self.sessions_dir) {
            for entry in entries.flatten() {
                let dir = entry.path();
                if !dir.is_dir() {
                    continue;
                }
                let meta_path = dir.join("meta.json");
                if let Some(meta) = read_meta(&meta_path) {
                    metas.push((dir, meta));
                }
            }
        }

        // 3. Pair each live pi pid with the most-recently-updated session in
        //    its cwd. A pid wins over any un-paired session.
        // Sort metas by updatedAt desc so we pick newest first.
        metas.sort_by(|a, b| b.1.updated_at_ms.cmp(&a.1.updated_at_ms));

        let mut pid_to_session: HashMap<u32, (PathBuf, MetaFile)> = HashMap::new();
        let mut used_sessions: std::collections::HashSet<String> = std::collections::HashSet::new();

        // Track which PIDs already got a session — first match wins because
        // metas is sorted newest-first.
        let mut remaining_pids: std::collections::HashMap<String, Vec<u32>> = cwd_index
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        for (dir, meta) in &metas {
            if meta.work_dir.is_empty() {
                continue;
            }
            if let Some(pids) = remaining_pids.get_mut(&meta.work_dir) {
                if let Some(pid) = pids.pop() {
                    used_sessions.insert(meta.id.clone());
                    pid_to_session.insert(pid, (dir.clone(), meta.clone()));
                }
            }
        }

        let mut sessions = Vec::new();

        // 4. Active sessions (pid-owned).
        for (pid, (dir, meta)) in &pid_to_session {
            if let Some(session) = self.load_session(
                Some(*pid),
                dir,
                meta,
                &shared.process_info,
                &shared.children_map,
                &shared.ports,
            ) {
                sessions.push(session);
            }
        }

        // 5. Recently-finished sessions: show for up to 5 min so they transition
        //    to Done instead of vanishing the instant the pi process exits.
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        for (dir, meta) in &metas {
            if used_sessions.contains(&meta.id) {
                continue;
            }
            if meta.updated_at_ms == 0 || now_ms.saturating_sub(meta.updated_at_ms) > 5 * 60 * 1000 {
                continue;
            }
            if let Some(session) = self.load_session(
                None,
                dir,
                meta,
                &shared.process_info,
                &shared.children_map,
                &shared.ports,
            ) {
                sessions.push(session);
            }
        }

        // 6. Evict cache entries for sessions that no longer exist on disk
        //    (the user deleted the dir) to avoid unbounded growth.
        let live_ids: std::collections::HashSet<&str> =
            sessions.iter().map(|s| s.session_id.as_str()).collect();
        self.transcript_cache.retain(|sid, _| live_ids.contains(sid.as_str()));

        sessions.sort_by_key(|s| std::cmp::Reverse(s.started_at));
        sessions
    }

    fn load_session(
        &mut self,
        pid: Option<u32>,
        session_dir: &Path,
        meta: &MetaFile,
        process_info: &HashMap<u32, ProcInfo>,
        children_map: &HashMap<u32, Vec<u32>>,
        ports: &HashMap<u32, Vec<u16>>,
    ) -> Option<AgentSession> {
        let events_path = session_dir.join("events.jsonl");

        // Incremental parse: reuse cached totals and offset when file identity
        // matches; otherwise reparse from scratch.
        let cached = self.transcript_cache.remove(&meta.id);
        let identity_changed = cached
            .as_ref()
            .map(|c| c.file_identity != file_identity(&events_path))
            .unwrap_or(false);
        let from_offset = if identity_changed {
            0
        } else {
            cached.as_ref().map(|c| c.new_offset).unwrap_or(0)
        };

        let base = if identity_changed { None } else { cached };
        let result = parse_events(&events_path, from_offset, base);

        // started_at from meta.createdAt (fallback to updatedAt).
        let started_at = if meta.created_at_ms > 0 {
            meta.created_at_ms
        } else {
            meta.updated_at_ms
        };

        let project_name = meta
            .work_dir
            .rsplit('/')
            .next()
            .unwrap_or("?")
            .to_string();

        let proc = pid.and_then(|p| process_info.get(&p));
        let mem_mb = proc.map(|p| p.rss_kb / 1024).unwrap_or(0);
        let display_pid = pid.unwrap_or(0);

        // Status detection — mirrors Codex semantics.
        let pid_alive = proc.is_some();
        let status = if !pid_alive {
            SessionStatus::Done
        } else {
            let since_activity = std::time::SystemTime::now()
                .duration_since(result.last_activity)
                .unwrap_or_default();
            if since_activity.as_secs() < 30 {
                SessionStatus::Working
            } else {
                let cpu_active = proc.is_some_and(|p| p.cpu_pct > 1.0);
                let has_active_child = pid.is_some_and(|p| {
                    process::has_active_descendant(p, children_map, process_info, 5.0)
                });
                if cpu_active || has_active_child {
                    SessionStatus::Working
                } else {
                    SessionStatus::Waiting
                }
            }
        };

        let current_tasks = if !result.current_task.is_empty() {
            vec![result.current_task.clone()]
        } else if !pid_alive {
            vec!["finished".to_string()]
        } else if matches!(status, SessionStatus::Waiting) {
            vec!["waiting for input".to_string()]
        } else {
            vec!["thinking...".to_string()]
        };

        // Children: all descendants, tagged with any listening port they own.
        let mut children = Vec::new();
        if let Some(p) = pid {
            let mut stack: Vec<u32> = children_map.get(&p).cloned().unwrap_or_default();
            while let Some(cpid) = stack.pop() {
                if let Some(cproc) = process_info.get(&cpid) {
                    let port = ports.get(&cpid).and_then(|v| v.first().copied());
                    children.push(ChildProcess {
                        pid: cpid,
                        command: cproc.command.clone(),
                        mem_kb: cproc.rss_kb,
                        port,
                    });
                }
                if let Some(grandchildren) = children_map.get(&cpid) {
                    stack.extend(grandchildren);
                }
            }
        }

        // Model + context window. pi-go's meta.Model is often blank; fall back
        // to the string captured from events (response model version), else "-".
        let model = if !meta.model.is_empty() {
            meta.model.clone()
        } else if !result.model.is_empty() {
            result.model.clone()
        } else {
            "-".to_string()
        };
        let context_window = context_window_for_model(&model);
        let context_percent = if context_window > 0 && result.last_context_tokens > 0 {
            (result.last_context_tokens as f64 / context_window as f64) * 100.0
        } else {
            0.0
        };

        let session = AgentSession {
            agent_cli: "pi",
            pid: display_pid,
            session_id: meta.id.clone(),
            cwd: meta.work_dir.clone(),
            project_name,
            started_at,
            status,
            model,
            effort: String::new(),
            context_percent,
            total_input_tokens: result.total_input,
            total_output_tokens: result.total_output,
            total_cache_read: 0,
            total_cache_create: 0,
            turn_count: result.turn_count,
            current_tasks,
            mem_mb,
            version: String::new(),
            git_branch: String::new(), // populated by MultiCollector via git CLI
            git_added: 0,
            git_modified: 0,
            token_history: result.token_history.clone(),
            subagents: vec![],
            mem_file_count: 0,
            mem_line_count: 0,
            children,
            initial_prompt: result.initial_prompt.clone(),
            first_assistant_text: result.first_assistant_text.clone(),
        };

        self.transcript_cache.insert(meta.id.clone(), result);
        Some(session)
    }

    /// Find live `pi` processes from shared ps data. `cmd_has_binary` does
    /// basename-exact matching on the first two argv tokens, so `pip`,
    /// `ping` and `python` are safely excluded.
    fn find_pi_pids(process_info: &HashMap<u32, ProcInfo>) -> Vec<u32> {
        let mut pids = Vec::new();
        for (pid, info) in process_info {
            let cmd = &info.command;
            if !process::cmd_has_binary(cmd, "pi") {
                continue;
            }
            // Extra guard: skip our own discovery helpers. `pi` alone is
            // an ambiguous binary name, so also reject anything that looks
            // like a helper subcommand we know about.
            if cmd.contains("pi-sandbox") {
                continue;
            }
            pids.push(*pid);
        }
        pids
    }

    /// Resolve the cwd of each PID via `lsof -a -d cwd -Fn -p <pid>`.
    /// Output format (one line per field):
    ///   p<pid>
    ///   f<fd>     (not emitted for cwd; still tolerated)
    ///   n<path>
    fn map_pid_to_cwd(pids: &[u32]) -> HashMap<u32, String> {
        let mut map = HashMap::new();
        if pids.is_empty() {
            return map;
        }

        let pid_args: Vec<String> = pids.iter().map(|p| format!("-p{}", p)).collect();
        let mut args: Vec<&str> = vec!["-a", "-d", "cwd", "-Fn"];
        for pa in &pid_args {
            args.push(pa);
        }

        let output = match Command::new("lsof").args(&args).output() {
            Ok(o) => o,
            Err(_) => return map,
        };
        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut current_pid: Option<u32> = None;
        for line in stdout.lines() {
            if let Some(rest) = line.strip_prefix('p') {
                current_pid = rest.parse::<u32>().ok();
            } else if let Some(name) = line.strip_prefix('n') {
                if let Some(pid) = current_pid {
                    map.entry(pid).or_insert_with(|| name.to_string());
                }
            }
        }
        map
    }
}

impl super::AgentCollector for PiCollector {
    fn collect(&mut self, shared: &super::SharedProcessData) -> Vec<AgentSession> {
        self.collect_sessions(shared)
    }
}

// ── meta.json ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct MetaFile {
    id: String,
    work_dir: String,
    model: String,
    created_at_ms: u64,
    updated_at_ms: u64,
}

fn read_meta(path: &Path) -> Option<MetaFile> {
    let content = fs::read_to_string(path).ok()?;
    let v: Value = serde_json::from_str(&content).ok()?;
    let id = v["id"].as_str()?.to_string();
    Some(MetaFile {
        id,
        work_dir: v["workDir"].as_str().unwrap_or("").to_string(),
        model: v["model"].as_str().unwrap_or("").to_string(),
        created_at_ms: rfc3339_to_ms(v["createdAt"].as_str()),
        updated_at_ms: rfc3339_to_ms(v["updatedAt"].as_str()),
    })
}

fn rfc3339_to_ms(ts: Option<&str>) -> u64 {
    match ts {
        Some(s) => chrono::DateTime::parse_from_rfc3339(s)
            .map(|dt| dt.timestamp_millis() as u64)
            .unwrap_or(0),
        None => 0,
    }
}

// ── events.jsonl parsing (incremental) ───────────────────────────────────────

struct TranscriptResult {
    model: String,
    total_input: u64,
    total_output: u64,
    /// Last model turn's prompt token count (input+cache equivalent) for context %.
    last_context_tokens: u64,
    turn_count: u32,
    current_task: String,
    last_activity: std::time::SystemTime,
    new_offset: u64,
    file_identity: (u64, u64),
    token_history: Vec<u64>,
    initial_prompt: String,
    first_assistant_text: String,
}

impl TranscriptResult {
    fn empty() -> Self {
        Self {
            model: String::new(),
            total_input: 0,
            total_output: 0,
            last_context_tokens: 0,
            turn_count: 0,
            current_task: String::new(),
            last_activity: std::time::UNIX_EPOCH,
            new_offset: 0,
            file_identity: (0, 0),
            token_history: Vec::new(),
            initial_prompt: String::new(),
            first_assistant_text: String::new(),
        }
    }
}

fn file_identity(path: &Path) -> (u64, u64) {
    fs::metadata(path)
        .ok()
        .map(|m| {
            let ino = m.ino();
            let mtime_ns = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0);
            (ino, mtime_ns)
        })
        .unwrap_or((0, 0))
}

/// Parse events.jsonl from `from_offset` to EOF, accumulating into `base`
/// (defaults to an empty result). pi-go's `UsageMetadata` is per-turn, so
/// we sum into the running totals rather than replacing.
fn parse_events(path: &Path, from_offset: u64, base: Option<TranscriptResult>) -> TranscriptResult {
    let mut result = base.unwrap_or_else(TranscriptResult::empty);
    result.file_identity = file_identity(path);

    let mut file = match fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return result,
    };
    let file_len = file.metadata().ok().map(|m| m.len()).unwrap_or(0);
    let seek_to = from_offset.min(file_len);
    if file.seek(SeekFrom::Start(seek_to)).is_err() {
        return result;
    }
    let reader = BufReader::new(&mut file);

    let mut bytes_consumed: u64 = seek_to;
    let mut trailing_partial = false;

    for line_result in reader.lines() {
        let line = match line_result {
            Ok(l) => l,
            Err(_) => break,
        };
        // Track raw length (+1 for newline). If this is a partial tail without
        // a trailing newline, BufReader still yields it — detect and exclude.
        let raw_len = line.len() as u64 + 1;
        if line.trim().is_empty() {
            bytes_consumed += raw_len;
            continue;
        }
        let parsed: Option<Value> = serde_json::from_str(&line).ok();
        if parsed.is_none() {
            // Partial JSON at EOF — stop, don't advance offset past it.
            trailing_partial = true;
            break;
        }
        bytes_consumed += raw_len;
        let val = parsed.unwrap();

        // Timestamp → last_activity
        if let Some(ts_str) = val["Timestamp"].as_str() {
            if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(ts_str) {
                let sys_time = std::time::UNIX_EPOCH
                    + std::time::Duration::from_millis(dt.timestamp_millis() as u64);
                if sys_time > result.last_activity {
                    result.last_activity = sys_time;
                }
            }
        }

        // ModelVersion (often empty, but capture when present).
        if let Some(m) = val["ModelVersion"].as_str() {
            if !m.is_empty() && result.model.is_empty() {
                result.model = m.to_string();
            }
        }

        let content = &val["Content"];
        let role = content["role"].as_str().unwrap_or("");
        let author = val["Author"].as_str().unwrap_or("");
        let is_model_turn = role == "model" || author == "pi";

        // UsageMetadata: per-turn on model events. Sum into running totals.
        let usage = &val["UsageMetadata"];
        if usage.is_object() {
            let prompt = usage["promptTokenCount"].as_u64().unwrap_or(0);
            let out = usage["candidatesTokenCount"].as_u64().unwrap_or(0);
            if prompt > 0 || out > 0 {
                result.total_input = result.total_input.saturating_add(prompt);
                result.total_output = result.total_output.saturating_add(out);
                result.last_context_tokens = prompt; // last turn's prompt size
                result.token_history.push(prompt + out);
            }
        }

        // Count completed turns.
        if val["TurnComplete"].as_bool().unwrap_or(false) && is_model_turn {
            result.turn_count = result.turn_count.saturating_add(1);
        }

        // Walk parts: first user text → initial_prompt, first model text →
        // first_assistant_text, latest functionCall → current_task.
        if let Some(parts) = content["parts"].as_array() {
            for part in parts {
                if let Some(text) = part["text"].as_str() {
                    let text = text.trim();
                    if text.is_empty() {
                        continue;
                    }
                    if role == "user" && result.initial_prompt.is_empty() {
                        result.initial_prompt = text.chars().take(200).collect();
                    } else if is_model_turn && result.first_assistant_text.is_empty() {
                        result.first_assistant_text = text.chars().take(200).collect();
                    }
                    continue;
                }
                if let Some(fc) = part.get("functionCall") {
                    let name = fc["name"].as_str().unwrap_or("");
                    if name.is_empty() {
                        continue;
                    }
                    let arg_hint = fc["args"]
                        .as_object()
                        .and_then(|m| {
                            m.get("file_path")
                                .and_then(|v| v.as_str())
                                .or_else(|| m.get("path").and_then(|v| v.as_str()))
                                .or_else(|| m.get("cmd").and_then(|v| v.as_str()))
                                .or_else(|| m.get("pattern").and_then(|v| v.as_str()))
                                .map(|s| s.to_string())
                        })
                        .unwrap_or_default();
                    result.current_task = if arg_hint.is_empty() {
                        name.to_string()
                    } else {
                        let short = arg_hint.rsplit('/').next().unwrap_or(&arg_hint);
                        format!("{} {}", name, short)
                    };
                }
            }
        }
    }

    // If we broke early on a partial line, bytes_consumed is the last known
    // good line boundary. Otherwise it should equal file_len.
    result.new_offset = if trailing_partial {
        bytes_consumed
    } else {
        bytes_consumed.min(file_len)
    };
    result
}

/// Best-effort model → context window size. Returns 0 when unknown so the
/// UI shows 0%. pi-go supports multiple providers (Gemini, Anthropic, Ollama),
/// and `meta.Model` is often blank, so this stays conservative.
fn context_window_for_model(model: &str) -> u64 {
    let m = model.to_ascii_lowercase();
    if m.contains("gemini-2.5") || m.contains("gemini-1.5") {
        1_000_000
    } else if m.contains("gemini") {
        200_000
    } else if m.contains("claude-opus") || m.contains("claude-sonnet") || m.contains("claude-haiku") {
        if m.contains("[1m]") { 1_000_000 } else { 200_000 }
    } else if m.contains("gpt-5") || m.contains("gpt-4") {
        128_000
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_events(file: &mut tempfile::NamedTempFile, lines: &[&str]) {
        for line in lines {
            writeln!(file, "{}", line).unwrap();
        }
        file.flush().unwrap();
    }

    #[test]
    fn test_rfc3339_to_ms() {
        let ms = rfc3339_to_ms(Some("2026-04-09T14:10:01.384873+02:00"));
        assert!(ms > 0);
        assert_eq!(rfc3339_to_ms(None), 0);
        assert_eq!(rfc3339_to_ms(Some("garbage")), 0);
    }

    #[test]
    fn test_parse_user_then_model_turn() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        write_events(&mut file, &[
            r#"{"Content":{"parts":[{"text":"Find TODO comments"}],"role":"user"},"Timestamp":"2026-04-09T14:10:00Z","Author":"user"}"#,
            r#"{"Content":{"parts":[{"functionCall":{"name":"grep","args":{"pattern":"TODO"}}}],"role":"model"},"UsageMetadata":{"promptTokenCount":1005,"candidatesTokenCount":181},"TurnComplete":true,"Timestamp":"2026-04-09T14:10:14Z","Author":"pi"}"#,
        ]);
        let result = parse_events(file.path(), 0, None);
        assert_eq!(result.initial_prompt, "Find TODO comments");
        assert_eq!(result.total_input, 1005);
        assert_eq!(result.total_output, 181);
        assert_eq!(result.last_context_tokens, 1005);
        assert_eq!(result.turn_count, 1);
        assert_eq!(result.current_task, "grep TODO");
        assert_eq!(result.token_history, vec![1186]);
    }

    #[test]
    fn test_incremental_accumulates_tokens() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        write_events(&mut file, &[
            r#"{"Content":{"parts":[{"text":"first"}],"role":"user"},"Timestamp":"2026-04-09T14:10:00Z","Author":"user"}"#,
            r#"{"Content":{"parts":[{"text":"ok"}],"role":"model"},"UsageMetadata":{"promptTokenCount":100,"candidatesTokenCount":50},"TurnComplete":true,"Timestamp":"2026-04-09T14:10:01Z","Author":"pi"}"#,
        ]);
        let first = parse_events(file.path(), 0, None);
        assert_eq!(first.total_input, 100);
        assert_eq!(first.total_output, 50);
        assert_eq!(first.turn_count, 1);

        // Append a new turn. new_offset should let us skip already-parsed bytes.
        let more = "\n".to_string()
            + r#"{"Content":{"parts":[{"text":"next"}],"role":"user"},"Timestamp":"2026-04-09T14:10:02Z","Author":"user"}"#
            + "\n"
            + r#"{"Content":{"parts":[{"text":"ok2"}],"role":"model"},"UsageMetadata":{"promptTokenCount":300,"candidatesTokenCount":20},"TurnComplete":true,"Timestamp":"2026-04-09T14:10:03Z","Author":"pi"}"#;
        std::fs::OpenOptions::new()
            .append(true)
            .open(file.path())
            .unwrap()
            .write_all(more.as_bytes())
            .unwrap();

        let second = parse_events(file.path(), first.new_offset, Some(first));
        assert_eq!(second.total_input, 400);
        assert_eq!(second.total_output, 70);
        assert_eq!(second.turn_count, 2);
        assert_eq!(second.last_context_tokens, 300);
    }

    #[test]
    fn test_read_meta() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("meta.json");
        std::fs::write(
            &path,
            r#"{"id":"abc","appName":"pi-go","userID":"local","workDir":"/tmp/proj","createdAt":"2026-04-09T14:10:00Z","updatedAt":"2026-04-09T14:11:00Z"}"#,
        )
        .unwrap();
        let meta = read_meta(&path).unwrap();
        assert_eq!(meta.id, "abc");
        assert_eq!(meta.work_dir, "/tmp/proj");
        assert!(meta.created_at_ms > 0);
        assert!(meta.updated_at_ms > meta.created_at_ms);
    }

    #[test]
    fn test_context_window_for_model() {
        assert_eq!(context_window_for_model("gemini-2.5-pro"), 1_000_000);
        assert_eq!(context_window_for_model("claude-opus-4-6"), 200_000);
        assert_eq!(context_window_for_model("claude-opus-4-6[1m]"), 1_000_000);
        assert_eq!(context_window_for_model("gpt-5-codex"), 128_000);
        assert_eq!(context_window_for_model(""), 0);
        assert_eq!(context_window_for_model("-"), 0);
    }
}
