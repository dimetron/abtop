use crate::model::{AgentSession, ChildProcess, SessionFile, SessionStatus};
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct ClaudeCollector {
    sessions_dir: PathBuf,
    projects_dir: PathBuf,
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

    pub fn collect(&mut self) -> Vec<AgentSession> {
        let session_files = match fs::read_dir(&self.sessions_dir) {
            Ok(entries) => entries,
            Err(_) => return vec![],
        };

        let process_info = Self::get_process_info();
        let children_map = Self::get_children_map(&process_info);
        let ports = Self::get_listening_ports();

        let mut sessions = Vec::new();
        for entry in session_files.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }

            if let Some(session) = self.load_session(&path, &process_info, &children_map, &ports) {
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

        let pid_alive = is_claude_process(sf.pid);

        let project_name = sf
            .cwd
            .rsplit('/')
            .next()
            .unwrap_or("?")
            .to_string();

        let proc = process_info.get(&sf.pid);
        let mem_mb = proc.map(|p| p.rss_kb / 1024).unwrap_or(0);

        let transcript_path = self.find_transcript(&sf.cwd, &sf.session_id);

        let mut model = String::from("?");
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

        if let Some(ref tp) = transcript_path {
            let offset = self.offsets.get(&sf.session_id).copied().unwrap_or(0);
            let result = parse_transcript(tp, offset);

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

            self.offsets.insert(sf.session_id.clone(), result.new_offset);
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
                SessionStatus::Waiting
            }
        };

        let context_window = context_window_for_model(&model);
        let context_percent = if context_window > 0 {
            (last_context_tokens as f64 / context_window as f64) * 100.0
        } else {
            0.0
        };

        if !pid_alive && current_task.is_empty() {
            current_task = "finished".to_string();
        } else if matches!(status, SessionStatus::Waiting) && current_task.is_empty() {
            current_task = "waiting for input".to_string();
        }

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

        Some(AgentSession {
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
            current_task,
            mem_mb,
            version,
            git_branch,
            children,
            transcript_offset: 0,
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

    fn get_process_info() -> HashMap<u32, ProcInfo> {
        let mut map = HashMap::new();
        let output = Command::new("ps")
            .args(["-eo", "pid,ppid,rss,command"])
            .output()
            .ok();

        if let Some(output) = output {
            let stdout = String::from_utf8_lossy(&output.stdout);
            for line in stdout.lines().skip(1) {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 4 {
                    if let (Ok(pid), Ok(ppid), Ok(rss)) = (
                        parts[0].parse::<u32>(),
                        parts[1].parse::<u32>(),
                        parts[2].parse::<u64>(),
                    ) {
                        let command = parts[3..].join(" ");
                        map.insert(pid, ProcInfo {
                            pid,
                            ppid,
                            rss_kb: rss,
                            command,
                        });
                    }
                }
            }
        }
        map
    }

    fn get_children_map(procs: &HashMap<u32, ProcInfo>) -> HashMap<u32, Vec<u32>> {
        let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
        for proc in procs.values() {
            children.entry(proc.ppid).or_default().push(proc.pid);
        }
        children
    }

    fn get_listening_ports() -> HashMap<u32, Vec<u16>> {
        let mut map: HashMap<u32, Vec<u16>> = HashMap::new();
        let output = Command::new("lsof")
            .args(["-i", "-P", "-n", "-sTCP:LISTEN"])
            .output()
            .ok();

        if let Some(output) = output {
            let stdout = String::from_utf8_lossy(&output.stdout);
            for line in stdout.lines().skip(1) {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 9 {
                    if let Ok(pid) = parts[1].parse::<u32>() {
                        // Parse port from "host:port" in NAME column
                        if let Some(addr) = parts.get(8) {
                            if let Some(port_str) = addr.rsplit(':').next() {
                                if let Ok(port) = port_str.parse::<u16>() {
                                    map.entry(pid).or_default().push(port);
                                }
                            }
                        }
                    }
                }
            }
        }
        map
    }
}

#[derive(Debug)]
struct ProcInfo {
    pid: u32,
    ppid: u32,
    rss_kb: u64,
    command: String,
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
}

fn parse_transcript(path: &Path, from_offset: u64) -> TranscriptResult {
    let mut result = TranscriptResult {
        model: "?".to_string(),
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
                                }
                                // Extract current task from last tool_use
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
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max - 1])
    }
}

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
