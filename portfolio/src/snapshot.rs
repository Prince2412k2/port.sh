//! Draw one frame to stdout and exit.
//!
//! The same development aid the other two crates have, and for the same reason:
//! every layout decision in here was checked by dumping a frame and looking at
//! it, not by describing the change and believing the description.

use ratatui::backend::TestBackend;
use ratatui::Terminal;

use crate::shell::{Section, Shell};

pub struct Opts {
    pub width: u16,
    pub height: u16,
    pub plain: bool,
    pub section: Option<String>,
    /// Seconds of animation to run before drawing, stepped at a fixed 1/60 so
    /// the same command always produces the same pixels.
    pub at: Option<f64>,
    /// Rows into a scrolling section.
    pub scroll: Option<u16>,
}

pub fn render(o: &Opts) -> std::io::Result<()> {
    let mut shell = Shell::new();
    let mut term = Terminal::new(TestBackend::new(o.width, o.height)).unwrap();

    // `--section boot` is the only way to see the opening in a snapshot;
    // everything else would otherwise render a title card.
    if o.section.as_deref() == Some("boot") {
        for _ in 0..(o.at.unwrap_or(0.8) * 60.0) as usize {
            shell.tick(1.0 / 60.0);
        }
        term.draw(|f| shell.render(f)).unwrap();
        let buf = term.backend().buffer();
        print!(
            "{}",
            if o.plain { termap::snapshot::plain(buf) } else { termap::snapshot::ansi(buf) }
        );
        return Ok(());
    }
    shell.skip_boot();

    if let Some(name) = &o.section {
        let Some(s) = Section::ALL.into_iter().find(|s| s.label() == name) else {
            eprintln!("portfolio: no section `{name}` (try: home experience projects skills)");
            return Ok(());
        };
        shell.go(s);
        // The section's own deferred setup — the tour framing itself against
        // the whole basemap, for one — happens on its first frame.
        term.draw(|f| shell.render(f)).unwrap();
        // Land it, unless a specific moment was asked for.
        let steps = match o.at {
            Some(secs) => (secs * 60.0).round().max(0.0) as usize,
            None => 60 * 12,
        };
        for _ in 0..steps {
            if o.at.is_none() && !shell.animating() {
                break;
            }
            shell.tick(1.0 / 60.0);
            // Redraw as we go: the map resolves tiles during render, so a tour
            // that ticks without drawing lands on a view whose tiles were never
            // fetched.
            term.draw(|f| shell.render(f)).unwrap();
        }
    }

    if let Some(n) = o.scroll {
        shell.set_scroll(n);
    }
    // Two passes: hover and the pick buffer it reads from are one frame apart.
    term.draw(|f| shell.render(f)).unwrap();
    term.draw(|f| shell.render(f)).unwrap();

    let buf = term.backend().buffer();
    print!(
        "{}",
        if o.plain { termap::snapshot::plain(buf) } else { termap::snapshot::ansi(buf) }
    );
    Ok(())
}
