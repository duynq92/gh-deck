mod api;
mod app;
mod config;
mod doc;
mod form;
mod gh;
mod keys;
mod tasks;
mod ui;

use app::{App, Msg};
use ratatui::crossterm::event::{self, DisableBracketedPaste, EnableBracketedPaste};
use ratatui::crossterm::execute;
use std::io::{self, stdout};
use std::process::ExitCode;
use std::sync::mpsc::{self, Receiver};
use std::thread;

const USAGE: &str = "Usage: ghdeck [OWNER/REPO]

Keyboard-driven TUI for GitHub pull requests, issues, releases and Actions.
Without an argument it shows all your repositories. Requires a signed-in `gh`.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let scope = match args.as_slice() {
        [] => None,
        [a] if a == "-h" || a == "--help" => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        [a] if a == "-V" || a == "--version" => {
            println!("ghdeck {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        [repo] if api::valid_repo(repo) => Some(repo.clone()),
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    let result = tui(scope);
    if let Err(e) = result {
        eprintln!("ghdeck: {e}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn tui(scope: Option<String>) -> io::Result<()> {
    let mut terminal = ratatui::try_init()?;
    let result = execute!(stdout(), EnableBracketedPaste).and_then(|_| run(&mut terminal, scope));
    let paste = execute!(stdout(), DisableBracketedPaste);
    ratatui::try_restore()?;
    result.and(paste)
}

fn run(terminal: &mut ratatui::DefaultTerminal, scope: Option<String>) -> io::Result<()> {
    let (tx, rx) = mpsc::channel();
    let input = tx.clone();
    thread::Builder::new()
        .stack_size(128 << 10)
        .spawn(move || {
            loop {
                let event = event::read();
                let failed = event.is_err();
                if input.send(Msg::Input(event)).is_err() || failed {
                    return;
                }
            }
        })?;
    let mut app = App::new(tx, scope);
    app.start();
    event_loop(terminal, &mut app, &rx)
}

/// Blocks until something happens: no timers, so an idle app uses no CPU.
fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    rx: &Receiver<Msg>,
) -> io::Result<()> {
    while !app.quit {
        terminal.draw(|frame| ui::draw(frame, app))?;
        // `app` owns a sender, so the channel cannot disconnect.
        let first = rx.recv().map_err(io::Error::other)?;
        app.handle(first);
        while !app.quit {
            let Ok(msg) = rx.try_recv() else { break };
            app.handle(msg);
        }
    }
    Ok(())
}
