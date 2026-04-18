//! Aggregated token usage broken down by coding agent CLI.
//!
//! Buckets:
//! - **Claude** — `agent_cli == "claude"`
//! - **Codex** — `agent_cli == "codex"`
//! - **Pi-Go** — `agent_cli == "pi"`
//! - **Cursor** — `agent_cli == "cursor"`

use crate::app::App;
use crate::model::AgentSession;
use crate::theme::Theme;
use serde_json::Value;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::{btop_block, fmt_tokens, grad_at, make_gradient, meter_bar, styled_label};

const AGENT_ORDER: [AgentKind; 4] = [
    AgentKind::Claude,
    AgentKind::Codex,
    AgentKind::Cursor,
    AgentKind::Pi,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum AgentKind {
    Claude,
    Codex,
    Cursor,
    Pi,
}

impl AgentKind {
    fn label(&self) -> &'static str {
        match self {
            AgentKind::Claude => "Claude",
            AgentKind::Codex => "Codex",
            AgentKind::Cursor => "Cursor",
            AgentKind::Pi => "Pi-Go",
        }
    }

    fn short(&self) -> &'static str {
        match self {
            AgentKind::Claude => "CC",
            AgentKind::Codex => "CDX",
            AgentKind::Cursor => "CUR",
            AgentKind::Pi => "PI",
        }
    }

    fn color(&self, _theme: &Theme) -> Color {
        match self {
            AgentKind::Claude => Color::Rgb(217, 119, 87),
            AgentKind::Codex => Color::Rgb(122, 157, 255),
            AgentKind::Cursor => Color::Rgb(168, 139, 250),
            AgentKind::Pi => Color::Rgb(120, 200, 140),
        }
    }
}

pub(crate) fn kind_for_session(session: &AgentSession) -> Option<AgentKind> {
    match session.agent_cli {
        "claude" => Some(AgentKind::Claude),
        "codex" => Some(AgentKind::Codex),
        "pi" => Some(AgentKind::Pi),
        "cursor" => Some(AgentKind::Cursor),
        _ => None,
    }
}

#[derive(Debug, Default, Clone)]
pub(crate) struct AgentStats {
    pub tokens: u64,
    pub sessions: u32,
    pub working: u32,
}

pub(crate) fn aggregate(sessions: &[AgentSession]) -> Vec<(AgentKind, AgentStats)> {
    let mut out: Vec<(AgentKind, AgentStats)> = AGENT_ORDER
        .iter()
        .map(|a| (*a, AgentStats::default()))
        .collect();

    for s in sessions {
        // Daily view: only include sessions started today.
        if !is_today_millis(s.started_at) {
            continue;
        }
        let Some(a) = kind_for_session(s) else { continue; };
        let idx = AGENT_ORDER.iter().position(|x| *x == a).unwrap_or(0);
        let entry = &mut out[idx].1;
        entry.tokens = entry.tokens.saturating_add(s.total_tokens());
        entry.sessions += 1;
        if matches!(s.status, crate::model::SessionStatus::Working) {
            entry.working += 1;
        }
    }

    // pi-go keeps authoritative day-level token totals in ~/.pi-go/usage.json.
    // When present for today, prefer it over the subset implied by currently
    // tracked session transcripts (active + recently finished).
    if let Some(pi_daily) = read_pi_daily_usage_tokens() {
        if let Some((_, pi_stats)) = out.iter_mut().find(|(k, _)| *k == AgentKind::Pi) {
            pi_stats.tokens = pi_stats.tokens.max(pi_daily);
        }
    }

    out
}

