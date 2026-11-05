use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
    Frame,
};

use super::{
    app::App,
    workflow::{RegistrationState, WorkflowSnapshot},
};

const BG: Color = Color::Rgb(16, 20, 28);
const PANEL: Color = Color::Rgb(23, 29, 39);
const MUTED: Color = Color::Rgb(130, 145, 164);
const TEXT: Color = Color::Rgb(224, 231, 240);
const ACCENT: Color = Color::Rgb(103, 211, 173);
const BLUE: Color = Color::Rgb(117, 176, 255);

pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    frame.render_widget(Block::default().style(Style::default().bg(BG)), area);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Min(5),
            Constraint::Length(3),
            Constraint::Length(4),
        ])
        .split(area);
    draw_header(
        frame,
        rows[0],
        app.snapshot.as_ref(),
        app.loading,
        app.load_error.as_deref(),
    );
    draw_conversation(frame, rows[1], app);
    draw_flow(frame, rows[2], app.snapshot.as_ref());
    draw_input(
        frame,
        rows[3],
        &app.input,
        app.cursor,
        &app.status,
        app.busy,
    );
    if let Some(choices) = app.model_choices.as_ref() {
        draw_model_picker(frame, area, choices, app.model_selection);
    }
}

fn draw_header(
    frame: &mut Frame,
    area: Rect,
    snapshot: Option<&WorkflowSnapshot>,
    loading: bool,
    error: Option<&str>,
) {
    let (repo, branch, model, registered) = match snapshot {
        Some(snapshot) => (
            snapshot.repository.as_str(),
            snapshot.branch.as_deref().unwrap_or("branch unknown"),
            snapshot.model.as_deref().unwrap_or("no AI selected"),
            snapshot.registration == RegistrationState::Registered,
        ),
        None => (
            "project unavailable",
            "branch unknown",
            "no AI selected",
            false,
        ),
    };
    let state = if loading {
        "LOADING"
    } else if error.is_some() {
        "STATUS ERROR"
    } else if registered {
        "READY"
    } else {
        "SETUP NEEDED"
    };
    let state_color = if registered {
        ACCENT
    } else if error.is_some() {
        Color::LightRed
    } else {
        BLUE
    };
    let lines = vec![
        Line::from(vec![
            Span::styled(
                " doclify ",
                Style::default()
                    .fg(BG)
                    .bg(ACCENT)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("  ", Style::default()),
            Span::styled(repo, Style::default().fg(TEXT).add_modifier(Modifier::BOLD)),
            Span::styled(format!("  ·  {branch}"), Style::default().fg(MUTED)),
            Span::raw("  "),
            Span::styled(
                state,
                Style::default()
                    .fg(state_color)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled(" AI  ", Style::default().fg(MUTED)),
            Span::styled(model, Style::default().fg(BLUE)),
        ]),
    ];
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(Style::default().fg(Color::Rgb(47, 58, 74)))
                .style(Style::default().bg(PANEL))
                .padding(ratatui::widgets::Padding::new(1, 1, 0, 0)),
        ),
        area,
    );
}

fn draw_conversation(frame: &mut Frame, area: Rect, app: &mut App) {
    let inner_width = area.width.saturating_sub(6).max(1) as usize;
    let mut lines = Vec::new();
    let mut wrapped_height = 0usize;
    for message in &app.messages {
        let (color, label) = if message.role == "you" {
            (BLUE, "YOU")
        } else {
            (ACCENT, "DOCLIFY")
        };
        lines.push(Line::from(Span::styled(
            format!("{label}  "),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        )));
        wrapped_height += 1;
        for text_line in message.text.lines() {
            let line = Line::from(Span::styled(text_line, Style::default().fg(TEXT)));
            wrapped_height += line.width().max(1).div_ceil(inner_width);
            lines.push(line);
        }
        lines.push(Line::from(""));
        wrapped_height += 1;
    }
    let inner_height = area.height.saturating_sub(2) as usize;
    let max_scroll = wrapped_height.saturating_sub(inner_height);
    let scroll = max_scroll
        .saturating_sub(app.conversation_from_bottom)
        .min(u16::MAX as usize) as u16;
    let conversation = Paragraph::new(lines)
        .wrap(Wrap { trim: true })
        .scroll((scroll, 0))
        .block(
            Block::default()
                .title(Span::styled(
                    "  CONVERSATION  ",
                    Style::default().fg(MUTED).add_modifier(Modifier::BOLD),
                ))
                .borders(Borders::BOTTOM)
                .border_style(Style::default().fg(Color::Rgb(47, 58, 74)))
                .padding(ratatui::widgets::Padding::new(2, 2, 1, 0)),
        );
    frame.render_widget(conversation, area);
}

