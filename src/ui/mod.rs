use crate::app::App;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Cell, Paragraph, Row, Table};
use ratatui::Frame;

// ── btop default theme — exact RGB values from btop_theme.cpp Default_theme ──

// Base colors
const MAIN_FG: Color = Color::Rgb(204, 204, 204);       // #cc
const TITLE: Color = Color::Rgb(238, 238, 238);          // #ee
const HI_FG: Color = Color::Rgb(181, 64, 64);            // #b54040
const SELECTED_BG: Color = Color::Rgb(106, 47, 47);      // #6a2f2f
const SELECTED_FG: Color = Color::Rgb(238, 238, 238);    // #ee
const INACTIVE_FG: Color = Color::Rgb(64, 64, 64);       // #40
const GRAPH_TEXT: Color = Color::Rgb(96, 96, 96);         // #60
const METER_BG: Color = Color::Rgb(64, 64, 64);          // #40
const PROC_MISC: Color = Color::Rgb(13, 231, 86);        // #0de756
const DIV_LINE: Color = Color::Rgb(48, 48, 48);          // #30
const SESSION_ID: Color = Color::Rgb(176, 160, 112);     // #b0a070 muted amber

// Box border colors (per panel, muted tones)
const CPU_BOX: Color = Color::Rgb(85, 109, 89);          // #556d59
const MEM_BOX: Color = Color::Rgb(108, 108, 75);         // #6c6c4b
const NET_BOX: Color = Color::Rgb(92, 88, 141);          // #5c588d
const PROC_BOX: Color = Color::Rgb(128, 82, 82);         // #805252

// Gradient: cpu (green → yellow → red) — used for context bars
const CPU_START: (u8, u8, u8) = (119, 202, 155);         // #77ca9b
const CPU_MID: (u8, u8, u8) = (203, 192, 108);           // #cbc06c
const CPU_END: (u8, u8, u8) = (220, 76, 76);             // #dc4c4c

// Gradient: process (same green → yellow → red)
const PROC_START: (u8, u8, u8) = (128, 208, 163);        // #80d0a3
const PROC_MID: (u8, u8, u8) = (220, 209, 121);          // #dcd179
const PROC_END: (u8, u8, u8) = (212, 84, 84);            // #d45454

// Gradient: used (red/pink, for memory-like bars)
const USED_START: (u8, u8, u8) = (89, 43, 38);           // #592b26
const USED_MID: (u8, u8, u8) = (217, 98, 109);           // #d9626d
const USED_END: (u8, u8, u8) = (255, 71, 105);           // #ff4769

// Gradient: free (green)
const FREE_START: (u8, u8, u8) = (56, 79, 33);           // #384f21
const FREE_MID: (u8, u8, u8) = (181, 230, 133);          // #b5e685
const FREE_END: (u8, u8, u8) = (220, 255, 133);          // #dcff85

// Gradient: cached (cyan/blue)
const CACHED_START: (u8, u8, u8) = (22, 51, 80);         // #163350
const CACHED_MID: (u8, u8, u8) = (116, 230, 252);        // #74e6fc
const CACHED_END: (u8, u8, u8) = (38, 197, 255);         // #26c5ff

// ── braille graph symbols — from btop_draw.cpp ──────────────────────────────
// 5x5 lookup: [prev_val * 5 + cur_val], values 0-4
const BRAILLE_UP: [&str; 25] = [
    " ", "⢀", "⢠", "⢰", "⢸",
    "⡀", "⣀", "⣠", "⣰", "⣸",
    "⡄", "⣄", "⣤", "⣴", "⣼",
    "⡆", "⣆", "⣦", "⣶", "⣾",
    "⡇", "⣇", "⣧", "⣷", "⣿",
];

// ── gradient interpolation (btop-faithful: linear RGB, 101 steps) ────────────

/// Generate 101-step gradient from start→mid→end, matching btop's generateGradients().
fn make_gradient(start: (u8, u8, u8), mid: (u8, u8, u8), end: (u8, u8, u8)) -> [Color; 101] {
    let mut out = [Color::Reset; 101];
    for i in 0..=100 {
        let (s, e, offset, range) = if i <= 50 {
            (start, mid, 0, 50)
        } else {
            (mid, end, 50, 50)
        };
        let t = i - offset;
        let r = s.0 as i32 + t as i32 * (e.0 as i32 - s.0 as i32) / range;
        let g = s.1 as i32 + t as i32 * (e.1 as i32 - s.1 as i32) / range;
        let b = s.2 as i32 + t as i32 * (e.2 as i32 - s.2 as i32) / range;
        out[i] = Color::Rgb(r.clamp(0, 255) as u8, g.clamp(0, 255) as u8, b.clamp(0, 255) as u8);
    }
    out
}

/// Pick color from a gradient at a given percentage.
fn grad_at(gradient: &[Color; 101], pct: f64) -> Color {
    let idx = (pct.clamp(0.0, 100.0)).round() as usize;
    gradient[idx.min(100)]
}

// ── btop-style meter bar using ■ character ───────────────────────────────────

