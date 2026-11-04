use std::{
    io,
    sync::mpsc::{self, Receiver},
    thread,
    time::{Duration, Instant},
};

use super::{
    commands::{self, ActionResult, ModelChoice},
    terminal::Session,
    ui,
    workflow::{self, WorkflowSnapshot},
};
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use doclify_capture::RepoInfo;

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
    pub cursor: usize,
    pub history: Vec<String>,
    pub history_position: Option<usize>,
    pub conversation_from_bottom: usize,
    pub status: String,
    pub busy: bool,
    pub model_choices: Option<Vec<ModelChoice>>,
    pub model_selection: usize,
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
                text: "Tell me what the docs should explain. Type /help to see commands.".into(),
            }],
            input: String::new(),
            cursor: 0,
            history: Vec::new(),
            history_position: None,
            conversation_from_bottom: 0,
            status: "Loading…".into(),
            busy: false,
            model_choices: None,
            model_selection: 0,
            should_quit: false,
        }
    }

    fn submit(&mut self) -> Option<Receiver<ActionResult>> {
        if self.busy {
            return None;
        }
        let text = self.input.trim().to_owned();
        if text.is_empty() {
            return None;
        }
        self.messages.push(Message {
            role: "you",
            text: text.clone(),
        });
        self.history.push(text.clone());
        self.history_position = None;
        self.input.clear();
        self.cursor = 0;
        self.busy = true;
        self.status = "Working…".into();
        self.conversation_from_bottom = 0;
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let _ = sender.send(commands::execute(&text));
        });
        Some(receiver)
    }

    fn choose_model(&mut self) -> Option<Receiver<ActionResult>> {
        if self.busy {
            return None;
        }
        let choice = self
            .model_choices
            .as_ref()?
            .get(self.model_selection)?
            .id
            .clone();
        self.model_choices = None;
        self.busy = true;
        self.status = format!("Selecting {choice}…");
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let result = commands::choose_model(&choice)
                .map(ActionResult::Message)
                .unwrap_or_else(|error| {
                    ActionResult::Message(format!("Couldn't choose that AI: {error}"))
                });
            let _ = sender.send(result);
        });
        Some(receiver)
    }

    fn insert(&mut self, character: char) {
        let byte = byte_offset(&self.input, self.cursor);
        self.input.insert(byte, character);
        self.cursor += 1;
    }

    fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let end = byte_offset(&self.input, self.cursor);
        let start = self.input[..end]
            .char_indices()
            .last()
            .map(|(index, _)| index)
            .unwrap_or(end);
        self.input.replace_range(start..end, "");
        self.cursor -= 1;
    }

    fn delete(&mut self) {
        let start = byte_offset(&self.input, self.cursor);
        let Some(character) = self.input[start..].chars().next() else {
            return;
        };
        self.input
            .replace_range(start..start + character.len_utf8(), "");
    }

    fn move_cursor(&mut self, right: bool) {
        let length = self.input.chars().count();
        if right {
            self.cursor = (self.cursor + 1).min(length);
        } else {
            self.cursor = self.cursor.saturating_sub(1);
        }
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
        self.cursor = self.input.chars().count();
    }
}

fn byte_offset(text: &str, character_index: usize) -> usize {
    text.char_indices()
        .nth(character_index)
        .map(|(index, _)| index)
        .unwrap_or(text.len())
}

type Refresh = Result<WorkflowSnapshot, String>;

fn refresh() -> Receiver<Refresh> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let result = std::env::current_dir()
            .map_err(|error| error.to_string())
            .and_then(|cwd| {
                RepoInfo::discover(&cwd)
                    .ok_or_else(|| "Open Doclify from inside a Git project.".to_string())
            })
            .and_then(|repo| workflow::load(&repo).map_err(|error| error.to_string()));
        let _ = sender.send(result);
    });
    receiver
}