/// One workflow stage: pending work in its colour, settled stages muted, unknown counts as `?`.
fn stage(
    label: &'static str,
    value: Option<usize>,
    pending: &str,
    settled: &str,
    color: Color,
    compact: bool,
) -> Vec<Span<'static>> {
    let (mark, detail, style) = match value {
        None => ("?", "unknown".to_string(), Style::default().fg(MUTED)),
        Some(0) => ("✓", settled.to_string(), Style::default().fg(MUTED)),
        Some(number) => (
            "●",
            format!("{number} {pending}"),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
    };
    let detail = if compact {
        match value {
            Some(number) if number > 0 => number.to_string(),
            _ => mark.to_string(),
        }
    } else {
        format!("{mark} {detail}")
    };
    vec![
        Span::styled(
            format!("{label} "),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
        Span::styled(detail, style),
    ]
}

fn draw_flow(frame: &mut Frame, area: Rect, snapshot: Option<&WorkflowSnapshot>) {
    let compact = area.width < 96;
    let value = |field: fn(&WorkflowSnapshot) -> Option<usize>| snapshot.and_then(field);
    // Docs has no pending state: files present means ready, zero means nothing written yet.
    let docs = match (value(|snapshot| snapshot.documentation_files), compact) {
        (None, _) => Span::styled("?", Style::default().fg(MUTED)),
        (Some(0), true) => Span::styled("0", Style::default().fg(MUTED)),
        (Some(0), false) => Span::styled("none yet", Style::default().fg(MUTED)),
        (Some(number), true) => Span::styled(number.to_string(), Style::default().fg(TEXT)),
        (Some(number), false) => {
            Span::styled(format!("{number} files ready"), Style::default().fg(TEXT))
        }
    };
    let separator = || Span::styled(" → ", Style::default().fg(MUTED));
    let mut spans = stage(
        "Capture",
        value(|snapshot| snapshot.pending_capture_events),
        "to import",
        "clear",
        ACCENT,
        compact,
    );
    spans.push(separator());
    spans.extend(stage(
        "Propose",
        value(|snapshot| snapshot.pending_extraction),
        "to draft",
        "clear",
        BLUE,
        compact,
    ));
    spans.push(separator());
    spans.extend(stage(
        "Review",
        value(|snapshot| snapshot.pending_review),
        "waiting",
        "clear",
        Color::Yellow,
        compact,
    ));
    spans.push(separator());
    spans.push(Span::styled(
        "Docs ",
        Style::default()
            .fg(Color::Magenta)
            .add_modifier(Modifier::BOLD),
    ));
    spans.push(docs);
    frame.render_widget(
        Paragraph::new(Line::from(spans)).block(
            Block::default()
                .borders(Borders::TOP)
                .border_style(Style::default().fg(Color::Rgb(47, 58, 74)))
                .style(Style::default().bg(PANEL))
                .padding(ratatui::widgets::Padding::new(1, 1, 0, 0)),
        ),
        area,
    );
}

fn draw_input(frame: &mut Frame, area: Rect, input: &str, cursor: usize, status: &str, busy: bool) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Length(1)])
        .split(area);
    let cursor_byte = input
        .char_indices()
        .nth(cursor)
        .map(|(index, _)| index)
        .unwrap_or(input.len());
    let before = &input[..cursor_byte];
    let after = &input[cursor_byte..];
    let mut input_spans = vec![
        Span::styled(
            "› ",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::styled(before, Style::default().fg(TEXT)),
    ];
    if let Some(current) = after.chars().next() {
        input_spans.push(Span::styled(
            current.to_string(),
            Style::default()
                .fg(PANEL)
                .bg(ACCENT)
                .add_modifier(Modifier::BOLD),
        ));
        input_spans.push(Span::styled(
            &after[current.len_utf8()..],
            Style::default().fg(TEXT),
        ));
    } else {
        input_spans.push(Span::styled("▏", Style::default().fg(ACCENT)));
    }
    let prompt = Paragraph::new(Line::from(input_spans)).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(ACCENT))
            .style(Style::default().bg(PANEL))
            .title(Span::styled(
                "  CHAT  ",
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            )),
    );
    frame.render_widget(prompt, chunks[0]);
    let help = Paragraph::new(Line::from(vec![
        Span::styled(
            if busy { "Working…" } else { status },
            Style::default().fg(MUTED),
        ),
        Span::styled(
            " · Enter send · ↑↓ history · PgUp/Dn scroll · Ctrl-C exit · /help",
            Style::default().fg(Color::Rgb(103, 119, 141)),
        ),
    ]))
    .wrap(Wrap { trim: true });
    frame.render_widget(help, chunks[1]);
}

fn draw_model_picker(
    frame: &mut Frame,
    area: Rect,
    choices: &[super::commands::ModelChoice],
    selected: usize,
) {
    let popup = centered_rect(68, area);
    frame.render_widget(Clear, popup);
    let items = choices
        .iter()
        .map(|choice| {
            ListItem::new(vec![
                Line::from(Span::styled(
                    choice.id.as_str(),
                    Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
                )),
                Line::from(Span::styled(
                    choice.status.as_str(),
                    Style::default().fg(MUTED),
                )),
            ])
        })
        .collect::<Vec<_>>();
    let list = List::new(items)
        .highlight_symbol("› ")
        .highlight_style(Style::default().bg(Color::Rgb(34, 46, 62)).fg(BLUE))
        .block(
            Block::default()
                .title(" Choose an AI · ↑/↓ move · Enter select · Esc cancel ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(ACCENT))
                .style(Style::default().bg(PANEL))
                .padding(ratatui::widgets::Padding::new(1, 1, 1, 1)),
        );
    let mut state = ListState::default();
    state.select(Some(selected));
    frame.render_stateful_widget(list, popup, &mut state);
}

fn centered_rect(percent: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent) / 2),
            Constraint::Percentage(percent),
            Constraint::Percentage((100 - percent) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent) / 2),
            Constraint::Percentage(percent),
            Constraint::Percentage((100 - percent) / 2),
        ])
        .split(vertical[1])[1]
}
