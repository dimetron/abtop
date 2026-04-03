use crate::app::App;
use crate::theme::Theme;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::{btop_block, grad_at, make_gradient};

pub(crate) fn draw_ports_panel(f: &mut Frame, app: &App, area: Rect, theme: &Theme) {
    // Collect (port, project_name, session_id_short)
    let mut all_ports: Vec<(u16, String, String)> = Vec::new();
    for session in &app.sessions {
        let sid_short = if session.session_id.len() >= 8 {
            &session.session_id[..8]
        } else {
            &session.session_id
        };
        for child in &session.children {
            if let Some(port) = child.port {
                all_ports.push((
                    port,
                    session.project_name.clone(),
                    sid_short.to_string(),
                ));
            }
        }
    }
    all_ports.sort_by_key(|p| p.0);

    let mut port_counts: std::collections::HashMap<u16, usize> =
        std::collections::HashMap::new();
    for (port, _, _) in &all_ports {
        *port_counts.entry(*port).or_default() += 1;
    }

    let proc_grad = make_gradient(theme.proc_grad.start, theme.proc_grad.mid, theme.proc_grad.end);

    let header_style = Style::default().fg(theme.main_fg).add_modifier(Modifier::BOLD);
    let mut lines = vec![Line::from(vec![
        Span::styled(" PORT  ", header_style),
        Span::styled("SESSION", header_style),
    ])];
    for (port, proj, sid) in &all_ports {
        let conflict = port_counts.get(port).copied().unwrap_or(0) > 1;
        let color = if conflict {
            grad_at(&proc_grad, 100.0)
        } else {
            theme.proc_misc
        };
        let warn = if conflict { " ⚠" } else { "" };
        let session_label = format!("{} {}{}", proj, sid, warn);
        lines.push(Line::from(vec![
            Span::styled(format!(" :{:<5}", port), Style::default().fg(color)),
            Span::styled(session_label, Style::default().fg(theme.main_fg)),
        ]));
    }

    // Orphan ports: processes whose parent session has ended but port is still open
    let orphan_color = grad_at(&proc_grad, 100.0);
    for orphan in &app.orphan_ports {
        let session_label = format!("{} ⚠orphan", orphan.project_name);
        lines.push(Line::from(vec![
            Span::styled(format!(" :{:<5}", orphan.port), Style::default().fg(orphan_color)),
            Span::styled(session_label, Style::default().fg(orphan_color)),
        ]));
    }

    let has_orphans = !app.orphan_ports.is_empty();

    if lines.len() <= 1 {
        lines.push(Line::from(Span::styled(
            " no open ports",
            Style::default().fg(theme.inactive_fg),
        )));
    }

    if has_orphans {
        lines.push(Line::from(Span::styled(
            " X to kill orphans",
            Style::default().fg(theme.inactive_fg),
        )));
    }

    let block = btop_block("ports", "⁴", theme.net_box, theme);
    f.render_widget(Paragraph::new(lines).block(block), area);
}