pub(crate) fn draw_agents_panel(f: &mut Frame, app: &App, area: Rect, theme: &Theme) {
    let stats = aggregate(&app.sessions);
    let total_tokens: u64 = stats.iter().map(|(_, s)| s.tokens).sum();

    let block = btop_block("agents", "⁷", theme.proc_box, theme);
    f.render_widget(block, area);
    let inner = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    };

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let cpu_grad = make_gradient(theme.cpu_grad.start, theme.cpu_grad.mid, theme.cpu_grad.end);
    let bar_w = (inner.width as usize).saturating_sub(20).clamp(4, 40);
    let mut lines: Vec<Line> = Vec::new();

    lines.push(Line::from(vec![
        styled_label(" Total:  ", theme.graph_text),
        Span::styled(
            fmt_tokens(total_tokens),
            Style::default().fg(theme.title).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("  {} sess", stats.iter().map(|(_, s)| s.sessions as usize).sum::<usize>()),
            Style::default().fg(theme.graph_text),
        ),
    ]));

    for (agent, s) in &stats {
        let pct = if total_tokens > 0 {
            s.tokens as f64 / total_tokens as f64 * 100.0
        } else {
            0.0
        };
        let agent_grad = agent_gradient(*agent);
        let label = format!(" {:<7}:", shorten_label(agent.label()));
        let mut spans = vec![Span::styled(
            label,
            Style::default().fg(agent.color(theme)),
        )];
        spans.extend(meter_bar(pct, bar_w, &agent_grad, theme.meter_bg));
        let count_span = if s.tokens > 0 {
            format!(" {}×{}", fmt_tokens(s.tokens), s.sessions)
        } else if s.sessions > 0 {
            format!(" — ×{}", s.sessions)
        } else {
            String::new()
        };
        spans.push(Span::styled(
            count_span,
            Style::default().fg(agent.color(theme)),
        ));
        lines.push(Line::from(spans));
    }

    if total_tokens > 0 && inner.height as usize >= lines.len() + 2 {
        let legend_w = inner.width as usize;
        lines.push(Line::from(stacked_bar_spans(
            &stats,
            total_tokens,
            legend_w,
            theme,
        )));

        let mut summary_spans = vec![styled_label(" ", theme.graph_text)];
        let mut first = true;
        for (agent, s) in &stats {
            if s.tokens == 0 {
                continue;
            }
            if !first {
                summary_spans.push(Span::styled(" · ", Style::default().fg(theme.graph_text)));
            }
            first = false;
            let pct = s.tokens as f64 / total_tokens as f64 * 100.0;
            summary_spans.push(Span::styled(
                format!("{} {:.0}%", agent.short(), pct),
                Style::default().fg(agent.color(theme)),
            ));
        }
        if first {
            summary_spans.push(Span::styled(
                "no token data",
                Style::default().fg(theme.inactive_fg),
            ));
        }
        lines.push(Line::from(summary_spans));
    } else if total_tokens == 0 {
        while lines.len() + 1 < inner.height as usize {
            lines.push(Line::from(""));
        }
        lines.push(Line::from(Span::styled(
            " no token telemetry yet",
            Style::default().fg(theme.inactive_fg),
        )));
    }

    let working_total: u32 = stats.iter().map(|(_, s)| s.working).sum();
    if working_total > 0 && lines.len() < inner.height as usize {
        let c = grad_at(&cpu_grad, 80.0);
        lines.push(Line::from(vec![
            styled_label(" ", theme.graph_text),
            Span::styled(format!("{} working", working_total), Style::default().fg(c)),
        ]));
    }

    f.render_widget(Paragraph::new(lines), inner);
}

