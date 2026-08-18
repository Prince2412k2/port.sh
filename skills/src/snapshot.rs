//! Render a single frame to stdout and exit.
//!
//! Every rendering decision in here was checked this way. A star field is
//! almost entirely a matter of thresholds — how faint the dust is, how far the
//! spikes reach, when a label is worth drawing — and none of those can be
//! judged from a description of the change. Diff the frames instead.

use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};
use ratatui::Terminal;

use crate::app::App;
use crate::ui;

#[derive(Default)]
pub struct Opts {
    pub width: u16,
    pub height: u16,
    pub plain: bool,
    pub tab: Option<String>,
    pub project: Option<String>,
    pub at: Option<f64>,
    pub cursor: Option<(f64, f64)>,
    pub drift: Option<(f64, f64)>,
}

pub fn render(app: &mut App, o: &Opts) -> std::io::Result<()> {
    if let Some(name) = &o.tab {
        app.tab = match name.as_str() {
            "projects" => crate::app::Tab::Projects,
            "skills" => crate::app::Tab::Skills,
            other => return fail(format!("no tab called {other:?}")),
        };
    }
    if let Some(d) = o.drift {
        app.drift = d;
    }
    if let Some(id) = &o.project {
        match app.projects.iter().position(|p| &p.id == id) {
            Some(i) => app.at = i,
            None => return fail(format!("no project called {id:?}")),
        }
    }
    // The clock is an input, not a side effect: a scene is a pure function of
    // it, so any instant of any animation can be pinned and diffed.
    if let Some(t) = o.at {
        app.t = t;
    }

    // TestBackend is infallible, hence the unwraps.
    let mut term = Terminal::new(TestBackend::new(o.width, o.height)).unwrap();
    // Two passes: the pointer is resolved against an area the first pass is
    // what establishes, so one pass would snapshot a state no reader ever sees.
    term.draw(|f| ui::render(f, app)).unwrap();
    if let Some((x, y)) = o.cursor {
        app.cursor = Some((x, y));
    }
    term.draw(|f| ui::render(f, app)).unwrap();

    let buf = term.backend().buffer().clone();
    print!("{}", if o.plain { plain(&buf) } else { ansi(&buf) });
    Ok(())
}

fn fail(msg: String) -> std::io::Result<()> {
    Err(std::io::Error::other(msg))
}

fn plain(buf: &Buffer) -> String {
    let mut s = String::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            s.push_str(buf.cell((x, y)).map_or(" ", |c| c.symbol()));
        }
        s.push('\n');
    }
    s
}

fn ansi(buf: &Buffer) -> String {
    let mut s = String::new();
    let mut last: Option<(Color, Color, Modifier)> = None;

    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            let Some(cell) = buf.cell((x, y)) else { continue };
            let key = (cell.fg, cell.bg, cell.modifier);
            if last != Some(key) {
                s.push_str("\x1b[0m");
                if let Some(c) = sgr(cell.fg, true) {
                    s.push_str(&c);
                }
                if let Some(c) = sgr(cell.bg, false) {
                    s.push_str(&c);
                }
                if cell.modifier.contains(Modifier::BOLD) {
                    s.push_str("\x1b[1m");
                }
                last = Some(key);
            }
            s.push_str(cell.symbol());
        }
        s.push_str("\x1b[0m\n");
        last = None;
    }
    s
}

fn sgr(c: Color, fg: bool) -> Option<String> {
    let base = if fg { 38 } else { 48 };
    match c {
        Color::Rgb(r, g, b) => Some(format!("\x1b[{base};2;{r};{g};{b}m")),
        Color::Indexed(i) => Some(format!("\x1b[{base};5;{i}m")),
        _ => None,
    }
}
