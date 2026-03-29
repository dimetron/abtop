use crate::app::App;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Gauge, Paragraph, Row, Table};
use ratatui::Frame;

pub fn draw(f: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),  // top: rate limit + context
            Constraint::Min(10),   // middle + bottom
            Constraint::Length(1), // footer
        ])
        .split(f.area());

    draw_top_panel(f, app, chunks[0]);

    let mid = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(35), // left panels
            Constraint::Percentage(65), // sessions
        ])
        .split(chunks[1]);

    let left = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(40), // tokens
            Constraint::Percentage(30), // projects
            Constraint::Percentage(30), // ports
        ])
        .split(mid[0]);

    draw_tokens_panel(f, app, left[0]);
    draw_projects_panel(f, app, left[1]);
    draw_ports_panel(f, app, left[2]);
    draw_sessions_panel(f, app, mid[1]);
    draw_footer(f, app, chunks[2]);
}

fn draw_top_panel(f: &mut Frame, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(area);

    // Rate limit (left)
    let rl_text = vec![
        Line::from(vec![
            Span::styled("5h ", Style::default().fg(Color::DarkGray)),
            Span::styled("—  unavailable", Style::default().fg(Color::DarkGray)),
        ]),
        Line::from(vec![
            Span::styled("7d ", Style::default().fg(Color::DarkGray)),
            Span::styled("—  unavailable", Style::default().fg(Color::DarkGray)),
        ]),
    ];
    let rl_block = Block::default()
        .title(" rate limit ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));
    let rl_para = Paragraph::new(rl_text).block(rl_block);
    f.render_widget(rl_para, chunks[0]);

    // Context bars (right)
    let mut lines = Vec::new();
    for session in &app.sessions {
        let pct = session.context_percent.min(100.0);
        let bar_width = 20;
        let filled = (pct / 100.0 * bar_width as f64) as usize;
        let empty = bar_width - filled;

        let color = if pct >= 90.0 {
            Color::Red
        } else if pct >= 80.0 {
            Color::Yellow
        } else {
            Color::Green
        };

        let warn = if pct >= 90.0 { " ⚠" } else { "" };

        lines.push(Line::from(vec![
            Span::styled(
                format!("{:<12}", truncate_str(&session.project_name, 12)),
                Style::default().fg(Color::Yellow),
            ),
            Span::styled("█".repeat(filled), Style::default().fg(color)),
            Span::styled("░".repeat(empty), Style::default().fg(Color::DarkGray)),
            Span::styled(format!(" {:>3.0}%{}", pct, warn), Style::default().fg(color)),
        ]));
    }
    let ctx_block = Block::default()
        .title(" context ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));
    let ctx_para = Paragraph::new(lines).block(ctx_block);
    f.render_widget(ctx_para, chunks[1]);
}

fn draw_tokens_panel(f: &mut Frame, app: &App, area: Rect) {
    let total_in: u64 = app.sessions.iter().map(|s| s.total_input_tokens).sum();
    let total_out: u64 = app.sessions.iter().map(|s| s.total_output_tokens).sum();
    let total_cache: u64 = app.sessions.iter().map(|s| s.total_cache_read + s.total_cache_create).sum();
    let total: u64 = total_in + total_out + total_cache;
    let turns: u32 = app.sessions.iter().map(|s| s.turn_count).sum();

    let lines = vec![
        Line::from(vec![
            Span::styled("Total  ", Style::default().fg(Color::DarkGray)),
            Span::styled(fmt_tokens(total), Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
        ]),
        Line::from(vec![
            Span::styled("Input  ", Style::default().fg(Color::DarkGray)),
            Span::styled(fmt_tokens(total_in), Style::default().fg(Color::Green)),
        ]),
        Line::from(vec![
            Span::styled("Output ", Style::default().fg(Color::DarkGray)),
            Span::styled(fmt_tokens(total_out), Style::default().fg(Color::Cyan)),
        ]),
        Line::from(vec![
            Span::styled("Cache  ", Style::default().fg(Color::DarkGray)),
            Span::styled(fmt_tokens(total_cache), Style::default().fg(Color::Magenta)),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("Turns  ", Style::default().fg(Color::DarkGray)),
            Span::styled(format!("{}", turns), Style::default().fg(Color::White)),
        ]),
    ];

    let block = Block::default()
        .title(" tokens ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Green));
    let para = Paragraph::new(lines).block(block);
    f.render_widget(para, area);
}

fn draw_projects_panel(f: &mut Frame, app: &App, area: Rect) {
    let mut lines = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for session in &app.sessions {
        if !seen.insert(&session.project_name) {
            continue;
        }
        lines.push(Line::from(vec![
            Span::styled(
                format!("{:<14}", truncate_str(&session.project_name, 14)),
                Style::default().fg(Color::Yellow),
            ),
        ]));
        let branch = if session.git_branch.is_empty() {
            "?".to_string()
        } else {
            session.git_branch.clone()
        };
        lines.push(Line::from(vec![
            Span::styled(format!("  {}", branch), Style::default().fg(Color::DarkGray)),
        ]));
    }

    let block = Block::default()
        .title(" projects ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow));
    let para = Paragraph::new(lines).block(block);
    f.render_widget(para, area);
}

fn draw_ports_panel(f: &mut Frame, app: &App, area: Rect) {
    let mut all_ports: Vec<(u16, String, String, u32)> = Vec::new();
    for session in &app.sessions {
        for child in &session.children {
            if let Some(port) = child.port {
                let cmd = child.command.split_whitespace().next().unwrap_or("?");
                let cmd = cmd.rsplit('/').next().unwrap_or(cmd);
                all_ports.push((port, session.project_name.clone(), cmd.to_string(), child.pid));
            }
        }
    }
    all_ports.sort_by_key(|p| p.0);

    // Detect conflicts
    let mut port_counts: std::collections::HashMap<u16, usize> = std::collections::HashMap::new();
    for (port, _, _, _) in &all_ports {
        *port_counts.entry(*port).or_default() += 1;
    }

    let mut lines = Vec::new();
    for (port, proj, cmd, pid) in &all_ports {
        let conflict = port_counts.get(port).copied().unwrap_or(0) > 1;
        let color = if conflict { Color::Red } else { Color::Green };
        let warn = if conflict { " ⚠" } else { "" };
        lines.push(Line::from(vec![
            Span::styled(format!(":{:<5}", port), Style::default().fg(color)),
            Span::styled(format!("{:<10}", truncate_str(proj, 10)), Style::default().fg(Color::Yellow)),
            Span::styled(format!("{:<8}", truncate_str(cmd, 8)), Style::default().fg(Color::DarkGray)),
            Span::styled(format!("{}{}", pid, warn), Style::default().fg(color)),
        ]));
    }

    if lines.is_empty() {
        lines.push(Line::from(Span::styled("  no open ports", Style::default().fg(Color::DarkGray))));
    }

    let block = Block::default()
        .title(" ports ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Blue));
    let para = Paragraph::new(lines).block(block);
    f.render_widget(para, area);
}

fn draw_sessions_panel(f: &mut Frame, app: &App, area: Rect) {
    let mut rows = Vec::new();
    for (i, session) in app.sessions.iter().enumerate() {
        let selected = i == app.selected;
        let marker = if selected { "►" } else { " " };

        let status = match &session.status {
            crate::model::SessionStatus::Working => Span::styled("● Work", Style::default().fg(Color::Green)),
            crate::model::SessionStatus::Waiting => Span::styled("◌ Wait", Style::default().fg(Color::Yellow)),
            crate::model::SessionStatus::Error(_) => Span::styled("✗ Err", Style::default().fg(Color::Red)),
            crate::model::SessionStatus::Done => Span::styled("✓ Done", Style::default().fg(Color::DarkGray)),
        };

        let model_short = session.model.replace("claude-", "").replace("-4-6", "").replace("-4-5", "");

        let ctx_color = if session.context_percent >= 90.0 {
            Color::Red
        } else if session.context_percent >= 80.0 {
            Color::Yellow
        } else {
            Color::Green
        };

        let row_style = if selected {
            Style::default().add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };

        rows.push(Row::new(vec![
            Cell::from(marker),
            Cell::from(format!("{}", session.pid)),
            Cell::from(truncate_str(&session.project_name, 14)),
            Cell::from(status),
            Cell::from(truncate_str(&model_short, 8)),
            Cell::from(Span::styled(
                format!("{:>3.0}%", session.context_percent),
                Style::default().fg(ctx_color),
            )),
            Cell::from(fmt_tokens(session.total_tokens())),
            Cell::from(if session.mem_mb > 0 { format!("{}M", session.mem_mb) } else { "—".to_string() }),
            Cell::from(format!("{}", session.turn_count)),
        ]).style(row_style).height(1));

        // 2nd line: current task
        let task_prefix = match &session.status {
            crate::model::SessionStatus::Working => "└─ ",
            crate::model::SessionStatus::Waiting => "└─ ",
            crate::model::SessionStatus::Error(e) => "└─ ",
            crate::model::SessionStatus::Done => "└─ ",
        };
        rows.push(Row::new(vec![
            Cell::from(""),
            Cell::from(""),
            Cell::from(Span::styled(
                format!("{}{}", task_prefix, truncate_str(&session.current_task, 50)),
                Style::default().fg(Color::DarkGray),
            )),
            Cell::from(""),
            Cell::from(""),
            Cell::from(""),
            Cell::from(""),
            Cell::from(""),
            Cell::from(""),
        ]).height(1));
    }

    // Selected session detail: children + subagents
    if let Some(session) = app.sessions.get(app.selected) {
        if !session.children.is_empty() {
            rows.push(Row::new(vec![Cell::from(""); 9]).height(1));
            rows.push(Row::new(vec![
                Cell::from(""),
                Cell::from(Span::styled(
                    format!("CHILDREN ({})", session.project_name),
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                )),
                Cell::from(""),
                Cell::from(""),
                Cell::from(""),
                Cell::from(""),
                Cell::from(""),
                Cell::from(""),
                Cell::from(""),
            ]).height(1));

            for child in &session.children {
                let cmd_short = child.command.split_whitespace().take(3).collect::<Vec<_>>().join(" ");
                let port_str = child.port.map(|p| format!(":{}", p)).unwrap_or_default();
                rows.push(Row::new(vec![
                    Cell::from(""),
                    Cell::from(format!("{}", child.pid)),
                    Cell::from(truncate_str(&cmd_short, 30)),
                    Cell::from(""),
                    Cell::from(""),
                    Cell::from(""),
                    Cell::from(format!("{}K", child.mem_kb / 1024)),
                    Cell::from(Span::styled(port_str, Style::default().fg(Color::Green))),
                    Cell::from(""),
                ]).height(1));
            }
        }

        // Session info
        rows.push(Row::new(vec![Cell::from(""); 9]).height(1));
        rows.push(Row::new(vec![
            Cell::from(""),
            Cell::from(Span::styled(
                format!("{} · {} · {} turns", session.version, session.elapsed_display(), session.turn_count),
                Style::default().fg(Color::DarkGray),
            )),
            Cell::from(""),
            Cell::from(""),
            Cell::from(""),
            Cell::from(""),
            Cell::from(""),
            Cell::from(""),
            Cell::from(""),
        ]).height(1));
    }

    let header = Row::new(vec![
        Cell::from(""),
        Cell::from(Span::styled("Pid", Style::default().fg(Color::DarkGray))),
        Cell::from(Span::styled("Project", Style::default().fg(Color::DarkGray))),
        Cell::from(Span::styled("Status", Style::default().fg(Color::DarkGray))),
        Cell::from(Span::styled("Model", Style::default().fg(Color::DarkGray))),
        Cell::from(Span::styled("CTX", Style::default().fg(Color::DarkGray))),
        Cell::from(Span::styled("Tokens", Style::default().fg(Color::DarkGray))),
        Cell::from(Span::styled("Mem", Style::default().fg(Color::DarkGray))),
        Cell::from(Span::styled("Turn", Style::default().fg(Color::DarkGray))),
    ]).height(1);

    let widths = [
        Constraint::Length(1),
        Constraint::Length(6),
        Constraint::Min(14),
        Constraint::Length(6),
        Constraint::Length(8),
        Constraint::Length(5),
        Constraint::Length(7),
        Constraint::Length(5),
        Constraint::Length(4),
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .block(
            Block::default()
                .title(" sessions ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        );

    f.render_widget(table, area);
}

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    let has_tmux = std::env::var("TMUX").is_ok();
    let enter_hint = if has_tmux { "Enter:jump" } else { "" };
    let line = Line::from(vec![
        Span::styled(" ↑↓", Style::default().fg(Color::Yellow)),
        Span::styled(":select  ", Style::default().fg(Color::DarkGray)),
        Span::styled(enter_hint, Style::default().fg(Color::DarkGray)),
        Span::styled("  q", Style::default().fg(Color::Yellow)),
        Span::styled(":quit  ", Style::default().fg(Color::DarkGray)),
        Span::styled("r", Style::default().fg(Color::Yellow)),
        Span::styled(":refresh", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("  {} sessions", app.sessions.len()),
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    f.render_widget(Paragraph::new(line), area);
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
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max - 1])
    }
}