/// Render a btop-style meter: filled ■ with gradient color, empty ■ with meter_bg.
fn meter_bar(pct: f64, width: usize, gradient: &[Color; 101]) -> Vec<Span<'static>> {
    if width == 0 {
        return Vec::new();
    }
    let clamped = pct.clamp(0.0, 100.0);
    let filled = ((clamped / 100.0) * width as f64).round() as usize;
    let mut spans = Vec::new();
    for i in 0..width {
        if i < filled {
            let cell_pct = (i as f64 / width as f64) * 100.0;
            spans.push(Span::styled(
                "■",
                Style::default().fg(grad_at(gradient, cell_pct)),
            ));
        } else {
            spans.push(Span::styled("■", Style::default().fg(METER_BG)));
        }
    }
    spans
}

// ── braille sparkline ────────────────────────────────────────────────────────

/// Render a braille sparkline from data points (0.0–1.0), colored with gradient.
fn braille_sparkline(data: &[f64], width: usize, gradient: &[Color; 101]) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    if data.is_empty() || width == 0 {
        for _ in 0..width {
            spans.push(Span::styled(" ", Style::default().fg(GRAPH_TEXT)));
        }
        return spans;
    }

    // We need pairs of data points per braille char (prev, cur)
    // Pad or sample data to fit width * 2 points
    let needed = width * 2;
    let sampled: Vec<f64> = if data.len() >= needed {
        data[data.len() - needed..].to_vec()
    } else {
        let mut v = vec![0.0; needed - data.len()];
        v.extend_from_slice(data);
        v
    };

    for i in 0..width {
        let prev = (sampled[i * 2].clamp(0.0, 1.0) * 4.0).round() as usize;
        let cur = (sampled[i * 2 + 1].clamp(0.0, 1.0) * 4.0).round() as usize;
        let idx = prev * 5 + cur;
        let pct = (sampled[i * 2 + 1] * 100.0).round() as usize;
        let color = grad_at(gradient, pct as f64);
        spans.push(Span::styled(
            BRAILLE_UP[idx.min(24)].to_string(),
            Style::default().fg(color),
        ));
    }
    spans
}

// ── btop-style block with notch title: ──┐¹title┌────── ─────────────────────

fn btop_block(title: &str, number: &str, box_color: Color) -> Block<'static> {
    Block::default()
        .title(Line::from(vec![
            Span::styled("┐", Style::default().fg(box_color)),
            Span::styled(
                number.to_string(),
                Style::default().fg(HI_FG).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                title.to_string(),
                Style::default().fg(TITLE).add_modifier(Modifier::BOLD),
            ),
            Span::styled("┌", Style::default().fg(box_color)),
        ]))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(box_color))
}

fn styled_label(text: &str) -> Span<'static> {
    Span::styled(text.to_string(), Style::default().fg(GRAPH_TEXT))
}

// ── main draw ────────────────────────────────────────────────────────────────

pub fn draw(f: &mut Frame, app: &App) {
    let area = f.area();

    // Top panel height: header + sessions + summary + borders
    let top_h: u16 = (app.sessions.len() as u16 + 4).min(10).max(5);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),      // header bar
            Constraint::Length(top_h),  // top: context panel
            Constraint::Length(8),      // mid: quota + tokens + projects + ports
            Constraint::Min(10),       // sessions (full width)
            Constraint::Length(1),     // footer
        ])
        .split(area);

    draw_header(f, app, chunks[0]);
    draw_context_panel(f, app, chunks[1]);

    let mid_panels = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(25), // quota (rate limit)
            Constraint::Percentage(25), // tokens
            Constraint::Percentage(25), // projects
            Constraint::Percentage(25), // ports
        ])
        .split(chunks[2]);

    draw_quota_panel(f, app, mid_panels[0]);
    draw_tokens_panel(f, app, mid_panels[1]);
    draw_projects_panel(f, app, mid_panels[2]);
    draw_ports_panel(f, app, mid_panels[3]);
    draw_sessions_panel(f, app, chunks[3]);
    draw_footer(f, app, chunks[4]);
}

