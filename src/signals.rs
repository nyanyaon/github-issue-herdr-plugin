//! Signal handling.
//!
//! `SIGWINCH` needs no code of its own: crossterm turns it into a resize event,
//! and the next draw recomputes the layout and re-wraps.
//!
//! `SIGHUP` is what `herdr pane close` sends, and it must leave the alternate
//! screen and exit 0. The handler thread blocks on the signal pipe until that
//! happens, so it is not a timer, a poll or a watcher — it costs nothing while
//! the pane is idle.

use std::io;
use std::process;
use std::thread;

use signal_hook::consts::SIGHUP;
use signal_hook::iterator::Signals;

pub fn on_hangup(restore: impl FnOnce() + Send + 'static) -> io::Result<()> {
    let mut signals = Signals::new([SIGHUP])?;
    thread::Builder::new()
        .name("hangup".to_string())
        .spawn(move || {
            if signals.forever().next().is_some() {
                restore();
                process::exit(0);
            }
        })?;
    Ok(())
}
