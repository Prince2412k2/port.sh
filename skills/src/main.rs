mod app;
mod canvas;
mod cards;
mod data;
mod grid;
mod logos;
mod marks;
mod scene;
mod snapshot;
mod tile;
mod ui;

use std::io::{self, Write};
use std::time::{Duration, Instant};

use crossterm::event::{self, Event};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use crate::app::App;

type Term = Terminal<CrosstermBackend<io::Stdout>>;

/// Frame interval while the sheet is drifting. Slow on purpose: the motion is
/// meant to be barely perceptible, and every frame is bytes down someone's ssh
/// connection. Nothing is redrawn at all when the sheet is held still.
const FRAME: Duration = Duration::from_millis(90);

fn main() {
    if let Err(e) = run_main() {
        eprintln!("skysheet: {e}");
        std::process::exit(1);
    }
}

fn run_main() -> io::Result<()> {
    let mut args = std::env::args().skip(1);
    let mut shot: Option<snapshot::Opts> = None;
    let mut logo_sheet = false;
    let mut mark_sheet = false;

    while let Some(a) = args.next() {
        match a.as_str() {
            "-h" | "--help" => {
                usage();
                return Ok(());
            }
            "--logos" => logo_sheet = true,
            "--marks" => mark_sheet = true,
            "--snapshot" => {
                let (w, h) = args
                    .next()
                    .and_then(|v| {
                        let (a, b) = v.to_lowercase().split_once('x').map(|(a, b)| {
                            (a.trim().to_string(), b.trim().to_string())
                        })?;
                        Some((a.parse().ok()?, b.parse().ok()?))
                    })
                    .unwrap_or((140, 40));
                shot = Some(snapshot::Opts { width: w, height: h, ..Default::default() });
            }
            "--plain" => set(&mut shot, |s| s.plain = true),
            "--tab" => {
                let t = args.next();
                set(&mut shot, |s| s.tab = t.clone());
            }
            "--project" => {
                let v = args.next();
                set(&mut shot, |s| s.project = v.clone());
            }
            "--at" => {
                let v = args.next().and_then(|v| v.parse().ok());
                set(&mut shot, |s| s.at = v);
            }
            "--cursor" => {
                let c = args.next().and_then(|v| {
                    let (x, y) = v.split_once(',')?;
                    Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
                });
                set(&mut shot, |s| s.cursor = c);
            }
            "--drift" => {
                let c = args.next().and_then(|v| {
                    let (x, y) = v.split_once(',')?;
                    Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
                });
                set(&mut shot, |s| s.drift = c);
            }
            other => {
                return Err(io::Error::other(format!("unknown option {other:?}")));
            }
        }
    }

    if logo_sheet {
        print!("{}", logos::sheet());
        return Ok(());
    }
    if mark_sheet {
        print!("{}", marks::sheet());
        return Ok(());
    }

    let mut app = App::new();
    if let Some(o) = shot {
        return snapshot::render(&mut app, &o);
    }

    let mut term = setup()?;
    install_panic_hook();
    let result = run(&mut term, &mut app);
    restore(&mut term)?;
    result
}

fn set(shot: &mut Option<snapshot::Opts>, f: impl FnOnce(&mut snapshot::Opts)) {
    if let Some(s) = shot.as_mut() {
        f(s);
    }
}

fn run(term: &mut Term, app: &mut App) -> io::Result<()> {
    let mut last = Instant::now();
    loop {
        if app.dirty {
            app.dirty = false;
            term.draw(|f| ui::render(f, app))?;
        }

        // A moving sheet needs a clock; a still one needs nothing at all, and
        // waits on input indefinitely rather than waking to redraw a frame
        // identical to the last. A throw keeps this true after the hand has
        // left: the sheet asks for frames until it has coasted to a stop.
        let moving = app.moving();
        let wait = if moving { FRAME } else { Duration::from_millis(400) };

        if event::poll(wait)? {
            loop {
                match event::read()? {
                    Event::Key(k) if k.is_press() => app.on_key(k),
                    Event::Mouse(m) => app.on_mouse(m),
                    Event::Resize(_, _) => app.dirty = true,
                    _ => {}
                }
                if app.quit || !event::poll(Duration::ZERO)? {
                    break;
                }
            }
        }

        let now = Instant::now();
        let dt = now.duration_since(last).as_secs_f64();
        last = now;
        // Ticked on whatever the state was *after* the events, so a scroll
        // that arrived during the wait starts moving on this frame rather than
        // the next one.
        if moving || app.moving() {
            app.tick(dt.min(0.25));
        }

        if app.quit {
            return Ok(());
        }
    }
}

fn usage() {
    println!("skysheet 0.2.0 -- projects and skills, in a terminal\n");
    println!("usage: skysheet [options]\n");
    println!("  --snapshot WxH   draw one frame to stdout and exit");
    println!("  --plain          snapshot without colour");
    println!("  --tab NAME       which tab to draw: projects | skills");
    println!("  --project ID     which card to draw, e.g. watch-party");
    println!("  --at SECONDS     pin the animation clock (snapshot only)");
    println!("  --cursor X,Y     place the pointer on the sheet (snapshot only)");
    println!("  --drift X,Y      slide the sheet before drawing (snapshot only)");
    println!("  --logos          print every tool mark and exit");
    println!("  --marks          print every project mark and exit");
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
    // Mode 1003 reports it unconditionally, which is the entire input for the
    // sheet -- without it there is no magnet.
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