// ── header bar — btop style: ¹cpu ─ menu ─ preset * ── time ── BAT ──────────

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let session_count = app.sessions.len();
    let active = app
        .sessions
        .iter()
        .filter(|s| matches!(s.status, crate::model::SessionStatus::Working))
        .count();

    let now = chrono::Local::now().format("%H:%M").to_string();
    let remaining = (area.width as usize).saturating_sub(35);

    let line = Line::from(vec![
        Span::styled(" abtop ", Style::default().fg(TITLE).add_modifier(Modifier::BOLD)),
        Span::styled("─", Style::default().fg(DIV_LINE)),
        Span::styled(" agent monitor ", Style::default().fg(GRAPH_TEXT)),
        Span::styled(
            format!("{:>width$}", now, width = remaining),
            Style::default().fg(GRAPH_TEXT),
        ),
        Span::styled(format!("  {}↑", active), Style::default().fg(PROC_MISC)),
        Span::styled(format!(" {}●", session_count), Style::default().fg(MAIN_FG)),
        Span::styled("  ", Style::default()),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

// ── context panel: left = token rate sparkline, right = context bars ────────

fn draw_context_panel(f: &mut Frame, app: &App, area: Rect) {
    let cpu_grad = make_gradient(CPU_START, CPU_MID, CPU_END);

    let block = btop_block("context", "¹", CPU_BOX);
    f.render_widget(block, area);

    let inner = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    };

    // Split: left = sparkline graph, right = context bars
    let halves = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(inner);

    // ── Left: token rate braille sparkline graph ──
    draw_context_sparkline(f, app, halves[0], &cpu_grad);

    // ── Right: per-session context bars (table) ──
    draw_context_bars(f, app, halves[1], &cpu_grad);
}

fn draw_context_sparkline(f: &mut Frame, app: &App, area: Rect, cpu_grad: &[Color; 101]) {
    let avail_h = area.height as usize;
    let avail_w = area.width as usize;
    let mut lines: Vec<Line> = Vec::new();

    // Token rate sparkline (fills most of the height)
    let spark_w = avail_w.saturating_sub(2).max(4);
    let rates: Vec<f64> = app.token_rates.iter().copied().collect();
    let max_rate = rates.iter().cloned().fold(1.0_f64, f64::max);
    let normalized: Vec<f64> = rates.iter().map(|&v| v / max_rate).collect();

    // Graph title
    lines.push(Line::from(vec![
        Span::styled(" Token Rate", Style::default().fg(TITLE).add_modifier(Modifier::BOLD)),
    ]));

    // Render sparkline across available rows using braille
    let graph_h = avail_h.saturating_sub(2); // title + summary line
    for row in 0..graph_h {
        // Each row shows a horizontal slice of the sparkline
        // For simplicity, render the full sparkline on the first graph row
        if row == 0 {
            let mut spark_spans = vec![Span::styled(" ", Style::default())];
            spark_spans.extend(braille_sparkline(&normalized, spark_w, cpu_grad));
            lines.push(Line::from(spark_spans));
        } else {
            lines.push(Line::from(""));
        }
    }

    // Summary line: total tokens + rate
    let total_tokens: u64 = app.sessions.iter().map(|s| s.total_tokens()).sum();
    let ticks_per_min = 30usize;
    let tokens_per_min: f64 = rates.iter().rev().take(ticks_per_min).sum();
    lines.push(Line::from(vec![
        Span::styled(format!(" {}", fmt_tokens(total_tokens)), Style::default().fg(MAIN_FG)),
        Span::styled(format!(" {}/m", fmt_tokens(tokens_per_min as u64)), Style::default().fg(GRAPH_TEXT)),
    ]));

    f.render_widget(Paragraph::new(lines), area);
}

fn draw_context_bars(f: &mut Frame, app: &App, area: Rect, cpu_grad: &[Color; 101]) {
    let header_style = Style::default().fg(MAIN_FG).add_modifier(Modifier::BOLD);

    // bar width = remaining space after Project(14) + Session(9) + pct(5) + padding
    let bar_width = (area.width as usize).saturating_sub(30).min(60).max(4);

    let mut rows = Vec::new();

    for session in &app.sessions {
        let raw_pct = session.context_percent;
        let bar_pct = raw_pct.min(100.0);
        let warn = if raw_pct >= 90.0 { "⚠" } else { "" };
        let pct_color = grad_at(cpu_grad, bar_pct);

        let sid_short = if session.session_id.len() >= 8 {
            &session.session_id[..8]
        } else {
            &session.session_id
        };

        rows.push(Row::new(vec![
            Cell::from(Span::styled(
                truncate_str(&session.project_name, 14),
                Style::default().fg(TITLE),
            )),
            Cell::from(Span::styled(
                sid_short.to_string(),
                Style::default().fg(SESSION_ID),
            )),
            Cell::from(Line::from({
                let mut spans = meter_bar(bar_pct, bar_width, cpu_grad);
                spans.push(Span::styled(
                    format!(" {:>3.0}%{}", raw_pct, warn),
                    Style::default().fg(pct_color),
                ));
                spans
            })),
        ]));
    }

    if app.sessions.is_empty() {
        rows.push(Row::new(vec![
            Cell::from(Span::styled(
                "no active sessions",
                Style::default().fg(INACTIVE_FG),
            )),
            Cell::from(""),
            Cell::from(""),
        ]));
    }

    let header = Row::new(vec![
        Cell::from(Span::styled("Project", header_style)),
        Cell::from(Span::styled("Session", header_style)),
        Cell::from(Span::styled("Context", header_style)),
    ]);

    let widths = [
        Constraint::Length(14),
        Constraint::Length(9),
        Constraint::Min(10),
    ];

    let table = Table::new(rows, widths).header(header);
    f.render_widget(table, area);
}

// ── quota panel: rate limit gauges + token rate ─────────────────────────────

fn draw_quota_panel(f: &mut Frame, app: &App, area: Rect) {
    let cpu_grad = make_gradient(CPU_START, CPU_MID, CPU_END);

    let block = btop_block("quota", "²", CPU_BOX);
    f.render_widget(block, area);

    let inner = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    };

    let avail_h = inner.height as usize;
    let avail_w = inner.width as usize;
    let mut lines: Vec<Line> = Vec::new();

    if app.rate_limits.is_empty() {
        lines.push(Line::from(Span::styled(" QUOTA", Style::default().fg(TITLE).add_modifier(Modifier::BOLD))));
        lines.push(Line::from(Span::styled("  — unavailable", Style::default().fg(INACTIVE_FG))));
        lines.push(Line::from(Span::styled("  abtop --setup", Style::default().fg(GRAPH_TEXT))));
    } else {
        let bar_w = avail_w.saturating_sub(12).min(12).max(3);
        for rl in &app.rate_limits {
            let fresh_str = rl.updated_at.map(|ts| {
                let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
                let ago = now.saturating_sub(ts);
                if ago < 60 { format!("{}s ago", ago) } else { format!("{}m ago", ago / 60) }
            }).unwrap_or_default();
            if !fresh_str.is_empty() {
                lines.push(Line::from(Span::styled(format!(" {}", fresh_str), Style::default().fg(INACTIVE_FG))));
            }
            if let Some(pct) = rl.five_hour_pct {
                let reset = rl.five_hour_resets_at.map(|ts| format_reset_time(ts)).unwrap_or_default();
                let c = grad_at(&cpu_grad, pct);
                let mut s = vec![styled_label(" 5h ")];
                s.extend(meter_bar(pct, bar_w, &cpu_grad));
                s.push(Span::styled(format!(" {:>3.0}%", pct), Style::default().fg(c)));
                lines.push(Line::from(s));
                if !reset.is_empty() {
                    lines.push(Line::from(Span::styled(format!("  {}", reset), Style::default().fg(GRAPH_TEXT))));
                }
            }
            if let Some(pct) = rl.seven_day_pct {
                let reset = rl.seven_day_resets_at.map(|ts| format_reset_time(ts)).unwrap_or_default();
                let c = grad_at(&cpu_grad, pct);
                let mut s = vec![styled_label(" 7d ")];
                s.extend(meter_bar(pct, bar_w, &cpu_grad));
                s.push(Span::styled(format!(" {:>3.0}%", pct), Style::default().fg(c)));
                lines.push(Line::from(s));
                if !reset.is_empty() {
                    lines.push(Line::from(Span::styled(format!("  {}", reset), Style::default().fg(GRAPH_TEXT))));
                }
            }
        }
    }

    // Bottom: total tokens + rate
    let total_tokens: u64 = app.sessions.iter().map(|s| s.total_tokens()).sum();
    let rates = &app.token_rates;
    let ticks_per_min = 30usize;
    let tokens_per_min: f64 = rates.iter().rev().take(ticks_per_min).sum();

    while lines.len() < avail_h.saturating_sub(1) {
        lines.push(Line::from(""));
    }
    lines.push(Line::from(vec![
        Span::styled(format!(" {}", fmt_tokens(total_tokens)), Style::default().fg(MAIN_FG)),
        Span::styled(format!(" {}/m", fmt_tokens(tokens_per_min as u64)), Style::default().fg(GRAPH_TEXT)),
    ]));

    f.render_widget(Paragraph::new(lines), inner);
}