fn stacked_bar_spans(
    stats: &[(AgentKind, AgentStats)],
    total_tokens: u64,
    width: usize,
    theme: &Theme,
) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    if width == 0 || total_tokens == 0 {
        return spans;
    }

    let mut allocs: Vec<(AgentKind, usize, f64)> = stats
        .iter()
        .filter(|(_, s)| s.tokens > 0)
        .map(|(a, s)| {
            let exact = (s.tokens as f64 / total_tokens as f64) * width as f64;
            let floor = exact.floor() as usize;
            (*a, floor, exact - floor as f64)
        })
        .collect();

    let used: usize = allocs.iter().map(|(_, n, _)| *n).sum();
    let mut remaining = width.saturating_sub(used);

    if remaining > 0 && !allocs.is_empty() {
        let mut order: Vec<usize> = (0..allocs.len()).collect();
        order.sort_by(|a, b| {
            allocs[*b]
                .2
                .partial_cmp(&allocs[*a].2)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        for idx in order {
            if remaining == 0 {
                break;
            }
            allocs[idx].1 += 1;
            remaining -= 1;
        }
    }

    for (agent, cells, _) in allocs {
        for _ in 0..cells {
            spans.push(Span::styled(
                "■",
                Style::default().fg(agent.color(theme)),
            ));
        }
    }
    spans
}

fn agent_gradient(agent: AgentKind) -> [Color; 101] {
    let base = match agent {
        AgentKind::Claude => (217, 119, 87),
        AgentKind::Codex => (122, 157, 255),
        AgentKind::Cursor => (168, 139, 250),
        AgentKind::Pi => (120, 200, 140),
    };
    let dark = (
        (base.0 as f32 * 0.55) as u8,
        (base.1 as f32 * 0.55) as u8,
        (base.2 as f32 * 0.55) as u8,
    );
    let mid = (
        (base.0 as f32 * 0.80) as u8,
        (base.1 as f32 * 0.80) as u8,
        (base.2 as f32 * 0.80) as u8,
    );
    make_gradient(dark, mid, base)
}

fn shorten_label(s: &str) -> String {
    if s.len() <= 7 {
        s.to_string()
    } else {
        s.chars().take(7).collect()
    }
}

fn read_pi_daily_usage_tokens() -> Option<u64> {
    let home = dirs::home_dir()?;
    let path = home.join(".pi-go").join("usage.json");
    let content = std::fs::read_to_string(path).ok()?;
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    parse_pi_usage_tokens_for_day(&content, &today)
}

fn parse_pi_usage_tokens_for_day(content: &str, day: &str) -> Option<u64> {
    let v: Value = serde_json::from_str(content).ok()?;
    if v.get("date").and_then(|d| d.as_str()) != Some(day) {
        return None;
    }
    let input = v.get("input_tokens").and_then(|n| n.as_u64()).unwrap_or(0);
    let output = v.get("output_tokens").and_then(|n| n.as_u64()).unwrap_or(0);
    Some(input.saturating_add(output))
}

fn is_today_millis(ts_ms: u64) -> bool {
    let dt = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ts_ms as i64);
    let Some(dt) = dt else { return false; };
    dt.with_timezone(&chrono::Local).date_naive() == chrono::Local::now().date_naive()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::SessionStatus;

    fn mk(agent: &'static str, model: &str, tokens_in: u64, tokens_out: u64) -> AgentSession {
        AgentSession {
            agent_cli: agent,
            pid: 1,
            session_id: "s".into(),
            cwd: "/tmp".into(),
            project_name: "p".into(),
            started_at: 0,
            status: SessionStatus::Waiting,
            model: model.to_string(),
            effort: String::new(),
            context_percent: 0.0,
            total_input_tokens: tokens_in,
            total_output_tokens: tokens_out,
            total_cache_read: 0,
            total_cache_create: 0,
            turn_count: 0,
            current_tasks: vec![],
            mem_mb: 0,
            version: String::new(),
            git_branch: String::new(),
            git_added: 0,
            git_modified: 0,
            token_history: vec![],
            subagents: vec![],
            mem_file_count: 0,
            mem_line_count: 0,
            children: vec![],
            initial_prompt: String::new(),
            first_assistant_text: String::new(),
        }
    }

    #[test]
    fn test_kind_for_session() {
        assert_eq!(kind_for_session(&mk("claude", "", 1, 1)), Some(AgentKind::Claude));
        assert_eq!(kind_for_session(&mk("codex", "", 1, 1)), Some(AgentKind::Codex));
        assert_eq!(kind_for_session(&mk("pi", "gemini-2.5-pro", 1, 1)), Some(AgentKind::Pi));
        assert_eq!(kind_for_session(&mk("pi", "qwen3.5:cloud", 1, 1)), Some(AgentKind::Pi));
        assert_eq!(kind_for_session(&mk("cursor", "", 1, 1)), Some(AgentKind::Cursor));
    }

    #[test]
    fn test_aggregate_totals() {
        let now_ms = chrono::Local::now().timestamp_millis() as u64;
        let mut sessions = vec![
            mk("claude", "", 100, 50),
            mk("codex", "", 200, 100),
            mk("pi", "gemini-2.5-pro", 300, 150),
            mk("pi", "qwen3.5:cloud", 50, 50),
            mk("cursor", "", 0, 0),
        ];
        for s in &mut sessions {
            s.started_at = now_ms;
        }
        let stats = aggregate(&sessions);
        let totals: std::collections::HashMap<AgentKind, u64> =
            stats.iter().map(|(a, s)| (*a, s.tokens)).collect();
        assert_eq!(totals[&AgentKind::Claude], 150);
        assert_eq!(totals[&AgentKind::Codex], 300);
        assert!(totals[&AgentKind::Pi] >= 500);
        assert_eq!(totals[&AgentKind::Cursor], 0);
    }

    #[test]
    fn test_preserves_order() {
        let stats = aggregate(&[]);
        assert_eq!(stats.len(), 4);
        assert_eq!(stats[0].0, AgentKind::Claude);
        assert_eq!(stats[1].0, AgentKind::Codex);
        assert_eq!(stats[2].0, AgentKind::Cursor);
        assert_eq!(stats[3].0, AgentKind::Pi);
    }

    #[test]
    fn test_stacked_bar_allocates_exact_width() {
        let now_ms = chrono::Local::now().timestamp_millis() as u64;
        let mut sessions = vec![
            mk("claude", "", 30, 0),
            mk("codex", "", 20, 0),
            mk("pi", "gemini-2.5-pro", 25, 0),
            mk("pi", "qwen3.5:cloud", 25, 0),
            mk("cursor", "", 0, 0),
        ];
        for s in &mut sessions {
            s.started_at = now_ms;
        }
        let stats = aggregate(&sessions);
        let total: u64 = stats.iter().map(|(_, s)| s.tokens).sum();
        let theme = Theme::btop();
        for width in [0, 1, 7, 10, 23, 40, 100] {
            let spans = stacked_bar_spans(&stats, total, width, &theme);
            assert_eq!(spans.len(), width, "width={}", width);
        }
    }

    #[test]
    fn test_parse_pi_usage_tokens_for_day() {
        let content = r#"{
  "date": "2026-04-18",
  "input_tokens": 25540599,
  "output_tokens": 163001,
  "requests": 591
}"#;
        assert_eq!(
            parse_pi_usage_tokens_for_day(content, "2026-04-18"),
            Some(25_703_600)
        );
        assert_eq!(parse_pi_usage_tokens_for_day(content, "2026-04-17"), None);
    }
}
