use crate::model::RateLimitInfo;
use serde::Deserialize;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// File written by the StatusLine hook: ~/.claude/abtop-rate-limits.json
const CLAUDE_RATE_FILE: &str = "abtop-rate-limits.json";

#[derive(Debug, Deserialize)]
struct RateLimitFile {
    #[serde(default)]
    source: String,
    #[serde(default)]
    five_hour: Option<WindowInfo>,
    #[serde(default)]
    seven_day: Option<WindowInfo>,
    #[serde(default)]
    updated_at: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct WindowInfo {
    #[serde(default)]
    used_percentage: f64,
    #[serde(default)]
    resets_at: u64,
}

/// Read rate limit info from all known sources.
pub fn read_rate_limits() -> Vec<RateLimitInfo> {
    let mut results = Vec::new();

    // Claude Code: read from StatusLine hook output file
    if let Some(claude_dir) = dirs::home_dir().map(|h| h.join(".claude")) {
        let path = claude_dir.join(CLAUDE_RATE_FILE);
        if let Some(info) = read_rate_file(&path, "claude") {
            results.push(info);
        }
    }

    // Codex: planned for v0.2 (reads from rollout JSONL, different schema)

    results
}

fn read_rate_file(path: &PathBuf, default_source: &str) -> Option<RateLimitInfo> {
    let content = std::fs::read_to_string(path).ok()?;
    let file: RateLimitFile = serde_json::from_str(&content).ok()?;

    // Ignore stale data (older than 10 minutes)
    if let Some(updated) = file.updated_at {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if now.saturating_sub(updated) > 600 {
            return None;
        }
    }

    // Reject if both windows are absent
    if file.five_hour.is_none() && file.seven_day.is_none() {
        return None;
    }

    let source = if file.source.is_empty() {
        default_source.to_string()
    } else {
        file.source
    };

    Some(RateLimitInfo {
        source,
        five_hour_pct: file.five_hour.as_ref().map(|w| w.used_percentage),
        five_hour_resets_at: file.five_hour.as_ref().map(|w| w.resets_at),
        seven_day_pct: file.seven_day.as_ref().map(|w| w.used_percentage),
        seven_day_resets_at: file.seven_day.as_ref().map(|w| w.resets_at),
        updated_at: file.updated_at,
    })
}