// ── tokens panel — maps to btop's ²mem panel ────────────────────────────────

fn draw_tokens_panel(f: &mut Frame, app: &App, area: Rect) {
    let selected = app.sessions.get(app.selected);
    let total_in: u64 = selected.map(|s| s.total_input_tokens).unwrap_or(0);
    let total_out: u64 = selected.map(|s| s.total_output_tokens).unwrap_or(0);
    let total_cache: u64 = selected
        .map(|s| s.total_cache_read + s.total_cache_create)
        .unwrap_or(0);
    let total: u64 = total_in + total_out + total_cache;
    let turns: u32 = selected.map(|s| s.turn_count).unwrap_or(0);
    let avg = if turns > 0 { total / turns as u64 } else { 0 };

    // Compute percentages for mini meter bars
    let (in_pct, out_pct, cache_pct) = if total > 0 {
        (
            total_in as f64 / total as f64 * 100.0,
            total_out as f64 / total as f64 * 100.0,
            total_cache as f64 / total as f64 * 100.0,
        )
    } else {
        (0.0, 0.0, 0.0)
    };

    let free_grad = make_gradient(FREE_START, FREE_MID, FREE_END);
    let used_grad = make_gradient(USED_START, USED_MID, USED_END);
    let cached_grad = make_gradient(CACHED_START, CACHED_MID, CACHED_END);

    let bar_w = (area.width as usize).saturating_sub(20).min(15).max(5);

    let total_line = vec![
        styled_label(" Total: "),
        Span::styled(
            fmt_tokens(total),
            Style::default().fg(TITLE).add_modifier(Modifier::BOLD),
        ),
    ];

    let mut input_line = vec![styled_label(" Input: ")];
    input_line.extend(meter_bar(in_pct, bar_w, &free_grad));
    input_line.push(Span::styled(
        format!(" {}", fmt_tokens(total_in)),
        Style::default().fg(grad_at(&free_grad, 80.0)),
    ));

    let mut output_line = vec![styled_label(" Output: ")];
    output_line.extend(meter_bar(out_pct, bar_w, &used_grad));
    output_line.push(Span::styled(
        format!(" {}", fmt_tokens(total_out)),
        Style::default().fg(grad_at(&used_grad, 80.0)),
    ));

    let mut cache_line = vec![styled_label(" Cache: ")];
    cache_line.extend(meter_bar(cache_pct, bar_w, &cached_grad));
    cache_line.push(Span::styled(
        format!(" {}", fmt_tokens(total_cache)),
        Style::default().fg(grad_at(&cached_grad, 80.0)),
    ));

    // Per-turn sparkline from selected session's token_history
    let cpu_grad = make_gradient(CPU_START, CPU_MID, CPU_END);
    let all_history: Vec<u64> = app
        .sessions
        .get(app.selected)
        .map(|s| s.token_history.clone())
        .unwrap_or_default();
    let spark_w = (area.width as usize).saturating_sub(16).min(20).max(5);
    let max_val = all_history.iter().copied().max().unwrap_or(1).max(1);
    let normalized: Vec<f64> = all_history
        .iter()
        .map(|&v| v as f64 / max_val as f64)
        .collect();
    let mut spark_line_spans = vec![styled_label(" ")];
    spark_line_spans.extend(braille_sparkline(&normalized, spark_w, &cpu_grad));
    spark_line_spans.push(Span::styled(" tokens/turn", Style::default().fg(GRAPH_TEXT)));

    let lines = vec![
        Line::from(total_line),
        Line::from(input_line),
        Line::from(output_line),
        Line::from(cache_line),
        Line::from(spark_line_spans),
        Line::from(vec![
            styled_label(" Turns: "),
            Span::styled(format!("{}", turns), Style::default().fg(MAIN_FG)),
            styled_label("  Avg: "),
            Span::styled(
                format!("{}/t", fmt_tokens(avg)),
                Style::default().fg(GRAPH_TEXT),
            ),
        ]),
    ];

    let panel_title = if let Some(s) = selected {
        format!("tokens ({})", truncate_str(&s.project_name, 12))
    } else {
        "tokens".to_string()
    };
    let block = btop_block(&panel_title, "³", MEM_BOX);
    f.render_widget(Paragraph::new(lines).block(block), area);
}


