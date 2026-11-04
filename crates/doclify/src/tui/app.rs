use std::{
    io,
    sync::mpsc::{self, Receiver},
    thread,
    time::Duration,
};

use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use doclify_capture::RepoInfo;
use ratatui::widgets::ListState;

use super::{
    terminal::Session,
    ui,
    workflow::{self, WorkflowSnapshot},
};

#[derive(Clone)]
pub struct Message {
    pub role: &'static str,
    pub text: String,
}

pub struct App {
    pub snapshot: Option<WorkflowSnapshot>,
    pub load_error: Option<String>,
    pub loading: bool,
    pub messages: Vec<Message>,
    pub input: String,
    pub history: Vec<String>,
    pub history_position: Option<usize>,
    pub conversation: ListState,
    pub status: String,
    pub should_quit: bool,
}

impl App {
    fn new() -> Self {
        Self {
            snapshot: None,
            load_error: None,
            loading: true,
            messages: vec![Message {
                role: "doclify",
                text: "Ready when you are. Ask a question or enter a slash command.".into(),
            }],
            input: String::new(),
            history: Vec::new(),
            history_position: None,
            conversation: ListState::default(),
            status: "Loading repository workflow…".into(),
            should_quit: false,
        }
    }

    fn submit(&mut self) {
        let text = self.input.trim().to_owned();
        if text.is_empty() {
            return;
        }
        self.messages.push(Message {
            role: "you",
            text: text.clone(),
        });
        self.history.push(text);
        self.history_position = None;
        self.input.clear();
        self.messages.push(Message {
            role: "doclify",
            text: "This action is a placeholder for the next integration stage.".into(),
        });
        self.status = "Message received · integration is not connected yet".into();
        self.conversation
            .select(Some(self.messages.len().saturating_sub(1)));
    }

    fn move_history(&mut self, up: bool) {
        if self.history.is_empty() {
            return;
        }
        let next = match (self.history_position, up) {
            (None, true) => self.history.len().checked_sub(1),
            (Some(0), true) => Some(0),
            (Some(index), true) => Some(index - 1),
            (None, false) => None,
            (Some(index), false) if index + 1 >= self.history.len() => None,
            (Some(index), false) => Some(index + 1),
        };
        self.history_position = next;
        self.input = next
            .map(|index| self.history[index].clone())
            .unwrap_or_default();
    }
}

type Refresh = Result<WorkflowSnapshot, String>;

fn refresh() -> Receiver<Refresh> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let result = std::env::current_dir()
            .map_err(|error| error.to_string())
            .and_then(|cwd| {
                RepoInfo::discover(&cwd).ok_or_else(|| "Not inside a Git repository".to_string())
            })
            .and_then(|repo| workflow::load(&repo).map_err(|error| error.to_string()));
        let _ = sender.send(result);
    });
    receiver
}

pub fn run() -> io::Result<()> {
    let mut session = Session::enter()?;
    let mut app = App::new();
    let refresh = refresh();
    loop {
        if let Ok(result) = refresh.try_recv() {
            app.loading = false;
            match result {
                Ok(snapshot) => {
                    app.status = snapshot
                        .background_error
                        .as_deref()
                        .or(snapshot.recovery_error.as_deref())
                        .map(|error| format!("Workflow warning · {error}"))
                        .unwrap_or_else(|| {
                            "Workflow loaded · commands are ready for integration".into()
                        });
                    app.snapshot = Some(snapshot);
                }
                Err(error) => {
                    app.load_error = Some(error.clone());
                    app.status = format!("Workflow unavailable · {error}");
                }
            }
        }
        session.terminal().draw(|frame| ui::draw(frame, &mut app))?;
        if event::poll(Duration::from_millis(100))? {
            match event::read()? {
                Event::Key(key) if key.kind == event::KeyEventKind::Press => {
                    match (key.code, key.modifiers) {
                        (KeyCode::Char('c'), KeyModifiers::CONTROL) => app.should_quit = true,
                        (KeyCode::Enter, _) => app.submit(),
                        (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                            app.input.push(c)
                        }
                        (KeyCode::Backspace, _) => {
                            app.input.pop();
                        }
                        (KeyCode::Up, _) => app.move_history(true),
                        (KeyCode::Down, _) => app.move_history(false),
                        (KeyCode::PageUp, _) => {
                            let index = app
                                .conversation
                                .selected()
                                .unwrap_or(app.messages.len().saturating_sub(1));
                            app.conversation.select(Some(index.saturating_sub(6)));
                        }
                        (KeyCode::PageDown, _) => {
                            let index = app.conversation.selected().unwrap_or(0);
                            app.conversation.select(Some(
                                (index + 6).min(app.messages.len().saturating_sub(1)),
                            ));
                        }
                        (KeyCode::Esc, _) => app.should_quit = true,
                        _ => {}
                    }
                }
                Event::Resize(_, _) => {}
                _ => {}
            }
        }
        if app.should_quit {
            break;
        }
    }
    Ok(())
}