pub fn run() -> io::Result<()> {
    let mut session = Session::enter()?;
    let mut app = App::new();
    let mut refresh_receiver = Some(refresh());
    let mut action_receiver: Option<Receiver<ActionResult>> = None;
    let mut last_refresh = Instant::now();
    loop {
        if let Some(receiver) = refresh_receiver.as_ref() {
            match receiver.try_recv() {
                Ok(result) => {
                    refresh_receiver = None;
                    app.loading = false;
                    match result {
                        Ok(snapshot) => {
                            let first_load = app.snapshot.is_none();
                            app.status = if snapshot.background_error.is_some()
                                || snapshot.recovery_error.is_some()
                            {
                                "Needs attention".into()
                            } else {
                                "Ready".into()
                            };
                            if first_load && snapshot.background_error.is_some() {
                                app.messages.push(Message {
                                    role: "doclify",
                                    text: "Doclify couldn't finish processing saved sessions. They're still saved; run `/sync` to try again.".into(),
                                });
                            }
                            if first_load && snapshot.recovery_error.is_some() {
                                app.messages.push(Message {
                                    role: "doclify",
                                    text: "Automatic session recovery isn't running. Run `doclify doctor` to see how to fix it.".into(),
                                });
                            }
                            if first_load && snapshot.model.is_none() {
                                app.messages.push(Message {
                                    role: "doclify",
                                    text: "Choose an AI with /model, then describe what the docs should explain. Doclify will draft suggestions for you to review before adding them.".into(),
                                });
                            }
                            if first_load
                                && snapshot.registration
                                    == workflow::RegistrationState::NotRegistered
                            {
                                app.messages.push(Message {
                                    role: "doclify",
                                    text: "Session capture isn't set up in this folder yet. Run `doclify init` here to save your coding sessions.".into(),
                                });
                            }
                            app.snapshot = Some(snapshot);
                        }
                        Err(_error) => {
                            if app.load_error.is_none() {
                                app.messages.push(Message {
                                    role: "doclify",
                                    text: "Couldn't load this project's status. Run `doclify doctor` to see what needs fixing.".into(),
                                });
                            }
                            app.load_error = Some("Can't load status".into());
                            app.status = "Can't load status".into();
                        }
                    }
                    last_refresh = Instant::now();
                }
                Err(mpsc::TryRecvError::Disconnected) => refresh_receiver = None,
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }

        if let Some(receiver) = action_receiver.as_ref() {
            match receiver.try_recv() {
                Ok(result) => {
                    action_receiver = None;
                    app.busy = false;
                    match result {
                        ActionResult::Message(text) => {
                            app.status = if text.starts_with("Couldn't") {
                                "Error".into()
                            } else {
                                "Ready".into()
                            };
                            app.messages.push(Message {
                                role: "doclify",
                                text,
                            });
                            app.conversation_from_bottom = 0;
                        }
                        ActionResult::ModelChoices(choices) => {
                            if choices.is_empty() {
                                app.messages.push(Message {
                                    role: "doclify",
                                    text: "No AI choices found. Check your setup with `doclify doctor`.".into(),
                                });
                            } else {
                                app.model_selection = 0;
                                app.model_choices = Some(choices);
                                app.status = "Choose AI".into();
                            }
                        }
                        ActionResult::Quit => app.should_quit = true,
                    }
                    refresh_receiver = Some(refresh());
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    action_receiver = None;
                    app.busy = false;
                    app.status = "Try again".into();
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }

        if refresh_receiver.is_none()
            && action_receiver.is_none()
            && last_refresh.elapsed() >= Duration::from_secs(2)
        {
            refresh_receiver = Some(refresh());
        }

        session.terminal().draw(|frame| ui::draw(frame, &mut app))?;
        if event::poll(Duration::from_millis(100))? {
            match event::read()? {
                Event::Key(key) if key.kind == event::KeyEventKind::Press => {
                    if let Some(choices) = app.model_choices.as_ref() {
                        match key.code {
                            KeyCode::Up => {
                                app.model_selection = app.model_selection.saturating_sub(1);
                            }
                            KeyCode::Down => {
                                app.model_selection =
                                    (app.model_selection + 1).min(choices.len() - 1);
                            }
                            KeyCode::Enter => action_receiver = app.choose_model(),
                            KeyCode::Esc => {
                                app.model_choices = None;
                                app.status = "Ready".into();
                            }
                            _ => {}
                        }
                    } else {
                        match (key.code, key.modifiers) {
                            (KeyCode::Char('c'), KeyModifiers::CONTROL) => app.should_quit = true,
                            (KeyCode::Enter, _) => action_receiver = app.submit(),
                            (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                                app.insert(c)
                            }
                            (KeyCode::Backspace, _) => app.backspace(),
                            (KeyCode::Delete, _) => app.delete(),
                            (KeyCode::Left, _) => app.move_cursor(false),
                            (KeyCode::Right, _) => app.move_cursor(true),
                            (KeyCode::Home, _) => app.cursor = 0,
                            (KeyCode::End, _) => app.cursor = app.input.chars().count(),
                            (KeyCode::Up, _) => app.move_history(true),
                            (KeyCode::Down, _) => app.move_history(false),
                            (KeyCode::PageUp, _) => {
                                app.conversation_from_bottom += 6;
                            }
                            (KeyCode::PageDown, _) => {
                                app.conversation_from_bottom =
                                    app.conversation_from_bottom.saturating_sub(6);
                            }
                            (KeyCode::Esc, _) if !app.busy => app.should_quit = true,
                            _ => {}
                        }
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