/// Format a reset timestamp as relative time (e.g., "1h 23m")
fn format_reset_time(reset_ts: u64) -> String {
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

// ── projects panel — maps to btop's disks ────────────────────────────────────

fn draw_projects_panel(f: &mut Frame, app: &App, area: Rect) {
    let mut lines = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for session in &app.sessions {
        if !seen.insert(&session.project_name) {
            continue;
        }
        lines.push(Line::from(vec![Span::styled(
            format!(" {}", truncate_str(&session.project_name, 14)),
            Style::default()
                .fg(TITLE)
                .add_modifier(Modifier::BOLD),
        )]));
        let branch = if session.git_branch.is_empty() {
            "no git".to_string()
        } else {
            session.git_branch.clone()
        };
        let used_grad = make_gradient(USED_START, USED_MID, USED_END);
        let branch_color = if session.git_branch.is_empty() { INACTIVE_FG } else { MAIN_FG };
        let mut branch_spans = vec![
            Span::styled("   ", Style::default()),
            Span::styled(branch, Style::default().fg(branch_color)),
        ];
        if session.git_added > 0 || session.git_modified > 0 {
            branch_spans.push(Span::styled(" ", Style::default()));
            if session.git_added > 0 {
                branch_spans.push(Span::styled(
                    format!("+{}", session.git_added),
                    Style::default().fg(PROC_MISC),
                ));
            }
            if session.git_modified > 0 {
                if session.git_added > 0 {
                    branch_spans.push(Span::styled(" ", Style::default()));
                }
                branch_spans.push(Span::styled(
                    format!("~{}", session.git_modified),
                    Style::default().fg(grad_at(&used_grad, 70.0)),
                ));
            }
        } else {
            branch_spans.push(Span::styled(" ✓clean", Style::default().fg(PROC_MISC)));
        }
        lines.push(Line::from(branch_spans));
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            " no projects",
            Style::default().fg(INACTIVE_FG),
        )));
    }

    let block = btop_block("projects", "", MEM_BOX);
    f.render_widget(Paragraph::new(lines).block(block), area);
}

// ── ports panel — maps to btop's ³net ────────────────────────────────────────

fn draw_ports_panel(f: &mut Frame, app: &App, area: Rect) {
    let mut all_ports: Vec<(u16, String, String, u32)> = Vec::new();
    for session in &app.sessions {
        for child in &session.children {
            if let Some(port) = child.port {
                let cmd = child.command.split_whitespace().next().unwrap_or("?");
                let cmd = cmd.rsplit('/').next().unwrap_or(cmd);
                all_ports.push((
                    port,
                    session.project_name.clone(),
                    cmd.to_string(),
                    child.pid,
                ));
            }
        }
    }
    all_ports.sort_by_key(|p| p.0);

    let mut port_counts: std::collections::HashMap<u16, usize> =
        std::collections::HashMap::new();
    for (port, _, _, _) in &all_ports {
        *port_counts.entry(*port).or_default() += 1;
    }

    let proc_grad = make_gradient(PROC_START, PROC_MID, PROC_END);

    let header_style = Style::default().fg(MAIN_FG).add_modifier(Modifier::BOLD);
    let mut lines = vec![Line::from(vec![
        Span::styled(" PORT  ", header_style),
        Span::styled("SESSION    ", header_style),
        Span::styled("CMD      ", header_style),
        Span::styled("PID", header_style),
    ])];
    for (port, proj, cmd, pid) in &all_ports {
        let conflict = port_counts.get(port).copied().unwrap_or(0) > 1;
        let color = if conflict {
            grad_at(&proc_grad, 100.0)
        } else {
            PROC_MISC
        };
        let warn = if conflict { " ⚠" } else { "" };
        lines.push(Line::from(vec![
            Span::styled(format!(" :{:<5}", port), Style::default().fg(color)),
            Span::styled(
                format!("{:<10}", truncate_str(proj, 10)),
                Style::default().fg(MAIN_FG),
            ),
            Span::styled(
                format!("{:<8}", truncate_str(cmd, 8)),
                Style::default().fg(GRAPH_TEXT),
            ),
            Span::styled(format!("{}{}", pid, warn), Style::default().fg(color)),
        ]));
    }

    if lines.len() <= 1 {
        lines.push(Line::from(Span::styled(
            " no open ports",
            Style::default().fg(INACTIVE_FG),
        )));
    }

    let block = btop_block("ports", "⁴", NET_BOX);
    f.render_widget(Paragraph::new(lines).block(block), area);
}

