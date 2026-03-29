use super::process::{self, ProcInfo};
use crate::model::{AgentSession, ChildProcess, SessionFile, SessionStatus, SubAgent};
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct ClaudeCollector {
    sessions_dir: PathBuf,
    projects_dir: PathBuf,
    #[allow(dead_code)]
    offsets: HashMap<String, u64>,
}

impl ClaudeCollector {
    pub fn new() -> Self {
        let home = dirs::home_dir().unwrap_or_default();
        Self {
            sessions_dir: home.join(".claude").join("sessions"),
            projects_dir: home.join(".claude").join("projects"),
            offsets: HashMap::new(),
        }
    }

    pub fn collect(&mut self, shared: &super::SharedProcessData) -> Vec<AgentSession> {
        let session_files = match fs::read_dir(&self.sessions_dir) {
            Ok(entries) => entries,
            Err(_) => return vec![],
        };

        let mut sessions = Vec::new();
        for entry in session_files.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }

            if let Some(session) = self.load_session(&path, &shared.process_info, &shared.children_map, &shared.ports) {
                sessions.push(session);
            }
        }

        sessions.sort_by_key(|s| std::cmp::Reverse(s.started_at));
        sessions
    }

    fn load_session(
        &mut self,
        path: &Path,
        process_info: &HashMap<u32, ProcInfo>,
        children_map: &HashMap<u32, Vec<u32>>,
        ports: &HashMap<u32, Vec<u16>>,
    ) -> Option<AgentSession> {
        let content = fs::read_to_string(path).ok()?;
        let sf: SessionFile = serde_json::from_str(&content).ok()?;

        let pid_alive = process_info.get(&sf.pid)
            .map(|p| p.command.contains("/claude") && p.command.contains("--session-id"))
            .unwrap_or(false);

        let project_name = sf
            .cwd
            .rsplit('/')
            .next()
            .unwrap_or("?")
            .to_string();

        let proc = process_info.get(&sf.pid);
        let mem_mb = proc.map(|p| p.rss_kb / 1024).unwrap_or(0);

        let transcript_path = self.find_transcript(&sf.cwd, &sf.session_id);

        let mut model = String::from("-");
        let mut total_input = 0u64;
        let mut total_output = 0u64;
        let mut total_cache_read = 0u64;
        let mut total_cache_create = 0u64;
        let mut last_context_tokens = 0u64;
        let mut turn_count = 0u32;
        let mut current_task = String::new();
        let mut version = String::new();
        let mut git_branch = String::new();
        let mut last_activity = std::time::UNIX_EPOCH;
        let mut token_history = Vec::new();
        let mut initial_prompt = String::new();

        if let Some(ref tp) = transcript_path {
            // Always parse from beginning to get correct cumulative totals.
            // Incremental parsing requires carrying forward previous totals,
            // which is v0.2 optimization scope.
            let result = parse_transcript(tp, 0);

            model = result.model;
            total_input = result.total_input;
            total_output = result.total_output;
            total_cache_read = result.total_cache_read;
            total_cache_create = result.total_cache_create;
            last_context_tokens = result.last_context_tokens;
            turn_count = result.turn_count;
            current_task = result.current_task;
            version = result.version;
            git_branch = result.git_branch;
            last_activity = result.last_activity;
            token_history = result.token_history;
            initial_prompt = result.initial_prompt;
        }

        let status = if !pid_alive {
            SessionStatus::Done
        } else {
            let since_activity = std::time::SystemTime::now()
                .duration_since(last_activity)
                .unwrap_or_default();
            if since_activity.as_secs() < 30 {
                SessionStatus::Working
            } else {
                // Transcript is stale (>30s). Check CPU-based signals:
                // 1. Claude process using CPU > 1% → likely thinking/streaming
                let claude_cpu_active = proc.map_or(false, |p| p.cpu_pct > 1.0);
                // 2. Any descendant using significant CPU (>5%) → likely running a tool
                //    (higher threshold avoids false positives from idle watchers/servers)
                let has_active_descendant = process::has_active_descendant(
                    sf.pid, children_map, process_info, 5.0,
                );
                if claude_cpu_active || has_active_descendant {
                    SessionStatus::Working
                } else {
                    SessionStatus::Waiting
                }
            }
        };

        let context_window = context_window_for_model(&model);
        let context_percent = if context_window > 0 {
            (last_context_tokens as f64 / context_window as f64) * 100.0
        } else {
            0.0
        };

        let current_tasks = if !current_task.is_empty() {
            vec![current_task]
        } else if !pid_alive {
            vec!["finished".to_string()]
        } else if matches!(status, SessionStatus::Waiting) {
            vec!["waiting for input".to_string()]
        } else {
            vec!["thinking...".to_string()]
        };

        let mut children = Vec::new();
        if let Some(child_pids) = children_map.get(&sf.pid) {
            for &cpid in child_pids {
                if let Some(cproc) = process_info.get(&cpid) {
                    let port = ports.get(&cpid).and_then(|v| v.first().copied());
                    children.push(ChildProcess {
                        pid: cpid,
                        command: cproc.command.clone(),
                        mem_kb: cproc.rss_kb,
                        port,
                    });
                }
            }
        }

        // Git stats: added and modified file counts
        let (git_added, git_modified) = process::collect_git_stats(&sf.cwd);

        // Subagent discovery
        let encoded_path = sf.cwd.replace('/', "-");
        let subagents_dir = self.projects_dir.join(&encoded_path).join(&sf.session_id).join("subagents");
        let subagents = Self::collect_subagents(&subagents_dir);

        // Memory status
        let memory_dir = self.projects_dir.join(&encoded_path).join("memory");
        let (mem_file_count, mem_line_count) = Self::collect_memory_status(&memory_dir);

        Some(AgentSession {
            agent_cli: "claude",
            pid: sf.pid,
            session_id: sf.session_id,
            cwd: sf.cwd,
            project_name,
            started_at: sf.started_at,
            status,
            model,
            context_percent,
            total_input_tokens: total_input,
            total_output_tokens: total_output,
            total_cache_read,
            total_cache_create,
            turn_count,
            current_tasks,
            mem_mb,
            version,
            git_branch,
            git_added,
            git_modified,
            token_history,
            subagents,
            mem_file_count,
            mem_line_count,
            children,
            transcript_offset: 0,
            initial_prompt,
        })
    }

    fn find_transcript(&self, cwd: &str, session_id: &str) -> Option<PathBuf> {
        let encoded = cwd.replace('/', "-");
        let dir = self.projects_dir.join(&encoded);
        let path = dir.join(format!("{}.jsonl", session_id));
        if path.exists() {
            Some(path)
        } else {
            None
        }
    }


    fn collect_subagents(subagents_dir: &Path) -> Vec<SubAgent> {
        let mut subagents = Vec::new();

        let entries = match fs::read_dir(subagents_dir) {
            Ok(e) => e,
            Err(_) => return subagents,
        };

        // Collect meta files and their corresponding jsonl files
        let mut meta_files: Vec<PathBuf> = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if name.ends_with(".meta.json") {
                    meta_files.push(path);
                }
            }
        }

        for meta_path in meta_files {
            let meta_name = match meta_path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n.to_string(),
                None => continue,
            };

            // Parse meta JSON
            let meta_content = match fs::read_to_string(&meta_path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let meta_val: Value = match serde_json::from_str(&meta_content) {
                Ok(v) => v,
                Err(_) => continue,
            };

            let agent_type = meta_val.get("agentType")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            let description = meta_val.get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("agent")
                .to_string();

            // Derive jsonl path: agent-{hash}.meta.json -> agent-{hash}.jsonl
            let jsonl_name = meta_name.replace(".meta.json", ".jsonl");
            let jsonl_path = meta_path.with_file_name(&jsonl_name);

            let mut tokens = 0u64;
            let mut last_activity = std::time::UNIX_EPOCH;

            if jsonl_path.exists() {
                // Get file mtime for status
                if let Ok(metadata) = fs::metadata(&jsonl_path) {
                    if let Ok(mtime) = metadata.modified() {
                        last_activity = mtime;
                    }
                }

                // Parse jsonl for token totals
                let transcript = parse_transcript(&jsonl_path, 0);
                tokens = transcript.total_input + transcript.total_output
                    + transcript.total_cache_read + transcript.total_cache_create;
            }

            let status = {
                let since = std::time::SystemTime::now()
                    .duration_since(last_activity)
                    .unwrap_or_default();
                if since.as_secs() < 30 {
                    "working".to_string()
                } else {
                    "done".to_string()
                }
            };

            // Use description as name, shorten if needed
            let name = truncate(&description, 30);

            subagents.push(SubAgent {
                name,
                agent_type,
                status,
                tokens,
            });
        }

        subagents
    }

    fn collect_memory_status(memory_dir: &Path) -> (u32, u32) {
        let mut file_count = 0u32;
        let mut line_count = 0u32;

        if let Ok(entries) = fs::read_dir(memory_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    file_count += 1;
                }
            }
        }

        let memory_md = memory_dir.join("MEMORY.md");
        if let Ok(content) = fs::read_to_string(&memory_md) {
            line_count = content.lines().count() as u32;
        }

        (file_count, line_count)
    }
}


