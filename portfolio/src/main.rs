//! An interactive portfolio, served over SSH.

mod acp;
mod about;
mod ask;
mod context;
mod emblems;
mod home;
mod json;
mod page;
mod paint;
mod shell;
mod taste;
mod snapshot;

use std::io::{self, Write};
use std::time::{Duration, Instant};

use crossterm::event::{self, Event};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use crate::shell::Shell;

type Term = Terminal<CrosstermBackend<io::Stdout>>;

fn main() -> io::Result<()> {
    let mut args = std::env::args().skip(1);
    let mut shot: Option<snapshot::Opts> = None;
    let mut start: Option<String> = None;

    while let Some(a) = args.next() {
        match a.as_str() {
            "-h" | "--help" => {
                println!("portfolio -- an interactive CV in a terminal\n");
                println!("  --section NAME   open on home | experience | projects | skills");
                println!("  --snapshot WxH   draw one frame to stdout and exit");
                println!("  --plain          snapshot without colour");
                println!("  --at SECONDS     how far into the section's animation to draw");
                return Ok(());
            }
            "--snapshot" => {
                let d = args.next().unwrap_or_else(|| "180x48".into());
                let (w, h) = d.split_once('x').unwrap_or(("180", "48"));
                shot = Some(snapshot::Opts {
                    width: w.parse().unwrap_or(180),
                    height: h.parse().unwrap_or(48),
                    plain: false,
                    section: None,
                    at: None,
                    scroll: None,
                });
            }
            "--plain" => {
                if let Some(s) = shot.as_mut() {
                    s.plain = true;
                }
            }
            "--at" => {
                let v = args.next().and_then(|v| v.parse().ok());
                if let Some(s) = shot.as_mut() {
                    s.at = v;
                }
            }
            "--emblems" => {
                print!("{}", emblems::sheet());
                return Ok(());
            }
            "--scroll" => {
                let v = args.next().and_then(|v| v.parse().ok());
                if let Some(s) = shot.as_mut() {
                    s.scroll = v;
                }
            }
            "--section" => start = args.next(),
            _ => {}
        }
    }

    if let Some(mut o) = shot {
        o.section = start.or(o.section);
        return snapshot::render(&o);
    }

    let mut term = setup()?;
    install_panic_hook();
    let result = run(&mut term, start);
    restore(&mut term)?;
    result
}

fn run(term: &mut Term, start: Option<String>) -> io::Result<()> {
    let mut shell = Shell::new();
    if let Some(name) = start {
        if let Some(s) = shell::Section::ALL.into_iter().find(|s| s.label() == name) {
            shell.go(s);
        }
    }
    let mut last = Instant::now();

    loop {
        term.draw(|f| shell.render(f))?;

        // Poll hard while something moves and lazily when nothing does. Over
        // SSH that is the difference between a quiet link and a steady trickle
        // of repaints for a screen that is not changing.
        let wait = if shell.animating() { 25 } else { 120 };
        if event::poll(Duration::from_millis(wait))? {
            // Drain the queue before drawing again: with any-motion tracking on,
            // a fast drag delivers dozens of events per frame and rendering each
            // one only adds latency.
            loop {
                match event::read()? {
                    Event::Key(k) => shell.on_key(k),
                    Event::Mouse(m) => shell.on_mouse(m),
                    _ => {}
                }
                if shell.quit || !event::poll(Duration::ZERO)? {
                    break;
                }
            }
        }
        if shell.quit {
            return Ok(());
        }

        // Clamped, so a stalled link plays an animation late rather than
        // teleporting it to the end.
        let now = Instant::now();
        let dt = now.duration_since(last).as_secs_f64().min(0.1);
        last = now;
        shell.tick(dt);
    }
}

fn setup() -> io::Result<Term> {
    enable_raw_mode()?;
    let mut out = io::stdout();
    execute!(out, EnterAlternateScreen, event::EnableMouseCapture, crossterm::cursor::Hide)?;
    // Crossterm's capture only reports motion while a button is held; 1003
    // reports it unconditionally, which is what hover needs.
    write!(out, "\x1b[?1003h")?;
    out.flush()?;
    Terminal::new(CrosstermBackend::new(out))
}

fn restore(term: &mut Term) -> io::Result<()> {
    let mut out = io::stdout();
    write!(out, "\x1b[?1003l")?;
    out.flush()?;
    execute!(out, crossterm::cursor::Show, event::DisableMouseCapture, LeaveAlternateScreen)?;
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
