use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph, Wrap},
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
    draw_input(frame, rows[3], &app.input, &app.status);
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
            snapshot.model.as_deref().unwrap_or("model not configured"),
            snapshot.registration == RegistrationState::Registered,
        ),
        None => (
            "repository unavailable",
            "branch unknown",
            "model not configured",
            false,
        ),
    };
    let state = if loading {
        "SYNCING"
    } else if error.is_some() {
        "OFFLINE"
    } else if registered {
        "CONNECTED"
    } else {
        "NOT INITIALIZED"
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
            Span::styled(" MODEL  ", Style::default().fg(MUTED)),
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
    let items: Vec<ListItem> = app
        .messages
        .iter()
        .map(|message| {
            let (color, label) = if message.role == "you" {
                (BLUE, "YOU")
            } else {
                (ACCENT, "DOC")
            };
            ListItem::new(vec![
                Line::from(Span::styled(
                    format!("{label}  "),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                )),
                Line::from(Span::styled(
                    message.text.as_str(),
                    Style::default().fg(TEXT),
                )),
                Line::from(""),
            ])
        })
        .collect();
    let list = List::new(items)
        .highlight_style(Style::default().bg(Color::Rgb(28, 36, 48)))
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
    if app.conversation.selected().is_none() {
        app.conversation
            .select(Some(app.messages.len().saturating_sub(1)));
    }
    frame.render_stateful_widget(list, area, &mut app.conversation);
}

fn count(value: Option<usize>) -> String {
    value
        .map(|number| number.to_string())
        .unwrap_or_else(|| "?".into())
}

fn draw_flow(frame: &mut Frame, area: Rect, snapshot: Option<&WorkflowSnapshot>) {
    let (capture, propose, review, docs) = snapshot
        .map(|snapshot| {
            (
                count(snapshot.pending_capture_events),
                count(snapshot.pending_extraction),
                count(snapshot.pending_review),
                count(snapshot.documentation_files),
            )
        })
        .unwrap_or_else(|| ("?".into(), "?".into(), "?".into(), "?".into()));
    let flow = Line::from(vec![
        Span::styled(
            format!(" CAPTURE  {capture} "),
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::styled("›", Style::default().fg(MUTED)),
        Span::styled(
            format!(" PROPOSE  {propose} "),
            Style::default().fg(BLUE).add_modifier(Modifier::BOLD),
        ),
        Span::styled("›", Style::default().fg(MUTED)),
        Span::styled(
            format!(" REVIEW  {review} "),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("›", Style::default().fg(MUTED)),
        Span::styled(
            format!(" DOCS  {docs} "),
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ),
    ]);
    frame.render_widget(
        Paragraph::new(flow).block(
            Block::default()
                .borders(Borders::TOP)
                .border_style(Style::default().fg(Color::Rgb(47, 58, 74)))
                .style(Style::default().bg(PANEL))
                .padding(ratatui::widgets::Padding::new(1, 1, 0, 0)),
        ),
        area,
    );
}

fn draw_input(frame: &mut Frame, area: Rect, input: &str, status: &str) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Length(1)])
        .split(area);
    let prompt = Paragraph::new(Line::from(vec![
        Span::styled(
            "› ",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::styled(input, Style::default().fg(TEXT)),
        Span::styled("▏", Style::default().fg(ACCENT)),
    ]))
    .block(
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
        Span::styled(status, Style::default().fg(MUTED)),
        Span::styled(
            "   ·   Enter send   ↑↓ history   PgUp/PgDn scroll   Ctrl-C quit   /help",
            Style::default().fg(Color::Rgb(103, 119, 141)),
        ),
    ]))
    .wrap(Wrap { trim: true });
    frame.render_widget(help, chunks[1]);
}
