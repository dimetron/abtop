use crate::app::App;
use crate::model::RateLimitInfo;
use crate::theme::Theme;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::{btop_block, fmt_tokens, grad_at, make_gradient, remaining_bar, styled_label};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum QuotaProvider {
    Anthropic,
    OpenAI,
    Ollama,
    Other,
}

const QUOTA_PROVIDER_ORDER: [QuotaProvider; 4] = [
    QuotaProvider::Anthropic,
    QuotaProvider::OpenAI,
    QuotaProvider::Ollama,
    QuotaProvider::Other,
];

impl QuotaProvider {
    fn short(self) -> &'static str {
        match self {
            QuotaProvider::Anthropic => "ANT",
            QuotaProvider::OpenAI => "OAI",
            QuotaProvider::Ollama => "OLM",
            QuotaProvider::Other => "OTH",
        }
    }
}

pub(crate) fn draw_quota_panel(f: &mut Frame, app: &App, area: Rect, theme: &Theme) {
    let cpu_grad = make_gradient(theme.cpu_grad.start, theme.cpu_grad.mid, theme.cpu_grad.end);

    let block = btop_block("quota(left)", "²", theme.cpu_box, theme);
    f.render_widget(block, area);

    let inner = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    };

    // Bottom summary: total tokens + rate
    let total_tokens: u64 = app.sessions.iter().map(|s| s.total_tokens()).sum();
    let rates = &app.token_rates;
    let ticks_per_min = 30usize;
    let tokens_per_min: f64 = rates.iter().rev().take(ticks_per_min).sum();
    // Split into side-by-side columns: one per known provider.
    let num_sources = QUOTA_PROVIDER_ORDER.len() as u16;
    let col_w = inner.width / num_sources;
    let content_h = inner.height.saturating_sub(1); // reserve last row for totals

    for (i, provider) in QUOTA_PROVIDER_ORDER.iter().enumerate() {
        let rl = rate_limit_for_provider(&app.rate_limits, *provider);
        let col_x = inner.x + (i as u16) * col_w;
        let this_w = if i as u16 == num_sources - 1 {
            inner.width - (i as u16) * col_w
        } else {
            col_w
        };
        let col_area = Rect { x: col_x, y: inner.y, width: this_w, height: content_h };
        let col_w_usize = col_area.width as usize;
        // " 5h " (4) + bar + " 100%" (~5) + padding → reserve ~10.
        // Previously capped at 8, so bars were always tiny. Let them grow.
        let bar_w = col_w_usize.saturating_sub(10).clamp(2, 30);

        let mut lines: Vec<Line> = Vec::new();

        // Source label with freshness
        let fresh_str = rl.and_then(|r| r.updated_at).map(|ts| {
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
            let ago = now.saturating_sub(ts);
            if ago < 60 { format!(" {}s ago", ago) } else { format!(" {}m ago", ago / 60) }
        }).unwrap_or_default();
        let label = format!(" {}{}", provider.short(), fresh_str);
        lines.push(Line::from(Span::styled(label, Style::default().fg(theme.title).add_modifier(Modifier::BOLD))));

        if let Some(rl) = rl {
            if let Some(used_pct) = rl.five_hour_pct {
                let remaining = (100.0 - used_pct).clamp(0.0, 100.0);
                let reset = rl.five_hour_resets_at.map(format_reset_time).unwrap_or_default();
                // Color by urgency: low remaining = red (high used), high remaining = green
                let c = grad_at(&cpu_grad, used_pct);
                let mut s = vec![styled_label(" 5h ", theme.graph_text)];
                s.extend(remaining_bar(remaining, bar_w, &cpu_grad, theme.meter_bg));
                s.push(Span::styled(format!(" {:>3.0}%", remaining), Style::default().fg(c)));
                lines.push(Line::from(s));
                if !reset.is_empty() {
                    lines.push(Line::from(Span::styled(format!("  {}", reset), Style::default().fg(theme.graph_text))));
                }
            } else {
                lines.push(Line::from(vec![
                    styled_label(" 5h ", theme.graph_text),
                    Span::styled("—", Style::default().fg(theme.inactive_fg)),
                ]));
            }
            if let Some(used_pct) = rl.seven_day_pct {
                let remaining = (100.0 - used_pct).clamp(0.0, 100.0);
                let reset = rl.seven_day_resets_at.map(format_reset_time).unwrap_or_default();
                let c = grad_at(&cpu_grad, used_pct);
                let mut s = vec![styled_label(" 7d ", theme.graph_text)];
                s.extend(remaining_bar(remaining, bar_w, &cpu_grad, theme.meter_bg));
                s.push(Span::styled(format!(" {:>3.0}%", remaining), Style::default().fg(c)));
                lines.push(Line::from(s));
                if !reset.is_empty() {
                    lines.push(Line::from(Span::styled(format!("  {}", reset), Style::default().fg(theme.graph_text))));
                }
            } else {
                lines.push(Line::from(vec![
                    styled_label(" 7d ", theme.graph_text),
                    Span::styled("—", Style::default().fg(theme.inactive_fg)),
                ]));
            }
        } else {
            lines.push(Line::from(vec![
                styled_label(" 5h ", theme.graph_text),
                Span::styled("—", Style::default().fg(theme.inactive_fg)),
            ]));
            lines.push(Line::from(vec![
                styled_label(" 7d ", theme.graph_text),
                Span::styled("—", Style::default().fg(theme.inactive_fg)),
            ]));
        }

        f.render_widget(Paragraph::new(lines), col_area);
    }

    // Total tokens summary on last row (full width)
    let bottom_area = Rect {
        x: inner.x,
        y: inner.y + content_h,
        width: inner.width,
        height: 1,
    };
    f.render_widget(Paragraph::new(vec![Line::from(vec![
        Span::styled(format!(" {}", fmt_tokens(total_tokens)), Style::default().fg(theme.main_fg)),
        Span::styled(format!(" {}/min", fmt_tokens(tokens_per_min as u64)), Style::default().fg(theme.graph_text)),
    ])]), bottom_area);
}

