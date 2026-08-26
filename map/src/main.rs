
use std::io::{self, Write};
use std::time::{Duration, Instant};

use crossterm::event::{self, Event};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use termap::app::App;
use termap::tiles::Source;
use termap::{canvas, snapshot, style, ui, view};

type Term = Terminal<CrosstermBackend<io::Stdout>>;

fn main() -> io::Result<()> {
    let mut args = std::env::args().skip(1);
    let mut path: Option<String> = None;
    let mut shot: Option<snapshot::Opts> = None;
    let mut tour = false;
    let mut start_at: Option<String> = None;

    while let Some(a) = args.next() {
        match a.as_str() {
            "-h" | "--help" => {
                println!("termap 0.1.0 -- a terminal map\n");
                println!("usage: termap [PATH.tmap] [options]\n");
                println!("  --snapshot WxH   draw one frame to stdout and exit");
                println!("  --plain          snapshot without colour");
                println!("  --theme MODE     night | paper");
                println!("  --zoom Z         start at zoom Z");
                println!("  --cursor X,Y     place the focus cursor (snapshot only)");
                println!("  --focus MODE     depth focus: off | subtle | strong");
                println!("  --roads MODE     road glyphs: braille | blocks | lines");
                println!("  --ground MODE    terrain: relief | contour | hachure | shade");
                println!("  --weight W       stroke width multiplier (0.4 - 2.5)");
                println!("  --center LON,LAT centre the view");
                println!("  --tilt DEG       camera pitch (0 = flat 2D)");
                println!("  --bearing DEG    camera rotation");
                println!();
                println!("  --tour           open on the experience tour");
                println!("  --place ID       tour stop to open on, or snapshot");
                println!("  --from ID        snapshot mid-flight: fly from here to --place");
                println!("  --at SECONDS     how far into that flight to draw");
                println!();
                println!("Reads data/mumbai.tmap if present, otherwise falls back to the");
                println!("embedded sample. Build real data with scripts/fetch-osm.sh.");
                return Ok(());
            }
            "--snapshot" => {
                let dims = args.next().unwrap_or_else(|| "180x48".into());
                let (w, h) = dims.split_once('x').unwrap_or(("180", "48"));
                shot = Some(snapshot::Opts {
                    width: w.parse().unwrap_or(180),
                    height: h.parse().unwrap_or(48),
                    plain: false,
                    theme: None,
                    zoom: None,
                    cursor: None,
                    focus: None,
                    roads: None,
                    ground: None,
                    weight: None,
                    center: None,
                    tilt: None,
                    bearing: None,
                    place: None,
                    from: None,
                    at: None,
                });
            }
            "--theme" => {
                let t = match args.next().as_deref() {
                    Some("paper") | Some("light") => Some(canvas::Theme::Paper),
                    Some("night") | Some("dark") => Some(canvas::Theme::Night),
                    _ => None,
                };
                if let Some(s) = shot.as_mut() {
                    s.theme = t;
                }
            }
            "--plain" => {
                if let Some(s) = shot.as_mut() {
                    s.plain = true;
                }
            }
            "--zoom" => {
                let z: Option<f64> = args.next().and_then(|v| v.parse().ok());
                if let Some(s) = shot.as_mut() {
                    s.zoom = z;
                }
            }
            "--focus" => {
                let m = match args.next().as_deref() {
                    Some("off") => Some(style::FocusMode::Off),
                    Some("strong") => Some(style::FocusMode::Strong),
                    Some("subtle") => Some(style::FocusMode::Subtle),
                    _ => None,
                };
                if let Some(s) = shot.as_mut() {
                    s.focus = m;
                }
            }
            "--roads" => {
                let g = match args.next().as_deref() {
                    Some("braille") => Some(canvas::RoadGlyph::Dotted),
                    Some("blocks") => Some(canvas::RoadGlyph::Block),
                    Some("lines") => Some(canvas::RoadGlyph::Line),
                    _ => None,
                };
                if let Some(s) = shot.as_mut() {
                    s.roads = g;
                }
            }
            "--ground" => {
                let g = match args.next().as_deref() {
                    Some("relief") => Some(view::Ground::Ribbon),
                    Some("contour") => Some(view::Ground::Contour),
                    Some("shade") => Some(view::Ground::Shade),
                    Some("hachure") => Some(view::Ground::Hachure),
                    _ => None,
                };
                if let Some(s) = shot.as_mut() {
                    s.ground = g;
                }
            }
            "--weight" => {
                let w: Option<f64> = args.next().and_then(|v| v.parse().ok());
                if let Some(s) = shot.as_mut() {
                    s.weight = w;
                }
            }
            "--center" => {
                let c = args.next().and_then(|v| {
                    let (a, b) = v.split_once(',')?;
                    Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
                });
                if let Some(s) = shot.as_mut() {
                    s.center = c;
                }
            }
            "--tilt" => {
                let v: Option<f64> = args.next().and_then(|v| v.parse().ok());
                if let Some(s) = shot.as_mut() {
                    s.tilt = v;
                }
            }
            "--bearing" => {
                let v: Option<f64> = args.next().and_then(|v| v.parse().ok());
                if let Some(s) = shot.as_mut() {
                    s.bearing = v;
                }
            }
            "--cursor" => {
                let c = args.next().and_then(|v| {
                    let (x, y) = v.split_once(',')?;
                    Some((x.parse().ok()?, y.parse().ok()?))
                });
                if let Some(s) = shot.as_mut() {
                    s.cursor = c;
                }
            }
            "--tour" => tour = true,
            "--place" => {
                let v = args.next();
                match shot.as_mut() {
                    Some(s) => s.place = v,
                    None => {
                        tour = true;
                        start_at = v;
                    }
                }
            }
            "--from" => {
                let v = args.next();
                if let Some(s) = shot.as_mut() {
                    s.from = v;
                }
            }
            "--at" => {
                let v: Option<f64> = args.next().and_then(|v| v.parse().ok());
                if let Some(s) = shot.as_mut() {
                    s.at = v;
                }
            }
            other => path = Some(other.to_string()),
        }
    }

    let mut app = App::new(Source::open(path.as_deref()));

    if let Some(o) = shot {
        return snapshot::render(&mut app, &o);
    }

    if tour {
        let i = start_at
            .as_deref()
            .and_then(|id| app.tour.places.iter().position(|p| p.id == id))
            .unwrap_or(0);
        app.start_tour(i);
    }

    let mut term = setup()?;
    install_panic_hook();

    let result = run(&mut term, &mut app);

    restore(&mut term)?;
    result
}