struct TranscriptResult {
    model: String,
    total_input: u64,
    total_output: u64,
    total_cache_read: u64,
    total_cache_create: u64,
    /// Last assistant turn's input context size (for context % calculation)
    last_context_tokens: u64,
    turn_count: u32,
    current_task: String,
    version: String,
    git_branch: String,
    last_activity: std::time::SystemTime,
    new_offset: u64,
    token_history: Vec<u64>,
    initial_prompt: String,
}

fn parse_transcript(path: &Path, from_offset: u64) -> TranscriptResult {
    let mut result = TranscriptResult {
        model: "-".to_string(),
        total_input: 0,
        total_output: 0,
        total_cache_read: 0,
        total_cache_create: 0,
        last_context_tokens: 0,
        turn_count: 0,
        current_task: String::new(),
        version: String::new(),
        git_branch: String::new(),
        last_activity: std::time::UNIX_EPOCH,
        new_offset: from_offset,
        token_history: Vec::new(),
        initial_prompt: String::new(),
    };

    let file = match fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return result,
    };

    let file_len = file.metadata().map(|m| m.len()).unwrap_or(0);
    if file_len <= from_offset {
        result.new_offset = file_len;
        return result;
    }

    let mut reader = BufReader::new(file);
    if from_offset > 0 {
        let _ = reader.seek(SeekFrom::Start(from_offset));
    }

    let mtime = fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .unwrap_or(std::time::UNIX_EPOCH);
    result.last_activity = mtime;

    let mut bytes_read = from_offset;
    let mut line_buf = String::new();
    loop {
        line_buf.clear();
        match reader.read_line(&mut line_buf) {
            Ok(0) => break,
            Ok(n) => {
                bytes_read += n as u64;
                let line = line_buf.trim();
                if line.is_empty() {
                    continue;
                }
                if let Ok(val) = serde_json::from_str::<Value>(line) {
                    match val.get("type").and_then(|t| t.as_str()) {
                        Some("assistant") => {
                            result.turn_count += 1;
                            // Clear previous task on each new turn so stale tasks
                            // don't persist when latest turn has no tool_use
                            result.current_task = String::new();
                            if let Some(msg) = val.get("message") {
                                if let Some(m) = msg.get("model").and_then(|m| m.as_str()) {
                                    result.model = m.to_string();
                                }
                                if let Some(usage) = msg.get("usage") {
                                    let inp = usage.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                                    let out = usage.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                                    let cr = usage.get("cache_read_input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                                    let cc = usage.get("cache_creation_input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                                    result.total_input += inp;
                                    result.total_output += out;
                                    result.total_cache_read += cr;
                                    result.total_cache_create += cc;
                                    // Context = last turn's total input (this is what the model "sees")
                                    result.last_context_tokens = inp + cr + cc;
                                    // Track per-turn total tokens for sparkline
                                    result.token_history.push(inp + out + cr + cc);
                                }
                                // Extract last tool_use from latest turn (= most recently running)
                                if let Some(content) = msg.get("content").and_then(|c| c.as_array()) {
                                    for item in content.iter().rev() {
                                        if item.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                                            let tool = item.get("name").and_then(|n| n.as_str()).unwrap_or("?");
                                            let arg = extract_tool_arg(item);
                                            result.current_task = format!("{} {}", tool, arg);
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                        Some("user") => {
                            if let Some(v) = val.get("version").and_then(|v| v.as_str()) {
                                result.version = v.to_string();
                            }
                            if let Some(b) = val.get("gitBranch").and_then(|b| b.as_str()) {
                                result.git_branch = b.to_string();
                            }
                            // Extract first user prompt as session title
                            if result.initial_prompt.is_empty() {
                                if let Some(msg) = val.get("message") {
                                    result.initial_prompt = extract_prompt_text(msg);
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            Err(_) => break,
        }
    }

    result.new_offset = bytes_read;
    result
}

/// Extract a short summary from the first user message content.
/// Handles both string content and array-of-blocks content.
fn extract_prompt_text(message: &Value) -> String {
    let raw = match message.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(arr)) => {
            // Find first text block
            arr.iter()
                .filter_map(|block| {
                    if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                        block.get("text").and_then(|t| t.as_str()).map(|s| s.to_string())
                    } else {
                        None
                    }
                })
                .next()
                .unwrap_or_default()
        }
        _ => return String::new(),
    };

    // Clean up: remove image markers, code blocks, markdown headers
    let cleaned: String = raw
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with("```"))
        .collect::<Vec<_>>()
        .join(" ");

    // Remove [Image #N] markers
    let mut result = cleaned;
    while let Some(start) = result.find("[Image") {
        if let Some(end) = result[start..].find(']') {
            result = format!("{}{}", &result[..start], result[start + end + 1..].trim_start());
        } else {
            break;
        }
    }

    let clean = result.trim().to_string();
    if clean.is_empty() {
        return String::new();
    }
    truncate(&clean, 50)
}

fn extract_tool_arg(tool_use: &Value) -> String {
    if let Some(input) = tool_use.get("input") {
        // Edit/Read: file_path
        if let Some(fp) = input.get("file_path").and_then(|f| f.as_str()) {
            return shorten_path(fp);
        }
        // Bash: command (first 40 chars)
        if let Some(cmd) = input.get("command").and_then(|c| c.as_str()) {
            let short = cmd.lines().next().unwrap_or(cmd);
            return truncate(short, 40);
        }
        // Grep/Glob: pattern
        if let Some(pat) = input.get("pattern").and_then(|p| p.as_str()) {
            return truncate(pat, 40);
        }
    }
    String::new()
}

fn shorten_path(path: &str) -> String {
    let parts: Vec<&str> = path.rsplit('/').collect();
    if parts.len() <= 2 {
        path.to_string()
    } else {
        format!("{}/{}", parts[1], parts[0])
    }
}

fn truncate(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max - 1).collect();
        format!("{}…", truncated)
    }
}

#[allow(dead_code)]
fn is_claude_process(pid: u32) -> bool {
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "command="])
        .output()
        .ok();

    match output {
        Some(out) => {
            let cmd = String::from_utf8_lossy(&out.stdout);
            cmd.contains("/claude") && cmd.contains("--session-id")
        }
        None => false,
    }
}

fn context_window_for_model(model: &str) -> u64 {
    if model.contains("[1m]") {
        1_000_000
    } else if model.contains("opus") || model.contains("sonnet") || model.contains("haiku") {
        200_000
    } else {
        200_000
    }
}
