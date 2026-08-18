//! Render a single frame to stdout and exit.
//!
//! Useful for eyeballing style changes without an interactive terminal, and for
//! diffing the renderer's output across edits.

use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};
use ratatui::Terminal;

use crate::app::App;
use crate::style::FocusMode;
use crate::ui;

pub struct Opts {
    pub width: u16,
    pub height: u16,
    pub plain: bool,
    pub zoom: Option<f64>,
    pub cursor: Option<(u16, u16)>,
    pub focus: Option<FocusMode>,
    pub roads: Option<crate::canvas::RoadGlyph>,
    pub weight: Option<f64>,
    pub center: Option<(f64, f64)>,
    pub tilt: Option<f64>,
    pub bearing: Option<f64>,
    /// Tour stop to render.
    pub place: Option<String>,
    /// Render mid-flight, coming from this stop.
    pub from: Option<String>,
    /// Seconds into the flight (or the arrival) to draw.
    pub at: Option<f64>,
}

pub fn render(app: &mut App, o: &Opts) -> std::io::Result<()> {
    if let Some(z) = o.zoom {
        app.set_zoom(z);
    }
    if let Some(m) = o.focus {
        app.focus = m;
    }
    if let Some(g) = o.roads {
        app.road_glyph = g;
    }
    if let Some(w) = o.weight {
        app.road_weight = w;
    }
    if let Some((lon, lat)) = o.center {
        app.vp.center = crate::geo::lonlat_to_world(lon, lat);
        app.set_zoom(app.vp.zoom);
    }
    // An explicit camera means manual: otherwise sync_camera overwrites it from
    // zoom on the first frame and the flag silently does nothing.
    if o.tilt.is_some() || o.bearing.is_some() {
        app.auto_view = false;
    }
    if let Some(t) = o.tilt {
        app.vp.tilt = t.to_radians();
    }
    if let Some(b) = o.bearing {
        app.vp.bearing = b.to_radians();
    }
    app.cursor = o.cursor;

    // TestBackend is infallible, hence the unwraps.
    let mut term = Terminal::new(TestBackend::new(o.width, o.height)).unwrap();

    if let Some(id) = &o.place {
        // One throwaway frame first: the flight is derived from the viewport's
        // width in world units, and the viewport does not know its own size
        // until something has drawn into it.
        term.draw(|f| ui::render(f, app)).unwrap();

        let find = |id: &str| app.tour.places.iter().position(|p| p.id == id);
        let Some(to) = find(id) else {
            eprintln!("termap: no place `{id}`");
            return Ok(());
        };
        match o.from.as_deref().and_then(find) {
            Some(from) => {
                // Land on the origin, let it settle, then take off. Stepping at
                // a fixed 1/60 makes the frame a pure function of --at, so two
                // runs of the same command produce the same pixels.
                app.start_tour(from);
                term.draw(|f| ui::render(f, app)).unwrap();
                run_to_rest(app);
                let vp = app.vp;
                app.tour.go(&vp, to);
            }
            None => {
                app.start_tour(to);
                // The opening descent is set up on the first frame after the
                // request, not on the request itself.
                term.draw(|f| ui::render(f, app)).unwrap();
            }
        }
        match o.at {
            Some(secs) => {
                for _ in 0..(secs * 60.0).round().max(0.0) as usize {
                    app.tick(1.0 / 60.0);
                }
            }
            None => run_to_rest(app),
        }
    }
    // Two passes: hover and the pick buffer it reads from are one frame apart,
    // so a single pass would snapshot a state the user never sees.
    term.draw(|f| ui::render(f, app)).unwrap();
    term.draw(|f| ui::render(f, app)).unwrap();

    let buf = term.backend().buffer().clone();
    print!("{}", if o.plain { plain(&buf) } else { ansi(&buf) });
    Ok(())
}

/// Advance the tour until it stops moving, with a bound so a bug cannot hang a
/// snapshot forever.
fn run_to_rest(app: &mut App) {
    for _ in 0..(60 * 20) {
        if !app.animating() {
            break;
        }
        app.tick(1.0 / 60.0);
    }
}

/// Public so the portfolio can print its own frames without a third copy of an
/// ANSI writer existing.
pub fn plain(buf: &Buffer) -> String {
    let mut s = String::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            s.push_str(buf.cell((x, y)).map_or(" ", |c| c.symbol()));
        }
        s.push('\n');
    }
    s
}

pub fn ansi(buf: &Buffer) -> String {
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
        Color::Reset => None,
        _ => None,
    }
}
