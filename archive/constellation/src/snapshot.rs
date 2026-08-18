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
    pub zoom: Option<f64>,
    pub focus: Option<String>,
    pub select: Option<String>,
    pub find: Option<String>,
    pub cursor: Option<(u16, u16)>,
    pub keys: bool,
    pub dust: Option<bool>,
    pub figures: Option<bool>,
}

pub fn render(app: &mut App, o: &Opts) -> std::io::Result<()> {
    if let Some(d) = o.dust {
        app.dust = d;
    }
    if let Some(g) = o.figures {
        app.figures = g;
    }
    if let Some(id) = &o.focus {
        match app.sky.con_by_id(id) {
            Some(c) => app.focus_con(c),
            None => return fail(format!("no project called {id:?}")),
        }
    }
    if let Some(q) = &o.find {
        app.set_query(q.clone());
    }
    if let Some(id) = &o.select {
        match app.sky.star_by_id(id) {
            Some(s) => {
                app.select(Some(s));
                app.view.look_at(app.lay.pos[s]);
                if app.view.zoom < 1.6 {
                    app.view.zoom = 1.6;
                }
            }
            None => return fail(format!("no skill called {id:?}")),
        }
    }
    // Zoom last: it is the one thing the reader is most likely to be overriding
    // on purpose, and focus/select both move the camera themselves.
    if let Some(z) = o.zoom {
        app.take_the_wheel();
        app.view.zoom = z.clamp(crate::sky::MIN_ZOOM, crate::sky::MAX_ZOOM);
    }
    if o.keys {
        app.mode = crate::app::Mode::Help;
    }
    app.cursor = o.cursor;

    // TestBackend is infallible, hence the unwraps.
    let mut term = Terminal::new(TestBackend::new(o.width, o.height)).unwrap();
    // Two passes: hover and the pick buffer it reads from are one frame apart,
    // so a single pass would snapshot a state no reader ever sees.
    term.draw(|f| ui::render(f, app)).unwrap();
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
