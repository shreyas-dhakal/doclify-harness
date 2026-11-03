//! Standalone capture CLI until the `doclify` executable absorbs these
//! subcommands. `hook` always exits 0 and prints only SessionStart context.

use std::process::ExitCode;

use doclify_capture::{hook, install, Agent, RepoInfo};

const USAGE: &str = "usage: doclify-capture hook <codex|claude>
       doclify-capture install [--exe <command>]
       doclify-capture uninstall";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["hook", agent] => {
            if let Some(agent) = Agent::parse(agent) {
                let _ = hook::run(agent);
            }
            ExitCode::SUCCESS
        }
        ["install", rest @ ..] => {
            let exe = match rest {
                [] => install::DEFAULT_EXE,
                ["--exe", exe] => exe,
                _ => return usage(),
            };
            report(repo().and_then(|r| install::install(&r, exe)), true)
        }
        ["uninstall"] => report(repo().and_then(|r| install::uninstall(&r)), false),
        _ => usage(),
    }
}

fn repo() -> std::io::Result<RepoInfo> {
    RepoInfo::discover(&std::env::current_dir()?)
        .ok_or_else(|| std::io::Error::other("not inside a git repository"))
}

fn report(result: std::io::Result<Vec<std::path::PathBuf>>, installed: bool) -> ExitCode {
    match result {
        Ok(paths) => {
            for p in paths {
                println!(
                    "{} {}",
                    if installed { "installed" } else { "removed" },
                    p.display()
                );
            }
            if installed {
                println!("Codex: trust this project and approve the new hooks in Codex's /hooks view; until then Codex capture is inactive.");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("doclify-capture: {e}");
            ExitCode::FAILURE
        }
    }
}

fn usage() -> ExitCode {
    eprintln!("{USAGE}");
    ExitCode::from(2)
}
