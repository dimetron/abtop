//! Aggregated token usage broken down by inferred LLM provider.
//!
//! Rolls every session's `total_tokens()` up to one of four buckets:
//! - **Anthropic** — Claude Code sessions, and pi-go sessions whose model
//!   contains `claude` / `opus` / `sonnet` / `haiku`.
//! - **OpenAI** — Codex CLI sessions, and pi-go sessions whose model
//!   contains `gpt` / `o1` / `o3`.
//! - **Ollama** — pi-go sessions whose model uses `ollama/` prefix or
//!   local/cloud suffixes such as `:local` / `:cloud`.
//! - **Other** — everything else (unknown model on pi-go, Cursor server-side
//!   sessions, any future agents).
//!
//! Cursor sessions report `tokens = 0` (no local telemetry) so they mostly
//! sit in "Other" with a zero bar — still useful because it shows *how many*
//! sessions there are, even without counts.

use crate::app::App;
use crate::model::AgentSession;
use crate::theme::Theme;
use std::collections::HashMap;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::{btop_block, fmt_tokens, grad_at, make_gradient, meter_bar, styled_label};

/// Stable ordering for display: Anthropic first (most common), then OpenAI,
/// Ollama, Other. Also stable for tests.
const PROVIDER_ORDER: [Provider; 4] = [
    Provider::Anthropic,
    Provider::OpenAI,
    Provider::Ollama,
    Provider::Other,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Provider {
    Anthropic,
    OpenAI,
    Ollama,
    Other,
}

impl Provider {
    fn label(&self) -> &'static str {
        match self {
            Provider::Anthropic => "Anthropic",
            Provider::OpenAI => "OpenAI",
            Provider::Ollama => "Ollama",
            Provider::Other => "Other",
        }
    }

    /// Short label used inside the compact stacked legend.
    fn short(&self) -> &'static str {
        match self {
            Provider::Anthropic => "Ant",
            Provider::OpenAI => "OAI",
            Provider::Ollama => "Olm",
            Provider::Other => "Oth",
        }
    }

    fn color(&self, theme: &Theme) -> Color {
        match self {
            // Claude orange (matches the *CC agent label).
            Provider::Anthropic => Color::Rgb(217, 119, 87),
            // Codex periwinkle (matches the >CD agent label).
            Provider::OpenAI => Color::Rgb(122, 157, 255),
            // Distinct violet for local/compatible Ollama usage.
            Provider::Ollama => Color::Rgb(168, 139, 250),
            Provider::Other => theme.inactive_fg,
        }
    }
}

/// Infer a session's provider from `agent_cli` + `model`.
/// Claude and Codex are unambiguous; pi-go looks at the model string;
/// Cursor falls through to Other (tokens are always 0 anyway).
pub(crate) fn provider_for_session(session: &AgentSession) -> Provider {
    match session.agent_cli {
        "claude" => Provider::Anthropic,
        "codex" => Provider::OpenAI,
        "pi" => provider_from_model(&session.model),
        _ => Provider::Other,
    }
}

fn provider_from_model(model: &str) -> Provider {
    let m = model.to_ascii_lowercase();
    if m.is_empty()
        || m == "-"
        || m.starts_with("ollama/")
        || m.ends_with(":cloud")
        || m.ends_with(":local")
        || m.starts_with("qwen")
        || m.starts_with("minimax")
        || m.starts_with("deepseek")
        || m.starts_with("llama")
        || m.starts_with("phi")
        || m.starts_with("codellama")
        || m.starts_with("gemma")
    {
        Provider::Ollama
    } else if m.contains("claude") || m.contains("opus") || m.contains("sonnet") || m.contains("haiku") {
        Provider::Anthropic
    } else if m.contains("gpt")
        || m.contains("o1")
        || m.contains("o3")
        || m.contains("codex")
    {
        Provider::OpenAI
    } else {
        Provider::Other
    }
}

/// Per-provider aggregate. One of these per bucket.
#[derive(Debug, Default, Clone)]
pub(crate) struct ProviderStats {
    pub tokens: u64,
    pub sessions: u32,
    pub working: u32,
}

#[derive(Debug, Default, Clone)]
struct ModelStats {
    tokens: u64,
    sessions: u32,
}

