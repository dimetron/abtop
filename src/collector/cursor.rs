use super::process::{self, ProcInfo};
use crate::model::{AgentSession, ChildProcess, SessionStatus};
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// Collector for the Cursor Agent (the in-IDE assistant in Cursor.app).
///
/// Session layout on disk:
///   `~/.cursor/projects/<encoded-cwd>/agent-transcripts/<uuid>/<uuid>.jsonl`
///
/// Each JSONL line is a role-tagged message:
///   ```json
///   {"role":"user",     "message":{"content":[{"type":"text","text":"..."}]}}
///   {"role":"assistant","message":{"content":[
///       {"type":"text","text":"..."},
///       {"type":"tool_use","name":"Read","input":{"path":"..."}}
///   ]}}
///   ```
///
/// Cursor writes **no** token counts, no model name and no rate-limit data
/// into these transcripts — everything lives server-side. So this collector
/// reports 0 tokens, `model = "-"`, and the UI hides the usage fields
/// gracefully (same as pi-go with an unknown model).
///
/// ## Discovery strategy
///
/// Cursor's per-workspace agent runs as:
///   `Cursor Helper (Plugin): extension-host (agent-exec) <basename> [N-NN]`
/// — the cwd of that process is `/` (useless), but the argv contains the
/// project basename, which matches the trailing segment of the encoded
/// project dir (e.g. `abtop` ↔ `Users-dimetron-p6s-pi-dev-abtop`).
///
/// 1. Scan `~/.cursor/projects/*/agent-transcripts/*/` for transcripts
///    modified within the history window (2h).
/// 2. Prefer the most recently modified transcript per project.
/// 3. Match each project to a live `extension-host (agent-exec) <basename>`
///    process by its basename. If found, mark as Working/Waiting with the
///    extension-host PID. Otherwise keep the session as a recently-finished
///    (Done-filtered) entry — same UX as Codex/pi-go.
///
/// The encoded-cwd → real-path decode is lossy (`/` and `.` both become
/// `-`, and long paths get a hash suffix), so we try the naive decode first
/// and fall back to showing the encoded id when the path doesn't exist.
pub struct CursorCollector {
    projects_dir: PathBuf,
    /// Incremental parse cache, keyed by transcript uuid.
    transcript_cache: HashMap<String, TranscriptResult>,
}

/// Keep finished Cursor sessions visible for 2 hours.
const HISTORY_WINDOW_MS: u64 = 2 * 60 * 60 * 1000;

impl CursorCollector {
    pub fn new() -> Self {
        let base = dirs::home_dir().unwrap_or_default().join(".cursor");
        Self {
            projects_dir: base.join("projects"),
            transcript_cache: HashMap::new(),
        }
    }

    fn collect_sessions(&mut self, shared: &super::SharedProcessData) -> Vec<AgentSession> {
        if !self.projects_dir.exists() {
            return vec![];
        }

        // 1. Map live extension-host (agent-exec) processes by the basename
        //    that appears in their argv (e.g. "abtop", "research", …).
        let basename_to_pid = Self::find_agent_exec_pids(&shared.process_info);

        // 2. Enumerate candidates (project dir, transcript jsonl, uuid, mtime_ms).
        let mut candidates = Self::enumerate_candidates(&self.projects_dir);

        // Newest first → per project we keep the freshest transcript.
        candidates.sort_by(|a, b| b.mtime_ms.cmp(&a.mtime_ms));

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let mut seen_projects: std::collections::HashSet<String> =
            std::collections::HashSet::new();

        let mut sessions = Vec::new();
        let mut kept_uuids: std::collections::HashSet<String> =
            std::collections::HashSet::new();

        for c in candidates {
            // Keep at most one transcript per project.
            if !seen_projects.insert(c.encoded.clone()) {
                continue;
            }

            // Stale transcripts are skipped unless a live extension-host
            // still claims this project — that means Cursor is open and
            // pointing at this workspace, even if the user hasn't said
            // anything recently.
            let basename = last_segment(&c.encoded);
            let live_pid = basename_to_pid.get(basename).copied();
            let fresh = now_ms.saturating_sub(c.mtime_ms) <= HISTORY_WINDOW_MS;
            if !fresh && live_pid.is_none() {
                continue;
            }

            if let Some(session) =
                self.load_session(live_pid, &c, shared)
            {
                kept_uuids.insert(session.session_id.clone());
                sessions.push(session);
            }
        }

        // Evict cache for transcripts that dropped off.
        self.transcript_cache.retain(|k, _| kept_uuids.contains(k));

        sessions.sort_by_key(|s| std::cmp::Reverse(s.started_at));
        sessions
    }

