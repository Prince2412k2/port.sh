//! Diagnostics that know whether anyone can read them.
//!
//! Every one of these used to be an `eprintln!`, which is correct right up
//! until the terminal is showing a drawn page. Then it is not a message, it is
//! a hole: the text lands at the cursor, pushes the rest of the screen down a
//! line, and ratatui -- which believes it knows what is on every cell -- goes
//! on diffing against a picture that no longer matches. Nothing repairs until
//! something forces a full repaint, so one stray line rots the frame from
//! wherever it landed to the bottom of the screen.
//!
//! The one that did it was the tool server. It starts lazily when the `ask`
//! section is first opened, so `portfolio: tool server on http://...` and the
//! list of tools behind it arrived *during* a section change, and the museum's
//! artwork was left stranded at the top of the page underneath them.
//!
//! So notes go to stderr while the terminal is ours to print on, and into a
//! holding buffer while a page is up. `release` empties it on the way out, so
//! the diagnostics are still there -- after the screen has been handed back,
//! which is the only time they could have been read anyway.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

static HELD: AtomicBool = AtomicBool::new(false);
static WAITING: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Past this the buffer stops growing. A run that produces more notes than
/// this while a page is up has something worse wrong with it than a lost line.
const KEEP: usize = 64;

/// The screen is ours to draw on; notes wait.
pub fn hold() {
    HELD.store(true, Ordering::Relaxed);
}

/// The screen is handed back. Say everything that was held.
pub fn release() {
    HELD.store(false, Ordering::Relaxed);
    if let Ok(mut held) = WAITING.lock() {
        for line in held.drain(..) {
            eprintln!("{line}");
        }
    }
}

#[doc(hidden)]
pub fn say(line: String) {
    if !HELD.load(Ordering::Relaxed) {
        eprintln!("{line}");
        return;
    }
    if let Ok(mut held) = WAITING.lock() {
        if held.len() < KEEP {
            held.push(line);
        }
    }
}

/// `eprintln!`, except that it waits its turn when a page is on the screen.
#[macro_export]
macro_rules! note {
    ($($arg:tt)*) => { $crate::note::say(format!($($arg)*)) };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Held notes are kept rather than lost, arrive when the screen goes
    /// back, and cannot grow without end.
    ///
    /// One test and not three, because `HELD` and `WAITING` are process-wide
    /// and the test harness runs threads: split up, they raced each other's
    /// state and failed on whichever ran second.
    #[test]
    fn notes_wait_for_the_screen_and_then_arrive() {
        release();
        hold();
        say("held one".into());
        say("held two".into());
        assert_eq!(WAITING.lock().unwrap().len(), 2, "a note was dropped");

        for i in 0..KEEP * 3 {
            say(format!("note {i}"));
        }
        assert_eq!(WAITING.lock().unwrap().len(), KEEP, "the buffer has no bottom");

        release();
        assert!(WAITING.lock().unwrap().is_empty(), "the buffer did not drain");
        assert!(!HELD.load(Ordering::Relaxed));

        // And with the screen back, a note goes straight out.
        say("straight through".into());
        assert!(WAITING.lock().unwrap().is_empty(), "a note was held with no screen up");
    }
}