pub(crate) fn aggregate(sessions: &[AgentSession]) -> Vec<(Provider, ProviderStats)> {
    let mut out: Vec<(Provider, ProviderStats)> = PROVIDER_ORDER
        .iter()
        .map(|p| (*p, ProviderStats::default()))
        .collect();

    for s in sessions {
        let p = provider_for_session(s);
        let idx = PROVIDER_ORDER.iter().position(|x| *x == p).unwrap_or(3);
        let entry = &mut out[idx].1;
        entry.tokens = entry.tokens.saturating_add(s.total_tokens());
        entry.sessions += 1;
        if matches!(s.status, crate::model::SessionStatus::Working) {
            entry.working += 1;
        }
    }

    out
}

fn aggregate_models_by_provider(
    sessions: &[AgentSession],
) -> HashMap<Provider, Vec<(String, ModelStats)>> {
    let mut grouped: HashMap<Provider, HashMap<String, ModelStats>> = HashMap::new();
    for s in sessions {
        let provider = provider_for_session(s);
        let model = normalized_model_label(s);
        let by_model = grouped.entry(provider).or_default();
        let entry = by_model.entry(model).or_default();
        entry.tokens = entry.tokens.saturating_add(s.total_tokens());
        entry.sessions += 1;
    }

    let mut out: HashMap<Provider, Vec<(String, ModelStats)>> = HashMap::new();
    for provider in PROVIDER_ORDER {
        let mut rows: Vec<(String, ModelStats)> = grouped
            .remove(&provider)
            .unwrap_or_default()
            .into_iter()
            .collect();
        rows.sort_by(|a, b| {
            b.1.tokens
                .cmp(&a.1.tokens)
                .then_with(|| b.1.sessions.cmp(&a.1.sessions))
                .then_with(|| a.0.cmp(&b.0))
        });
        out.insert(provider, rows);
    }
    out
}

fn normalized_model_label(session: &AgentSession) -> String {
    let model = session.model.trim();
    if !model.is_empty() && model != "-" {
        model.to_string()
    } else {
        match session.agent_cli {
            "claude" => "claude".to_string(),
            "codex" => "codex".to_string(),
            "pi" => "pi-go".to_string(),
            _ => "unknown".to_string(),
        }
    }
}

pub(crate) fn draw_providers_panel(f: &mut Frame, app: &App, area: Rect, theme: &Theme) {
    let stats = aggregate(&app.sessions);
    let model_breakdown = aggregate_models_by_provider(&app.sessions);
    let total_tokens: u64 = stats.iter().map(|(_, s)| s.tokens).sum();

    let block = btop_block("providers", "⁶", theme.mem_box, theme);
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

    // Leading " Anthrop:" (9) + trailing " 99.9k×8" (~9) + breathing room.
    let bar_w = (inner.width as usize).saturating_sub(20).clamp(4, 40);

    let mut lines: Vec<Line> = Vec::new();

    // Total header
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

    // One meter per provider
    for (provider, s) in &stats {
        let pct = if total_tokens > 0 {
            s.tokens as f64 / total_tokens as f64 * 100.0
        } else {
            0.0
        };
        let provider_grad = provider_gradient(*provider, theme);
        let label = format!(" {:<7}:", shorten_provider(provider.label()));

        let mut spans = vec![Span::styled(
            label,
            Style::default().fg(provider.color(theme)),
        )];
        spans.extend(meter_bar(pct, bar_w, &provider_grad, theme.meter_bg));

        // Count annotation: tokens × session count. When no tokens were
        // recorded (e.g. Cursor), still show the session count — the user
        // then knows the provider *exists* but has no telemetry.
        let count_span = if s.tokens > 0 {
            format!(" {}×{}", fmt_tokens(s.tokens), s.sessions)
        } else if s.sessions > 0 {
            format!(" — ×{}", s.sessions)
        } else {
            String::new()
        };
        spans.push(Span::styled(
            count_span,
            Style::default().fg(provider.color(theme)),
        ));
        lines.push(Line::from(spans));
    }

    // Model breakdown: show top model for each provider that has sessions.
    if inner.height as usize >= lines.len() + 2 {
        for (provider, s) in &stats {
            if s.sessions == 0 {
                continue;
            }
            let Some(models) = model_breakdown.get(provider) else {
                continue;
            };
            let Some((model, ms)) = models.first() else {
                continue;
            };
            let model_name = shorten_model(model, 13);
            let text = if ms.tokens > 0 {
                format!("  {} {} {}", provider.short(), model_name, fmt_tokens(ms.tokens))
            } else {
                format!("  {} {} ×{}", provider.short(), model_name, ms.sessions)
            };
            lines.push(Line::from(Span::styled(
                text,
                Style::default().fg(theme.graph_text),
            )));
            if lines.len() + 2 >= inner.height as usize {
                break;
            }
        }
    }

    // Stacked legend bar + mini "Anthrop 52% · OpenAI 30% …" summary.
    // Skip when total is 0 (everything would be dashes anyway) or height is tight.
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
        for (provider, s) in &stats {
            if s.tokens == 0 {
                continue;
            }
            if !first {
                summary_spans.push(Span::styled(" · ", Style::default().fg(theme.graph_text)));
            }
            first = false;
            let pct = s.tokens as f64 / total_tokens as f64 * 100.0;
            summary_spans.push(Span::styled(
                format!("{} {:.0}%", provider.short(), pct),
                Style::default().fg(provider.color(theme)),
            ));
        }
        if first {
            // nothing had tokens
            summary_spans.push(Span::styled(
                "no token data",
                Style::default().fg(theme.inactive_fg),
            ));
        }
        lines.push(Line::from(summary_spans));
    } else if total_tokens == 0 {
        // Surface a friendly message so the panel isn't silent.
        while lines.len() + 1 < inner.height as usize {
            lines.push(Line::from(""));
        }
        lines.push(Line::from(Span::styled(
            " no token telemetry yet",
            Style::default().fg(theme.inactive_fg),
        )));
    }

    // Highlight a future "working" indicator in the bottom-right if we ended
    // up with extra space.
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