    fn load_session(
        &mut self,
        pid: Option<u32>,
        cand: &Candidate,
        shared: &super::SharedProcessData,
    ) -> Option<AgentSession> {
        // Incremental parse.
        let cached = self.transcript_cache.remove(&cand.uuid);
        let identity_changed = cached
            .as_ref()
            .map(|c| c.file_identity != file_identity(&cand.transcript))
            .unwrap_or(false);
        let from_offset = if identity_changed {
            0
        } else {
            cached.as_ref().map(|c| c.new_offset).unwrap_or(0)
        };
        let base = if identity_changed { None } else { cached };
        let result = parse_transcript(&cand.transcript, from_offset, base);

        let process_info = &shared.process_info;
        let children_map = &shared.children_map;
        let ports = &shared.ports;

        let proc = pid.and_then(|p| process_info.get(&p));
        let mem_mb = proc.map(|p| p.rss_kb / 1024).unwrap_or(0);
        let display_pid = pid.unwrap_or(0);

        let cwd = decode_project_cwd(&cand.encoded);
        let project_name = cwd.rsplit('/').next().unwrap_or("?").to_string();

        // started_at: first transcript mtime we see. For live projects
        // without any messages yet we fall back to the enumeration mtime.
        let started_at = if result.started_at_ms > 0 {
            result.started_at_ms
        } else {
            cand.mtime_ms
        };

        // Status — we have no per-turn timestamps, so we rely on mtime.
        let pid_alive = proc.is_some();
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let since_activity_ms = now_ms.saturating_sub(cand.mtime_ms);

        let status = if !pid_alive {
            SessionStatus::Done
        } else if since_activity_ms < 30_000 {
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

        // Children of the extension-host. Note: Cursor agents mostly invoke
        // short-lived shell children, so this list is often empty.
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

        let session = AgentSession {
            agent_cli: "cursor",
            pid: display_pid,
            session_id: cand.uuid.clone(),
            cwd,
            project_name,
            started_at,
            status,
            // Cursor doesn't write model/usage into transcripts.
            model: "-".to_string(),
            effort: String::new(),
            context_percent: 0.0,
            total_input_tokens: 0,
            total_output_tokens: 0,
            total_cache_read: 0,
            total_cache_create: 0,
            turn_count: result.turn_count,
            current_tasks,
            mem_mb,
            version: String::new(),
            git_branch: String::new(),
            git_added: 0,
            git_modified: 0,
            token_history: Vec::new(),
            subagents: vec![],
            mem_file_count: 0,
            mem_line_count: 0,
            children,
            initial_prompt: result.initial_prompt.clone(),
            first_assistant_text: result.first_assistant_text.clone(),
        };

        self.transcript_cache.insert(cand.uuid.clone(), result);
        Some(session)
    }

    /// Scan `ps` output for `Cursor Helper (Plugin): extension-host (agent-exec) <basename>`
    /// processes and return `{ basename → pid }`. When a basename appears
    /// more than once we keep the highest PID (newest window).
    fn find_agent_exec_pids(process_info: &HashMap<u32, ProcInfo>) -> HashMap<String, u32> {
        const NEEDLE: &str = "extension-host (agent-exec) ";
        let mut out: HashMap<String, u32> = HashMap::new();
        for (pid, info) in process_info {
            let cmd = &info.command;
            // Only Cursor's helper runs this argv pattern.
            if !cmd.contains("Cursor Helper") || !cmd.contains(NEEDLE) {
                continue;
            }
            let Some(rest) = cmd.split(NEEDLE).nth(1) else { continue; };
            // Trailing fragment looks like:  `abtop [16-63]` or `abtop`
            let basename = rest
                .split_whitespace()
                .next()
                .unwrap_or("")
                .trim_matches(|c: char| c == '[' || c == ']')
                .to_string();
            if basename.is_empty() {
                continue;
            }
            // Keep the highest PID so the most recently spawned window wins.
            out.entry(basename)
                .and_modify(|v| {
                    if *pid > *v {
                        *v = *pid;
                    }
                })
                .or_insert(*pid);
        }
        out
    }

    fn enumerate_candidates(projects_dir: &Path) -> Vec<Candidate> {
        let mut out = Vec::new();
        let Ok(projects) = fs::read_dir(projects_dir) else {
            return out;
        };
        for project in projects.flatten() {
            let project_path = project.path();
            if !project_path.is_dir() {
                continue;
            }
            let encoded = match project_path.file_name().and_then(|s| s.to_str()) {
                Some(n) => n.to_string(),
                None => continue,
            };
            // Skip anonymous/numeric windows — they have no resolvable cwd
            // and their transcripts aren't useful in a per-project view.
            if !is_path_encoded(&encoded) {
                continue;
            }
            let ts_dir = project_path.join("agent-transcripts");
            let Ok(uuid_dirs) = fs::read_dir(&ts_dir) else {
                continue;
            };
            for uuid_entry in uuid_dirs.flatten() {
                let uuid_path = uuid_entry.path();
                if !uuid_path.is_dir() {
                    continue;
                }
                let uuid = match uuid_path.file_name().and_then(|s| s.to_str()) {
                    Some(n) => n.to_string(),
                    None => continue,
                };
                let jsonl = uuid_path.join(format!("{}.jsonl", uuid));
                let Ok(meta) = fs::metadata(&jsonl) else { continue };
                let mtime_ms = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                out.push(Candidate {
                    encoded: encoded.clone(),
                    uuid,
                    transcript: jsonl,
                    mtime_ms,
                });
            }
        }
        out
    }
}

impl super::AgentCollector for CursorCollector {
    fn collect(&mut self, shared: &super::SharedProcessData) -> Vec<AgentSession> {
        self.collect_sessions(shared)
    }
}

// ── helpers ──────────────────────────────────────────────────────────────────

struct Candidate {
    encoded: String,
    uuid: String,
    transcript: PathBuf,
    mtime_ms: u64,
}

/// Last dash-separated segment — the project basename (`abtop`, `research`, …).
fn last_segment(encoded: &str) -> &str {
    encoded.rsplit('-').next().unwrap_or(encoded)
}

/// True iff the project dir looks like a path-encoded cwd (starts with
/// `Users-`, `private-`, `tmp-`, …) rather than an opaque numeric workspace id.
fn is_path_encoded(name: &str) -> bool {
    // Cursor uses path-encoded names that begin with the first segment
    // of the absolute path. On macOS these all start with `Users-` for
    // the per-user home. We also accept a few other legitimate roots so
    // this isn't macOS-specific.
    let first = name.split('-').next().unwrap_or("");
    matches!(first, "Users" | "home" | "tmp" | "var" | "opt" | "private")
}

/// Best-effort decode of the encoded project dir to an absolute path.
/// The encoding is lossy (`/`, `.` → `-`), so we try the naive decode
/// and only use it when the resulting path exists on disk. Otherwise we
/// fall back to a readable form: `/Users/.../encoded-dir` style.
fn decode_project_cwd(encoded: &str) -> String {
    let naive = format!("/{}", encoded.replace('-', "/"));
    if Path::new(&naive).exists() {
        return naive;
    }
    // Try stripping a trailing 7-char hash suffix (Cursor uses these
    // when the raw path would be ambiguous or too long).
    if let Some(stripped) = strip_hash_suffix(encoded) {
        let candidate = format!("/{}", stripped.replace('-', "/"));
        if Path::new(&candidate).exists() {
            return candidate;
        }
    }
    // Fallback: return the naive form with a `~encoded` marker so the
    // user still sees a readable project name.
    naive
}

fn strip_hash_suffix(encoded: &str) -> Option<&str> {
    // e.g. `…plt-0b8ec7e` — 7 lowercase hex chars after the last dash.
    let (head, tail) = encoded.rsplit_once('-')?;
    if tail.len() == 7 && tail.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()) {
        Some(head)
    } else {
        None
    }
}

