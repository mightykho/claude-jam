//! ratatui render functions: the main list, the delete-confirmation popup,
//! and the centered-rect helper that positions the popup over the list.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph};
use ratatui::Frame;
use rusqlite::Connection;

use claude_jam::db::{fetch_latest_milestone, fetch_milestones};
use claude_jam::models::Session;
use claude_jam::time::{relative_time, seconds_since, STALE_THRESHOLD_SECS};

use super::style::{
    format_context_bar, shortcut_label, status_color, status_emoji, truncate_chars,
};
use super::App;

/// Compute a `Rect` centered inside `area`, clamped so it never exceeds the
/// parent dimensions.
pub fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    }
}

/// Render the Y/N confirmation popup shown after `d` is pressed on a session.
pub fn render_delete_popup(frame: &mut Frame, area: Rect, session: &Session) {
    let tmux = session
        .tmux_session
        .as_deref()
        .filter(|t| !t.is_empty())
        .unwrap_or(&session.session_id);

    let topic = session.topic.as_deref().unwrap_or("");
    let inner_width: usize = 46;

    let target = truncate_chars(tmux, inner_width);
    let topic_line = if topic.is_empty() {
        None
    } else {
        Some(truncate_chars(topic, inner_width))
    };

    let height: u16 = if topic_line.is_some() { 8 } else { 7 };
    let popup = centered_rect(inner_width as u16 + 4, height, area);

    frame.render_widget(Clear, popup);

    let block = Block::default()
        .title(Span::styled(
            " Delete session? ",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Red));

    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::raw(" "),
        Span::styled(
            target,
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
    ]));
    if let Some(t) = topic_line {
        lines.push(Line::from(vec![
            Span::raw(" "),
            Span::styled(t, Style::default().fg(Color::DarkGray)),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::raw(" "),
        Span::styled(
            "[Y]",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("es", Style::default().fg(Color::Green)),
        Span::raw("   "),
        Span::styled(
            "[N]",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        Span::styled("o", Style::default().fg(Color::Red)),
        Span::raw("   "),
        Span::styled("Esc", Style::default().fg(Color::DarkGray)),
        Span::styled(" cancel", Style::default().fg(Color::DarkGray)),
    ]));

    frame.render_widget(Paragraph::new(lines), inner);
}

/// Top-level dashboard render. Lays out the session list, the footer, and the
/// delete popup overlay (when pending).
pub fn ui(frame: &mut Frame, app: &App, conn: &Connection) {
    let chunks = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).split(frame.area());

    let area = chunks[0];
    let width = if app.borderless {
        area.width as usize
    } else {
        area.width.saturating_sub(4) as usize
    };

    let items: Vec<ListItem> = app
        .sessions
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let is_stale = seconds_since(&s.updated_at) > STALE_THRESHOLD_SECS;
            let emoji = status_emoji(&s.status, is_stale);
            let color = status_color(&s.status, is_stale);
            let time = relative_time(&s.updated_at);
            let tmux = s
                .tmux_session
                .as_deref()
                .filter(|t| !t.is_empty())
                .unwrap_or("?")
                .to_string();

            let label = shortcut_label(i);
            let mut lines = vec![];

            // Title: shortcut + emoji + tmux name
            let title_spans: Vec<Span> = vec![
                Span::styled(format!("{} ", label), Style::default().fg(Color::Cyan)),
                Span::styled(format!("{} ", emoji), Style::default()),
                Span::styled(
                    tmux.trim().to_string(),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
            ];

            // Context bar (optional) — comes before time
            let ctx_bar = s
                .context_used
                .zip(s.context_total)
                .and_then(|(u, t)| format_context_bar(u, t));
            let bar_width = ctx_bar
                .as_ref()
                .map(|(b, _)| b.chars().count() + 2)
                .unwrap_or(0);

            // Detail: context + time + activity
            let mut detail_spans: Vec<Span> = vec![];
            if let Some((ref bar, bar_color)) = ctx_bar {
                detail_spans.push(Span::styled(bar.clone(), Style::default().fg(bar_color)));
                detail_spans.push(Span::raw("  "));
            }
            detail_spans.push(Span::styled(time.clone(), Style::default().fg(Color::Gray)));

            let activity = if s.status == "idle" {
                "Done".to_string()
            } else {
                let tool = s.tool_name.as_deref().unwrap_or("");
                let detail = s.detail.as_deref().unwrap_or("");
                if !tool.is_empty() && !detail.is_empty() {
                    format!("{} {}", tool, detail)
                } else if !tool.is_empty() {
                    tool.to_string()
                } else {
                    detail.to_string()
                }
            };
            if !activity.is_empty() {
                let max_activity = if app.vertical {
                    width.saturating_sub(2 + bar_width + time.chars().count() + 5)
                } else {
                    width.saturating_sub(
                        label.chars().count()
                            + 2
                            + emoji.chars().count()
                            + 1
                            + tmux.chars().count()
                            + 2
                            + bar_width
                            + time.chars().count()
                            + 5,
                    )
                };
                let activity_display = truncate_chars(&activity, max_activity);
                detail_spans.push(Span::styled(
                    format!(" — {}", activity_display),
                    Style::default().fg(Color::DarkGray),
                ));
            }

            if app.vertical {
                lines.push(Line::from(title_spans));
                let mut indented: Vec<Span> = vec![Span::raw("  ")];
                indented.extend(detail_spans);
                lines.push(Line::from(indented));
            } else {
                let mut combined = title_spans;
                combined.push(Span::raw("  "));
                combined.extend(detail_spans);
                lines.push(Line::from(combined));
            }

            // Topic line (if present)
            let latest_milestone = fetch_latest_milestone(conn, &s.session_id);

            if let Some(ref topic) = s.topic {
                let max_topic = width.saturating_sub(4);
                let topic_display = truncate_chars(topic, max_topic);
                lines.push(Line::from(vec![
                    Span::styled("  │ ", Style::default().fg(Color::Gray)),
                    Span::styled(
                        topic_display,
                        Style::default()
                            .fg(Color::DarkGray)
                            .add_modifier(Modifier::BOLD),
                    ),
                ]));
            }

            // Latest milestone
            if let Some(ref ms) = latest_milestone {
                let ms_time = relative_time(&ms.created_at);
                let prefix = if s.topic.is_some() {
                    "  ├ "
                } else {
                    "  │ "
                };
                let max_ms = width.saturating_sub(ms_time.chars().count() + 8);
                let ms_display = truncate_chars(&ms.summary, max_ms);
                lines.push(Line::from(vec![
                    Span::styled(prefix, Style::default().fg(Color::Gray)),
                    Span::styled("⚑ ", Style::default().fg(Color::Magenta)),
                    Span::styled(ms_display, Style::default().fg(Color::Gray)),
                    Span::styled(
                        format!("  {}", ms_time),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]));
            }

            // Expanded milestones (all except latest)
            if app.expanded == Some(i) {
                let all_milestones = fetch_milestones(conn, &s.session_id);
                for (mi, ms) in all_milestones.iter().enumerate().skip(1) {
                    let ms_time = relative_time(&ms.created_at);
                    let max_ms = width.saturating_sub(ms_time.chars().count() + 10);
                    let ms_display = truncate_chars(&ms.summary, max_ms);
                    let connector = if mi == all_milestones.len() - 1 {
                        "  └ "
                    } else {
                        "  ├ "
                    };
                    lines.push(Line::from(vec![
                        Span::styled(connector, Style::default().fg(Color::Gray)),
                        Span::styled("⚑ ", Style::default().fg(Color::DarkGray)),
                        Span::styled(ms_display, Style::default().fg(Color::DarkGray)),
                        Span::styled(
                            format!("  {}", ms_time),
                            Style::default().fg(Color::DarkGray),
                        ),
                    ]));
                }
            }

            // Blank line separator between sessions
            lines.push(Line::from(""));

            if i == app.selected {
                ListItem::new(lines).style(Style::default().bg(Color::Black).fg(Color::White))
            } else {
                ListItem::new(lines)
            }
        })
        .collect();

    let list = if app.borderless {
        List::new(items)
    } else {
        List::new(items).block(
            Block::default()
                .title(" Claude Jam ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        )
    };

    frame.render_widget(list, area);

    if app.sessions.is_empty() {
        let msg = Paragraph::new("No active sessions")
            .style(Style::default().fg(Color::DarkGray))
            .centered();
        let inner = area.inner(ratatui::layout::Margin {
            horizontal: 2,
            vertical: 2,
        });
        frame.render_widget(msg, inner);
    }

    // Footer
    let footer = Line::from(vec![
        Span::raw(" 1-9/C-a..z").bold(),
        Span::raw(" jump  "),
        Span::raw("j/k").bold(),
        Span::raw(" navigate  "),
        Span::raw("o").bold(),
        Span::raw(" expand  "),
        Span::raw("Enter").bold(),
        Span::raw(" switch  "),
        Span::raw("d").bold(),
        Span::raw(" delete  "),
        Span::raw("q").bold(),
        Span::raw(" quit  "),
    ])
    .style(Style::default().fg(Color::DarkGray));

    frame.render_widget(Paragraph::new(footer), chunks[1]);

    if let Some(idx) = app.pending_delete {
        if let Some(s) = app.sessions.get(idx) {
            render_delete_popup(frame, frame.area(), s);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn centered_rect_centers_within_parent() {
        let parent = Rect {
            x: 0,
            y: 0,
            width: 100,
            height: 40,
        };
        let r = centered_rect(40, 10, parent);
        assert_eq!(r.x, 30);
        assert_eq!(r.y, 15);
        assert_eq!(r.width, 40);
        assert_eq!(r.height, 10);
    }

    #[test]
    fn centered_rect_clamps_to_parent_size() {
        let parent = Rect {
            x: 0,
            y: 0,
            width: 20,
            height: 5,
        };
        let r = centered_rect(40, 10, parent);
        assert_eq!(r.width, 20);
        assert_eq!(r.height, 5);
    }
}
