mod app;
mod canvas;
mod card;
mod data;
mod draw;
mod labels;
mod layout;
mod logos;
mod sky;
mod snapshot;
mod ui;

use std::io::{self, Write};
use std::time::Duration;

use crossterm::event::{self, Event};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use crate::app::App;

/// The sheet is compiled in, so the program is one file with no data directory
/// to lose. A path argument overrides it, which is how the sheet gets edited
/// without a rebuild.
const SHEET: &str = include_str!("../data/skills.sky");

type Term = Terminal<CrosstermBackend<io::Stdout>>;

fn main() {
    if let Err(e) = run_main() {
        // Plain, because the most likely error by far is a typo in a
        // hand-edited sheet, and `Error: Custom { kind: Other, .. }` is not
        // what anybody wants to read after making one.
        eprintln!("skysheet: {e}");
        std::process::exit(1);
    }
}

fn run_main() -> io::Result<()> {
    let mut args = std::env::args().skip(1);
    let mut path: Option<String> = None;
    let mut shot: Option<snapshot::Opts> = None;
    let mut layout_only: Option<Option<String>> = None;
    let mut logo_sheet = false;

    while let Some(a) = args.next() {
        match a.as_str() {
            "-h" | "--help" => {
                usage();
                return Ok(());
            }
            "--snapshot" => {
                let (w, h) = args
                    .next()
                    .and_then(|v| {
                        let (a, b) = v.to_lowercase().split_once('x').map(|(a, b)| {
                            (a.trim().to_string(), b.trim().to_string())
                        })?;
                        Some((a.parse().ok()?, b.parse().ok()?))
                    })
                    .unwrap_or((160, 46));
                shot = Some(snapshot::Opts { width: w, height: h, ..Default::default() });
            }
            "--layout" => layout_only = Some(args.next()),
            "--logos" => logo_sheet = true,
            "--plain" => set(&mut shot, |s| s.plain = true),
            "--keys" => set(&mut shot, |s| s.keys = true),
            "--no-dust" => set(&mut shot, |s| s.dust = Some(false)),
            "--no-figures" => set(&mut shot, |s| s.figures = Some(false)),
            "--zoom" => {
                let z = args.next().and_then(|v| v.parse().ok());
                set(&mut shot, |s| s.zoom = z);
            }
            "--focus" => {
                let v = args.next();
                set(&mut shot, |s| s.focus = v.clone());
            }
            "--select" => {
                let v = args.next();
                set(&mut shot, |s| s.select = v.clone());
            }
            "--find" => {
                let v = args.next();
                set(&mut shot, |s| s.find = v.clone());
            }
            "--cursor" => {
                let c = args.next().and_then(|v| {
                    let (x, y) = v.split_once(',')?;
                    Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
                });
                set(&mut shot, |s| s.cursor = c);
            }
            other => path = Some(other.to_string()),
        }
    }

    let sheet = match &path {
        Some(p) => std::fs::read_to_string(p)
            .map_err(|e| io::Error::new(e.kind(), format!("{p}: {e}")))?,
        None => SHEET.to_string(),
    };
    let mut app = App::new(&sheet).map_err(io::Error::other)?;

    if logo_sheet {
        print!("{}", logos::sheet());
        return Ok(());
    }

    if let Some(only) = layout_only {
        print!("{}", layout::report(&app.sky, &app.lay, only.as_deref()));
        return Ok(());
    }

    if let Some(o) = shot {
        return snapshot::render(&mut app, &o);
    }

    let mut term = setup()?;
    install_panic_hook();
    let result = run(&mut term, &mut app);
    restore(&mut term)?;
    result
}

/// Options only mean anything alongside `--snapshot`; quietly ignoring them
/// otherwise beats making the caller remember the order they came in.
fn set(shot: &mut Option<snapshot::Opts>, f: impl FnOnce(&mut snapshot::Opts)) {
    if let Some(s) = shot.as_mut() {
        f(s);
    }
}

fn run(term: &mut Term, app: &mut App) -> io::Result<()> {
    loop {
        if app.dirty {
            // Cleared *before* the draw, because resolving hover during the
            // draw can legitimately dirty the next frame.
            app.dirty = false;
            term.draw(|f| ui::render(f, app))?;
        }

        // The sky does not animate. Blocking here rather than redrawing on a
        // timer is what keeps an idle session at zero CPU and zero bytes —
        // which matters when the audience arrives over SSH.
        if event::poll(Duration::from_millis(250))? {
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

        if app.quit {
            return Ok(());
        }
    }
}

fn usage() {
    println!("skysheet 0.1.0 -- skills as a night sky\n");
    println!("usage: skysheet [PATH.sky] [options]\n");
    println!("  --snapshot WxH   draw one frame to stdout and exit");
    println!("  --plain          snapshot without colour");
    println!("  --keys           snapshot with the help overlay open");
    println!("  --zoom Z         start at zoom Z");
    println!("  --focus ID       focus one project, e.g. netjail");
    println!("  --select ID      open one skill's story, e.g. nftables");
    println!("  --find QUERY     apply a search");
    println!("  --cursor X,Y     place the hover cursor (snapshot only)");
    println!("  --layout [ID]    print where the simulation put things, and exit");
    println!("  --logos          print every tool mark and exit");
    println!("  --no-dust        without the milky way and background stars");
    println!("  --no-figures     without the constellation lines");
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