/// Build a one-row horizontal stacked bar: `■■■■■■` colored per provider.
/// Providers with 0 tokens contribute no cells. Floor-rounding with a
/// remainder pass so the bar always fills exactly `width` cells.
fn stacked_bar_spans(
    stats: &[(Provider, ProviderStats)],
    total_tokens: u64,
    width: usize,
    theme: &Theme,
) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    if width == 0 || total_tokens == 0 {
        return spans;
    }

    // First pass: floor-allocate cells per provider, track remainders.
    let mut allocs: Vec<(Provider, usize, f64)> = stats
        .iter()
        .filter(|(_, s)| s.tokens > 0)
        .map(|(p, s)| {
            let exact = (s.tokens as f64 / total_tokens as f64) * width as f64;
            let floor = exact.floor() as usize;
            (*p, floor, exact - floor as f64)
        })
        .collect();

    let used: usize = allocs.iter().map(|(_, n, _)| *n).sum();
    let mut remaining = width.saturating_sub(used);

    // Second pass: hand out remainders to the largest fractional parts.
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

    for (provider, cells, _) in allocs {
        for _ in 0..cells {
            spans.push(Span::styled(
                "■",
                Style::default().fg(provider.color(theme)),
            ));
        }
    }
    spans
}

fn provider_gradient(provider: Provider, theme: &Theme) -> [Color; 101] {
    // Use the box's tinted version of the provider color so the meter still
    // reads as "a bar" (darker left, brighter right). For Other, fall back to
    // the theme's free-gradient so it doesn't overwhelm the provider colors.
    let base = match provider {
        Provider::Anthropic => (217, 119, 87),
        Provider::OpenAI => (122, 157, 255),
        Provider::Ollama => (168, 139, 250),
        Provider::Other => {
            return make_gradient(theme.free_grad.start, theme.free_grad.mid, theme.free_grad.end);
        }
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

/// Trim the provider label to 7 chars so the column aligns: "Anthrop",
/// "OpenAI", "Google", "Other".
fn shorten_provider(s: &str) -> String {
    if s.len() <= 7 {
        s.to_string()
    } else {
        s.chars().take(7).collect()
    }
}

fn shorten_model(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(width.saturating_sub(1)).collect();
        out.push('~');
        out
    }
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
    fn test_claude_always_anthropic() {
        let s = mk("claude", "whatever", 100, 50);
        assert_eq!(provider_for_session(&s), Provider::Anthropic);
    }

    #[test]
    fn test_codex_always_openai() {
        let s = mk("codex", "", 10, 10);
        assert_eq!(provider_for_session(&s), Provider::OpenAI);
    }

    #[test]
    fn test_pi_model_inference() {
        assert_eq!(
            provider_for_session(&mk("pi", "gemini-2.5-pro", 0, 0)),
            Provider::Other
        );
        assert_eq!(
            provider_for_session(&mk("pi", "claude-opus-4-6", 0, 0)),
            Provider::Anthropic
        );
        assert_eq!(
            provider_for_session(&mk("pi", "gpt-5-codex", 0, 0)),
            Provider::OpenAI
        );
        assert_eq!(
            provider_for_session(&mk("pi", "qwen3.5:cloud", 0, 0)),
            Provider::Ollama
        );
        assert_eq!(
            provider_for_session(&mk("pi", "minimax-m2.5:local", 0, 0)),
            Provider::Ollama
        );
        assert_eq!(
            provider_for_session(&mk("pi", "ollama/qwen3.5:latest", 0, 0)),
            Provider::Ollama
        );
        assert_eq!(
            provider_for_session(&mk("pi", "llama3:8b", 0, 0)),
            Provider::Ollama
        );
        assert_eq!(
            provider_for_session(&mk("pi", "", 0, 0)),
            Provider::Ollama
        );
    }

    #[test]
    fn test_cursor_is_other() {
        let s = mk("cursor", "-", 0, 0);
        assert_eq!(provider_for_session(&s), Provider::Other);
    }

    #[test]
    fn test_aggregate_totals() {
        let now_ms = chrono::Local::now().timestamp_millis() as u64;
        let mut sessions = vec![
            mk("claude", "claude-opus-4-6", 100, 50),
            mk("claude", "claude-sonnet-4-6", 200, 100),
            mk("codex", "gpt-5-codex", 1_000, 500),
            mk("pi", "gemini-2.5-pro", 2_000, 1_000),
            mk("pi", "qwen3.5:cloud", 500, 500),
            mk("cursor", "-", 0, 0),
        ];
        for s in &mut sessions {
            s.started_at = now_ms;
        }
        let stats = aggregate(&sessions);
        let totals: std::collections::HashMap<Provider, u64> =
            stats.iter().map(|(p, s)| (*p, s.tokens)).collect();
        assert_eq!(totals[&Provider::Anthropic], 450); // 150 + 300
        assert_eq!(totals[&Provider::OpenAI], 1_500);
        assert_eq!(totals[&Provider::Ollama], 1_000);
        assert_eq!(totals[&Provider::Other], 3_000);

        // Session counts include zero-token providers.
        let counts: std::collections::HashMap<Provider, u32> =
            stats.iter().map(|(p, s)| (*p, s.sessions)).collect();
        assert_eq!(counts[&Provider::Anthropic], 2);
        assert_eq!(counts[&Provider::Ollama], 1);
        assert_eq!(counts[&Provider::Other], 2); // gemini + cursor sessions
    }

    #[test]
    fn test_aggregate_preserves_provider_order() {
        let stats = aggregate(&[]);
        assert_eq!(stats.len(), 4);
        assert_eq!(stats[0].0, Provider::Anthropic);
        assert_eq!(stats[1].0, Provider::OpenAI);
        assert_eq!(stats[2].0, Provider::Ollama);
        assert_eq!(stats[3].0, Provider::Other);
    }

    #[test]
    fn test_stacked_bar_allocates_exactly_width() {
        let now_ms = chrono::Local::now().timestamp_millis() as u64;
        let mut sessions = vec![
            mk("claude", "claude-opus-4-6", 30, 0),
            mk("codex", "gpt-5", 20, 0),
            mk("pi", "gemini-2.5-pro", 50, 0),
        ];
        for s in &mut sessions {
            s.started_at = now_ms;
        }
        let stats = aggregate(&sessions);
        let total: u64 = stats.iter().map(|(_, s)| s.tokens).sum();
        let theme = Theme::btop();
        for width in [0, 1, 7, 10, 23, 40, 100] {
            let spans = stacked_bar_spans(&stats, total, width, &theme);
            // The bar always occupies exactly `width` cells when there is
            // token data — never over or under, even with remainder rounding.
            assert_eq!(spans.len(), width, "width={}", width);
        }
    }

    #[test]
    fn test_stacked_bar_zero_total_is_empty() {
        let theme = Theme::btop();
        let spans = stacked_bar_spans(&aggregate(&[]), 0, 20, &theme);
        assert!(spans.is_empty());
    }

    #[test]
    fn test_aggregate_models_by_provider_sorts_by_tokens() {
        let sessions = vec![
            mk("claude", "claude-opus-4-6", 50, 50),
            mk("claude", "claude-opus-4-6", 10, 10),
            mk("claude", "claude-sonnet-4-6", 20, 20),
        ];
        let models = aggregate_models_by_provider(&sessions);
        let anth = models.get(&Provider::Anthropic).unwrap();
        assert_eq!(anth[0].0, "claude-opus-4-6");
        assert_eq!(anth[0].1.tokens, 120);
        assert_eq!(anth[1].0, "claude-sonnet-4-6");
    }

    #[test]
    fn test_normalized_model_label_fallback() {
        let s = mk("codex", "-", 1, 1);
        assert_eq!(normalized_model_label(&s), "codex");
    }
}