// ── sessions panel — maps to btop's ⁴proc ───────────────────────────────────

fn draw_sessions_panel(f: &mut Frame, app: &App, area: Rect) {
    // Render the outer block
    let block = btop_block("sessions", "⁵", PROC_BOX);
    f.render_widget(block, area);

    let inner = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    };

    // Session list: 1 header + 2 rows per session (main + 1 task line)
    let session_rows: u16 = app.sessions.len() as u16 * 2;
    let detail_reserve = 5.min(inner.height / 2);
    let max_table = inner.height.saturating_sub(detail_reserve);
    let table_h = (1 + session_rows).min(max_table);

    let panel_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(table_h),
            Constraint::Min(0),
        ])
        .split(inner);

    // ── Session list table ──
    let proc_grad = make_gradient(PROC_START, PROC_MID, PROC_END);
    let mut rows = Vec::new();

    for (i, session) in app.sessions.iter().enumerate() {
        let selected = i == app.selected;
        let marker = if selected { "►" } else { " " };

        let (agent_label, agent_color) = match session.agent_cli {
            "claude" => ("*CC", Color::Rgb(217, 119, 87)),  // #D97757 terracotta
            "codex"  => (">CD", Color::Rgb(122, 157, 255)), // #7A9DFF periwinkle
            other => {
                // Future agents: uppercase first 3 chars of name
                let fallback: String = other.chars().take(3).collect::<String>().to_uppercase();
                // Leak to get 'static — bounded by number of distinct agent types
                (Box::leak(fallback.into_boxed_str()) as &str, INACTIVE_FG)
            }
        };

        let (status_icon, status_color) = match &session.status {
            crate::model::SessionStatus::Working => ("● Work", PROC_MISC),
            crate::model::SessionStatus::Waiting => {
                ("◌ Wait", grad_at(&proc_grad, 50.0))
            }
            crate::model::SessionStatus::Error(_) => {
                ("✗ Err ", grad_at(&proc_grad, 100.0))
            }
            crate::model::SessionStatus::Done => ("✓ Done", INACTIVE_FG),
        };

        let model_short = shorten_model(&session.model);
        let ctx_color = grad_at(&proc_grad, session.context_percent);

        let row_style = if selected {
            Style::default()
                .bg(SELECTED_BG)
                .fg(SELECTED_FG)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };

        let sid_short = if session.session_id.len() >= 8 {
            &session.session_id[..8]
        } else {
            &session.session_id
        };

        // Summary column: LLM summary > pending > raw prompt > "—"
        let summary_col = app.session_summary(session);

        rows.push(
            Row::new(vec![
                Cell::from(Span::styled(marker, Style::default().fg(HI_FG))),
                Cell::from(Span::styled(agent_label, Style::default().fg(agent_color))),
                Cell::from(Span::styled(
                    format!("{}", session.pid),
                    Style::default().fg(INACTIVE_FG),
                )),
                Cell::from(Span::styled(
                    truncate_str(&session.project_name, 14),
                    Style::default().fg(TITLE),
                )),
                Cell::from(Span::styled(
                    sid_short.to_string(),
                    Style::default().fg(SESSION_ID),
                )),
                Cell::from(Span::styled(
                    summary_col,
                    Style::default().fg(MAIN_FG),
                )),
                Cell::from(Span::styled(status_icon, Style::default().fg(status_color))),
                Cell::from(Span::styled(
                    truncate_str(&model_short, 8),
                    Style::default().fg(GRAPH_TEXT),
                )),
                Cell::from(Span::styled(
                    format!("{:>3.0}%", session.context_percent),
                    Style::default().fg(ctx_color),
                )),
                Cell::from(Span::styled(
                    fmt_tokens(session.total_tokens()),
                    Style::default().fg(MAIN_FG),
                )),
                Cell::from(Span::styled(
                    if session.mem_mb > 0 {
                        format!("{}M", session.mem_mb)
                    } else {
                        "—".to_string()
                    },
                    Style::default().fg(GRAPH_TEXT),
                )),
                Cell::from(Span::styled(
                    format!("{}", session.turn_count),
                    Style::default().fg(GRAPH_TEXT),
                )),
            ])
            .style(row_style)
            .height(1),
        );

        // 2nd line: current task (last tool_use = currently running)
        // Placed in Summary column (index 5) so it uses the wide Min() space
        let task_text = session.current_tasks.last().map(|s| s.as_str()).unwrap_or("");
        rows.push(
            Row::new(vec![
                Cell::from(""),
                Cell::from(""),
                Cell::from(""),
                Cell::from(""),
                Cell::from(""),
                Cell::from(Span::styled(
                    format!("└─ {}", task_text),
                    Style::default().fg(GRAPH_TEXT),
                )),
                Cell::from(""),
                Cell::from(""),
                Cell::from(""),
                Cell::from(""),
                Cell::from(""),
                Cell::from(""),
            ])
            .height(1),
        );
    }

    let header_style = Style::default()
        .fg(MAIN_FG)
        .add_modifier(Modifier::BOLD);
    let header = Row::new(vec![
        Cell::from(""),
        Cell::from(Span::styled("AI", header_style)),
        Cell::from(Span::styled("Pid", header_style)),
        Cell::from(Span::styled("Project", header_style)),
        Cell::from(Span::styled("Session", header_style)),
        Cell::from(Span::styled("Summary", header_style)),
        Cell::from(Span::styled("Status", header_style)),
        Cell::from(Span::styled("Model", header_style)),
        Cell::from(Span::styled("Context", header_style)),
        Cell::from(Span::styled("Tokens", header_style)),
        Cell::from(Span::styled("Mem", header_style)),
        Cell::from(Span::styled("Turn", header_style)),
    ])
    .height(1);

    let widths = [
        Constraint::Length(1),   // marker
        Constraint::Length(3),   // agent label (*CC/>CD)
        Constraint::Length(6),   // pid
        Constraint::Length(14),  // project
        Constraint::Length(9),   // session id (8 chars + pad)
        Constraint::Min(10),     // summary (fills remaining space)
        Constraint::Length(6),   // status
        Constraint::Length(6),   // model
        Constraint::Length(7),   // context
        Constraint::Length(7),   // tokens
        Constraint::Length(5),   // mem
        Constraint::Length(4),   // turn
    ];

    let table = Table::new(rows, widths).header(header);
    f.render_widget(table, panel_chunks[0]);

    // ── Detail section for selected session (full-width Paragraph, not Table) ──
    if let Some(session) = app.sessions.get(app.selected) {
        let detail_area = panel_chunks[1];
        if detail_area.height < 3 {
            return;
        }

        // Reserve bottom lines for MEM + version
        let footer_h = 3u16;
        let detail_body_h = detail_area.height.saturating_sub(footer_h);
        let detail_body = Rect {
            x: detail_area.x,
            y: detail_area.y,
            width: detail_area.width,
            height: detail_body_h,
        };
        let detail_footer = Rect {
            x: detail_area.x,
            y: detail_area.y + detail_body_h,
            width: detail_area.width,
            height: footer_h.min(detail_area.height),
        };

        let has_children = !session.children.is_empty();
        let has_subagents = !session.subagents.is_empty();

        if has_children || has_subagents {
            let body_chunks = if has_children && has_subagents {
                Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
                    .split(detail_body)
            } else {
                Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([Constraint::Percentage(100)])
                    .split(detail_body)
            };

            // Children (left side)
            if has_children {
                let children_area = body_chunks[0];
                let mut lines = Vec::new();
                let max_hdr = (children_area.width as usize).saturating_sub(2);
                lines.push(Line::from(Span::styled(
                    truncate_str(
                        &format!(" CHILDREN (►{} · {})", session.pid, &session.project_name),
                        max_hdr,
                    ),
                    Style::default().fg(TITLE).add_modifier(Modifier::BOLD),
                )));
                for child in &session.children {
                    let cmd_short = child
                        .command
                        .split_whitespace()
                        .take(3)
                        .collect::<Vec<_>>()
                        .join(" ");
                    let port_str = child.port.map(|p| format!(" :{}", p)).unwrap_or_default();
                    let max_cmd = (children_area.width as usize).saturating_sub(18);
                    lines.push(Line::from(vec![
                        Span::styled(
                            format!(" {:<6}", child.pid),
                            Style::default().fg(MAIN_FG),
                        ),
                        Span::styled(
                            truncate_str(&cmd_short, max_cmd),
                            Style::default().fg(GRAPH_TEXT),
                        ),
                        Span::styled(
                            format!(" {:>5}", fmt_mem_kb(child.mem_kb)),
                            Style::default().fg(GRAPH_TEXT),
                        ),
                        Span::styled(port_str, Style::default().fg(PROC_MISC)),
                    ]));
                }
                f.render_widget(Paragraph::new(lines), children_area);
            }

            // Subagents (right side, or full width if no children)
            if has_subagents {
                let sa_area = if has_children {
                    body_chunks[1]
                } else {
                    body_chunks[0]
                };

                let mut lines = Vec::new();
                lines.push(Line::from(Span::styled(
                    " SUBAGENTS",
                    Style::default().fg(TITLE).add_modifier(Modifier::BOLD),
                )));

                let col_w = sa_area.width as usize;
                let use_two_cols = session.subagents.len() > 6 && col_w >= 50;

                if use_two_cols {
                    let half_w = col_w / 2;
                    let name_w = half_w.saturating_sub(12);
                    let mid = (session.subagents.len() + 1) / 2;
                    let left_agents = &session.subagents[..mid];
                    let right_agents = &session.subagents[mid..];

                    for row_idx in 0..mid {
                        let mut spans = Vec::new();
                        // Left column
                        let sa = &left_agents[row_idx];
                        let icon = if sa.status == "working" { "●" } else { "✓" };
                        let fg = if sa.status == "working" { MAIN_FG } else { GRAPH_TEXT };
                        spans.push(Span::styled(
                            format!("  {} {:<w$}", icon, truncate_str(&sa.name, name_w), w = name_w),
                            Style::default().fg(fg),
                        ));
                        spans.push(Span::styled(
                            format!("{:>6}", fmt_tokens(sa.tokens)),
                            Style::default().fg(GRAPH_TEXT),
                        ));

                        // Right column
                        if let Some(sa_r) = right_agents.get(row_idx) {
                            let icon_r = if sa_r.status == "working" { "●" } else { "✓" };
                            let fg_r = if sa_r.status == "working" { MAIN_FG } else { GRAPH_TEXT };
                            spans.push(Span::styled(
                                format!("  {} {:<w$}", icon_r, truncate_str(&sa_r.name, name_w), w = name_w),
                                Style::default().fg(fg_r),
                            ));
                            spans.push(Span::styled(
                                format!("{:>6}", fmt_tokens(sa_r.tokens)),
                                Style::default().fg(GRAPH_TEXT),
                            ));
                        }
                        lines.push(Line::from(spans));
                    }
                } else {
                    let name_w = col_w.saturating_sub(12);
                    for sa in &session.subagents {
                        let icon = if sa.status == "working" { "●" } else { "✓" };
                        let fg = if sa.status == "working" { MAIN_FG } else { GRAPH_TEXT };
                        lines.push(Line::from(vec![
                            Span::styled(
                                format!("  {} {:<w$}", icon, truncate_str(&sa.name, name_w), w = name_w),
                                Style::default().fg(fg),
                            ),
                            Span::styled(
                                format!("{:>6}", fmt_tokens(sa.tokens)),
                                Style::default().fg(GRAPH_TEXT),
                            ),
                        ]));
                    }
                }
                f.render_widget(Paragraph::new(lines), sa_area);
            }
        }

        // Footer: MEM + version (full width)
        {
            let cpu_grad = make_gradient(CPU_START, CPU_MID, CPU_END);
            let mem_color = if session.mem_line_count >= 180 {
                grad_at(&cpu_grad, 100.0)
            } else {
                GRAPH_TEXT
            };
            let footer_lines = vec![
                Line::from(""),
                Line::from(Span::styled(
                    format!(
                        " MEM {} files · {}/200 lines",
                        session.mem_file_count, session.mem_line_count
                    ),
                    Style::default().fg(mem_color),
                )),
                Line::from(Span::styled(
                    format!(
                        " {} · {} · {} turns",
                        session.version,
                        session.elapsed_display(),
                        session.turn_count
                    ),
                    Style::default().fg(INACTIVE_FG),
                )),
            ];
            f.render_widget(Paragraph::new(footer_lines), detail_footer);
        }
    }
}

