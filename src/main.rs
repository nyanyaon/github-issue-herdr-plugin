//! The viewer — `bin/herdr-issues`, the command a herdr pane runs.

use std::io;

use crossterm::event::{self, Event};
use ratatui::DefaultTerminal;

use herdr_issues::app::App;
use herdr_issues::environment::Environment;
use herdr_issues::signals;

fn main() -> io::Result<()> {
    let environment = Environment::from_process();
    let mut terminal = ratatui::init();
    signals::on_hangup(|| {
        ratatui::restore();
    })?;
    let mut app = App::start(&environment);
    let outcome = run(&mut terminal, &mut app);
    ratatui::restore();
    outcome
}

/// The event loop. Between renders the process blocks in `event::read`: no
/// timer, no polling thread, no held subscription.
fn run(terminal: &mut DefaultTerminal, app: &mut App) -> io::Result<()> {
    loop {
        terminal.draw(|frame| app.render(frame))?;
        if app.should_exit() {
            return Ok(());
        }
        match event::read()? {
            Event::Key(key) => app.handle_key(key),
            // SIGWINCH. The next draw lays out against the new size and re-wraps.
            Event::Resize(_, _) => {}
            _ => {}
        }
    }
}