fn run(term: &mut Term, app: &mut App) -> io::Result<()> {
    let mut last = Instant::now();
    loop {
        term.draw(|f| ui::render(f, app))?;

        // Poll hard while something is animating and lazily when it is not.
        // A flight wants smooth frames; a still map wants an idle process, and
        // over SSH that difference is the difference between a quiet link and a
        // steady trickle of repaints.
        let wait = if app.animating() { 25 } else { 120 };
        if event::poll(Duration::from_millis(wait))? {
            // Drain everything queued before drawing again. With any-motion
            // tracking on, a fast drag can deliver dozens of events per frame
            // and rendering each one would only add latency.
            loop {
                match event::read()? {
                    Event::Key(k) => app.on_key(k),
                    Event::Mouse(m) => app.on_mouse(m),
                    Event::Resize(_, _) => {}
                    _ => {}
                }
                if app.quit || !event::poll(Duration::ZERO)? {
                    break;
                }
            }
        }

        if app.quit {
            return Ok(());
        }

        // Clamped, so a stalled link plays the rest of the flight slowly rather
        // than teleporting the camera to the end of it. A scripted animation
        // that skips is worse than one that lags.
        let now = Instant::now();
        let dt = now.duration_since(last).as_secs_f64().min(0.1);
        last = now;
        app.tick(dt);
    }
}

fn setup() -> io::Result<Term> {
    enable_raw_mode()?;
    let mut out = io::stdout();
    execute!(
        out,
        EnterAlternateScreen,
        event::EnableMouseCapture,
        crossterm::cursor::Hide
    )?;
    // Crossterm's mouse capture only reports motion while a button is held.
    // Mode 1003 reports it unconditionally, which is what hover needs.
    write!(out, "\x1b[?1003h")?;
    out.flush()?;
    Terminal::new(CrosstermBackend::new(out))
}

fn restore(term: &mut Term) -> io::Result<()> {
    let mut out = io::stdout();
    write!(out, "\x1b[?1003l")?;
    out.flush()?;
    execute!(
        out,
        crossterm::cursor::Show,
        event::DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    disable_raw_mode()?;
    let _ = term.show_cursor();
    Ok(())
}

/// A panic in raw mode leaves the terminal unusable, so put it back first.
fn install_panic_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let mut out = io::stdout();
        let _ = write!(out, "\x1b[?1003l");
        let _ = execute!(
            out,
            crossterm::cursor::Show,
            event::DisableMouseCapture,
            LeaveAlternateScreen
        );
        let _ = disable_raw_mode();
        prev(info);
    }));
}