// ── footer — btop style: ↑ select ↓ info ↵ terminate ── ─────────────────────

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    let has_tmux = std::env::var("TMUX").is_ok();
    let remaining = (area.width as usize).saturating_sub(50);

    let mut spans = vec![
        Span::styled(" ↑↓", Style::default().fg(HI_FG)),
        Span::styled(" select ", Style::default().fg(MAIN_FG)),
    ];
    if has_tmux {
        spans.push(Span::styled("↵", Style::default().fg(HI_FG)));
        spans.push(Span::styled(" jump ", Style::default().fg(MAIN_FG)));
    }
    spans.push(Span::styled("x", Style::default().fg(HI_FG)));
    spans.push(Span::styled(" kill ", Style::default().fg(MAIN_FG)));
    spans.push(Span::styled("q", Style::default().fg(HI_FG)));
    spans.push(Span::styled(" quit ", Style::default().fg(MAIN_FG)));
    spans.push(Span::styled("r", Style::default().fg(HI_FG)));
    spans.push(Span::styled(" force ", Style::default().fg(MAIN_FG)));
    spans.push(Span::styled("2s auto", Style::default().fg(INACTIVE_FG)));
    spans.push(Span::styled(
        format!("{:>width$}", format!("{} sessions", app.sessions.len()), width = remaining),
        Style::default().fg(GRAPH_TEXT),
    ));

    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

// ── utility functions ────────────────────────────────────────────────────────

fn shorten_model(model: &str) -> String {
    // "claude-opus-4-6" → "opus4.6", "claude-sonnet-4-6" → "sonnet4.6", "claude-haiku-4-5" → "haiku4.5"
    let s = model
        .strip_prefix("claude-")
        .unwrap_or(model);
    let s = s.trim_end_matches("[1m]");
    // Extract name and version: "opus-4-6" → ("opus", "4.6")
    if let Some(pos) = s.find(|c: char| c.is_ascii_digit()) {
        let name = s[..pos].trim_end_matches('-');
        let ver = s[pos..].replace('-', ".");
        format!("{}{}", name, ver)
    } else {
        s.to_string()
    }
}

fn fmt_mem_kb(kb: u64) -> String {
    if kb >= 1_048_576 {
        format!("{:.1}G", kb as f64 / 1_048_576.0)
    } else if kb >= 1024 {
        format!("{}M", kb / 1024)
    } else {
        format!("{}K", kb)
    }
}

fn fmt_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        format!("{}", n)
    }
}

fn truncate_str(s: &str, max: usize) -> String {
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