fn provider_from_rate_source(source: &str) -> QuotaProvider {
    let s = source.to_ascii_lowercase();
    if s.contains("claude") || s.contains("anthropic") {
        QuotaProvider::Anthropic
    } else if s.contains("codex") || s.contains("openai") {
        QuotaProvider::OpenAI
    } else if s.contains("ollama") {
        QuotaProvider::Ollama
    } else {
        QuotaProvider::Other
    }
}

fn rate_limit_for_provider(rate_limits: &[RateLimitInfo], provider: QuotaProvider) -> Option<&RateLimitInfo> {
    rate_limits
        .iter()
        .filter(|rl| provider_from_rate_source(&rl.source) == provider)
        .max_by_key(|rl| rl.updated_at.unwrap_or(0))
}

/// Format a reset timestamp as relative time (e.g., "1h 23m")
pub(crate) fn format_reset_time(reset_ts: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    if reset_ts <= now {
        return "now".to_string();
    }
    let diff = reset_ts - now;
    if diff < 60 {
        format!("{}s", diff)
    } else if diff < 3600 {
        format!("{}m", diff / 60)
    } else if diff < 86400 {
        format!("{}h {}m", diff / 3600, (diff % 3600) / 60)
    } else {
        format!("{}d {}h", diff / 86400, (diff % 86400) / 3600)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_provider_from_rate_source() {
        assert_eq!(provider_from_rate_source("claude"), QuotaProvider::Anthropic);
        assert_eq!(provider_from_rate_source("anthropic-api"), QuotaProvider::Anthropic);
        assert_eq!(provider_from_rate_source("codex"), QuotaProvider::OpenAI);
        assert_eq!(provider_from_rate_source("openai"), QuotaProvider::OpenAI);
        assert_eq!(provider_from_rate_source("ollama"), QuotaProvider::Ollama);
        assert_eq!(provider_from_rate_source("unknown"), QuotaProvider::Other);
    }

    #[test]
    fn test_rate_limit_for_provider_prefers_latest() {
        let limits = vec![
            RateLimitInfo {
                source: "claude".into(),
                updated_at: Some(10),
                ..Default::default()
            },
            RateLimitInfo {
                source: "claude".into(),
                updated_at: Some(20),
                ..Default::default()
            },
        ];
        let picked = rate_limit_for_provider(&limits, QuotaProvider::Anthropic).unwrap();
        assert_eq!(picked.updated_at, Some(20));
    }
}