// ── transcript parsing (incremental) ─────────────────────────────────────────

struct TranscriptResult {
    turn_count: u32,
    current_task: String,
    /// Mtime of the transcript the first time we parsed it — used as the
    /// session start timestamp (Cursor transcripts have no per-message ts).
    started_at_ms: u64,
    new_offset: u64,
    file_identity: (u64, u64),
    initial_prompt: String,
    first_assistant_text: String,
    /// True after we've counted the current user→assistant pair.
    last_was_user: bool,
}

impl TranscriptResult {
    fn empty() -> Self {
        Self {
            turn_count: 0,
            current_task: String::new(),
            started_at_ms: 0,
            new_offset: 0,
            file_identity: (0, 0),
            initial_prompt: String::new(),
            first_assistant_text: String::new(),
            last_was_user: false,
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

fn file_ctime_ms(path: &Path) -> u64 {
    fs::metadata(path)
        .ok()
        .and_then(|m| m.created().ok().or_else(|| m.modified().ok()))
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn parse_transcript(path: &Path, from_offset: u64, base: Option<TranscriptResult>) -> TranscriptResult {
    let mut result = base.unwrap_or_else(TranscriptResult::empty);
    result.file_identity = file_identity(path);
    if result.started_at_ms == 0 {
        result.started_at_ms = file_ctime_ms(path);
    }

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
        let raw_len = line.len() as u64 + 1;
        if line.trim().is_empty() {
            bytes_consumed += raw_len;
            continue;
        }
        let parsed: Option<Value> = serde_json::from_str(&line).ok();
        if parsed.is_none() {
            trailing_partial = true;
            break;
        }
        bytes_consumed += raw_len;
        let val = parsed.unwrap();

        let role = val["role"].as_str().unwrap_or("");
        let parts = val["message"]["content"].as_array();

        // Count one turn per user→assistant transition, so multi-line
        // assistant responses don't inflate the counter.
        match role {
            "user" => {
                result.last_was_user = true;
            }
            "assistant" if result.last_was_user => {
                result.turn_count = result.turn_count.saturating_add(1);
                result.last_was_user = false;
            }
            _ => {}
        }

        let Some(parts) = parts else { continue };

        for part in parts {
            let ptype = part["type"].as_str().unwrap_or("");
            match ptype {
                "text" => {
                    let text = part["text"].as_str().unwrap_or("").trim();
                    if text.is_empty() {
                        continue;
                    }
                    // Strip Cursor's <user_query> tag so the prompt reads cleanly.
                    let cleaned = strip_user_query_tag(text);
                    if role == "user" && result.initial_prompt.is_empty() {
                        result.initial_prompt = cleaned.chars().take(200).collect();
                    } else if role == "assistant" && result.first_assistant_text.is_empty() {
                        result.first_assistant_text = cleaned.chars().take(200).collect();
                    }
                }
                "tool_use" => {
                    let name = part["name"].as_str().unwrap_or("");
                    if name.is_empty() {
                        continue;
                    }
                    let arg_hint = part["input"]
                        .as_object()
                        .and_then(|m| {
                            // Pick the most informative arg we know about.
                            m.get("path")
                                .and_then(|v| v.as_str())
                                .or_else(|| m.get("file_path").and_then(|v| v.as_str()))
                                .or_else(|| m.get("command").and_then(|v| v.as_str()))
                                .or_else(|| m.get("pattern").and_then(|v| v.as_str()))
                                .or_else(|| m.get("query").and_then(|v| v.as_str()))
                                .or_else(|| m.get("url").and_then(|v| v.as_str()))
                                .map(|s| s.to_string())
                        })
                        .unwrap_or_default();
                    result.current_task = if arg_hint.is_empty() {
                        name.to_string()
                    } else {
                        let short = arg_hint.rsplit('/').next().unwrap_or(&arg_hint);
                        // Keep the display compact — the sessions panel
                        // truncates aggressively anyway.
                        let short = short.chars().take(60).collect::<String>();
                        format!("{} {}", name, short)
                    };
                }
                _ => {}
            }
        }
    }

    result.new_offset = if trailing_partial {
        bytes_consumed
    } else {
        bytes_consumed.min(file_len)
    };
    result
}

fn strip_user_query_tag(s: &str) -> String {
    // Cursor wraps freshly-typed prompts in <user_query>…</user_query>.
    // Strip both tags; whitespace trimming handles the newlines.
    let trimmed = s.trim();
    let stripped = trimmed
        .strip_prefix("<user_query>")
        .unwrap_or(trimmed)
        .trim();
    stripped
        .strip_suffix("</user_query>")
        .unwrap_or(stripped)
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_lines(file: &mut tempfile::NamedTempFile, lines: &[&str]) {
        for line in lines {
            writeln!(file, "{}", line).unwrap();
        }
        file.flush().unwrap();
    }

    #[test]
    fn test_last_segment() {
        assert_eq!(last_segment("Users-dimetron-p6s-pi-dev-abtop"), "abtop");
        assert_eq!(last_segment("abtop"), "abtop");
    }

    #[test]
    fn test_is_path_encoded() {
        assert!(is_path_encoded("Users-dimetron-foo"));
        assert!(is_path_encoded("home-me-proj"));
        assert!(is_path_encoded("private-var-folders-x"));
        assert!(!is_path_encoded("1770471792203"));
        assert!(!is_path_encoded("empty-window"));
    }

    #[test]
    fn test_strip_hash_suffix() {
        assert_eq!(
            strip_hash_suffix("Users-dimetron-plt-0b8ec7e"),
            Some("Users-dimetron-plt")
        );
        assert_eq!(strip_hash_suffix("Users-dimetron-abtop"), None);
        assert_eq!(strip_hash_suffix("short"), None);
    }

    #[test]
    fn test_strip_user_query_tag() {
        assert_eq!(
            strip_user_query_tag("<user_query>\nnow add cursor sessions\n</user_query>"),
            "now add cursor sessions"
        );
        assert_eq!(strip_user_query_tag("plain"), "plain");
    }

    #[test]
    fn test_decode_project_cwd_naive_hits_existing_dir() {
        // "/tmp" exists on every reasonable dev box, "/Users" on macOS.
        // We test the unambiguous case where the decode round-trips.
        let home = dirs::home_dir().expect("HOME");
        let encoded = home
            .to_string_lossy()
            .trim_start_matches('/')
            .replace('/', "-");
        let decoded = decode_project_cwd(&encoded);
        assert_eq!(decoded, home.to_string_lossy());
    }

    #[test]
    fn test_parse_basic_turns_and_tool_use() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        write_lines(&mut file, &[
            r#"{"role":"user","message":{"content":[{"type":"text","text":"<user_query>\nFind TODOs\n</user_query>"}]}}"#,
            r#"{"role":"assistant","message":{"content":[{"type":"text","text":"On it."},{"type":"tool_use","name":"Grep","input":{"pattern":"TODO","path":"src/app.rs"}}]}}"#,
            r#"{"role":"assistant","message":{"content":[{"type":"tool_use","name":"Read","input":{"path":"/Users/me/proj/src/lib.rs"}}]}}"#,
            r#"{"role":"user","message":{"content":[{"type":"text","text":"thanks"}]}}"#,
            r#"{"role":"assistant","message":{"content":[{"type":"text","text":"done"}]}}"#,
        ]);
        let result = parse_transcript(file.path(), 0, None);
        assert_eq!(result.initial_prompt, "Find TODOs");
        assert_eq!(result.first_assistant_text, "On it.");
        // two user→assistant transitions.
        assert_eq!(result.turn_count, 2);
        // Most recent tool_use wins.
        assert_eq!(result.current_task, "Read lib.rs");
    }

    #[test]
    fn test_incremental_resume_does_not_double_count() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        write_lines(&mut file, &[
            r#"{"role":"user","message":{"content":[{"type":"text","text":"first"}]}}"#,
            r#"{"role":"assistant","message":{"content":[{"type":"text","text":"ok"}]}}"#,
        ]);
        let first = parse_transcript(file.path(), 0, None);
        assert_eq!(first.turn_count, 1);
        assert_eq!(first.initial_prompt, "first");

        let more = "\n".to_string()
            + r#"{"role":"user","message":{"content":[{"type":"text","text":"second"}]}}"#
            + "\n"
            + r#"{"role":"assistant","message":{"content":[{"type":"tool_use","name":"Shell","input":{"command":"ls -la"}}]}}"#;
        std::fs::OpenOptions::new()
            .append(true)
            .open(file.path())
            .unwrap()
            .write_all(more.as_bytes())
            .unwrap();

        let second = parse_transcript(file.path(), first.new_offset, Some(first));
        assert_eq!(second.turn_count, 2);
        // initial_prompt is sticky — not overwritten by later user messages.
        assert_eq!(second.initial_prompt, "first");
        assert_eq!(second.current_task, "Shell ls -la");
    }

    #[test]
    fn test_partial_trailing_line_preserved() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        // Valid line + partial (no newline, truncated JSON).
        let good = r#"{"role":"user","message":{"content":[{"type":"text","text":"hi"}]}}"#;
        let partial = r#"{"role":"assistant","message":{"content":[{"type":"tex"#;
        file.write_all(good.as_bytes()).unwrap();
        file.write_all(b"\n").unwrap();
        file.write_all(partial.as_bytes()).unwrap();
        file.flush().unwrap();

        let result = parse_transcript(file.path(), 0, None);
        assert_eq!(result.initial_prompt, "hi");
        // new_offset must stop before the partial line so the next tick
        // re-reads it once it's completed.
        let full_len = std::fs::metadata(file.path()).unwrap().len();
        assert!(result.new_offset < full_len, "new_offset={}, file_len={}", result.new_offset, full_len);
    }
}
