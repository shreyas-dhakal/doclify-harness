mod app;
mod commands;
mod terminal;
mod ui;
pub mod workflow;

use std::process::ExitCode;

pub fn run() -> ExitCode {
    match app::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("doclify: terminal interface failed: {error}");
            ExitCode::FAILURE
        }
    }
}
