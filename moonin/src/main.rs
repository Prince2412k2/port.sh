//! Playground for the library. Nothing here belongs in the host app — it exists
//! so the module can be driven and looked at on its own.
//!
//! It deliberately honours `moving()` in its own poll rate, the same way the
//! portfolio does, so the cost of a behaviour is visible here rather than only
//! once it is wired in.

use std::io::{self, Write};
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEventKind, MouseEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::Terminal;

use moonin::{Cue, Feeling, Mascot};

type Term = Terminal<CrosstermBackend<io::Stdout>>;

const LABEL: Color = Color::Rgb(96, 100, 112);
const STEP: f32 = 1.0 / 60.0;

fn main() -> io::Result<()> {
    if std::env::args().any(|a| a == "--sheet") {
        return sheet();
    }
    if std::env::args().any(|a| a == "--faces") {
        return faces();
    }
    if let Some(mode) = std::env::args().skip_while(|a| a != "--film").nth(1) {
        return film(&mode);
    }
    install_panic_hook();
    let mut term = setup()?;
    let result = run(&mut term);
    restore(&mut term)?;
    result
}

fn run(term: &mut Term) -> io::Result<()> {
    let mut mascot = Mascot::new(0x5eed);
    let mut debt = 0.0f32;
    let mut last = Instant::now();
    let mut quit = false;

    loop {
        let now = Instant::now();
        debt = (debt + (now - last).as_secs_f32()).min(0.25);
        last = now;
        while debt >= STEP {
            mascot.tick(STEP);
            debt -= STEP;
        }

        let mut state = String::new();
        term.draw(|frame| {
            let area = frame.area();
            let stage = Rect { height: area.height.saturating_sub(1), ..area };
            mascot.render(frame.buffer_mut(), stage);
            state = format!(
                "{}   move the mouse: near it walks, far it rolls, high it flies.  q quit",
                if mascot.rolling() {
                    "rolling"
                } else if mascot.flying() {
                    "flying"
                } else if mascot.asleep() {
                    "asleep"
                } else {
                    "grounded"
                },
            );
            note(frame.buffer_mut(), area, &state);
        })?;
        if quit {
            return Ok(());
        }

        // The same trade the host makes: poll hard while something moves, lazily
        // when nothing does.
        let budget = if mascot.moving() { 16 } else { 120 };
        if event::poll(Duration::from_millis(budget).saturating_sub(now.elapsed()))? {
            while event::poll(Duration::ZERO)? {
                match event::read()? {
                    Event::Key(k) if k.kind == KeyEventKind::Press => match k.code {
                        KeyCode::Char('q') | KeyCode::Esc => quit = true,
                        _ => {}
                    },
                    Event::Mouse(m) => {
                        if matches!(
                            m.kind,
                            MouseEventKind::Moved
                                | MouseEventKind::Down(_)
                                | MouseEventKind::Drag(_)
                        ) {
                            mascot.pointer(Some((m.column, m.row)));
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

fn note(buf: &mut Buffer, area: Rect, text: &str) {
    buf.set_string(
        area.x,
        area.bottom() - 1,
        text.chars().take(area.width as usize).collect::<String>(),
        Style::default().fg(LABEL),
    );
}

/// `--sheet` — the gaze aimed five ways, for checking the eye without a mouse.
/// Follow is switched off so the body holds still while only the look moves.
fn sheet() -> io::Result<()> {
    let (cols, rows) = (19u16, 11u16);
    let mid = (cols / 2, rows / 2);
    let looks: [(&str, i32, i32); 5] = [
        ("left", -12, 0),
        ("up", 0, -7),
        ("centre", 0, 0),
        ("down", 0, 7),
        ("right", 12, 0),
    ];

    let mut strips: Vec<Vec<String>> = Vec::new();
    for (_, dc, dr) in looks {
        let area = Rect::new(0, 0, cols, rows);
        let mut mascot = Mascot::new(0x5eed);
        mascot.skin_mut().follow = false;
        mascot.confine(area);
        // Settle first: it falls to the floor, so a target measured from the
        // middle of the area would not mean what it looks like it means.
        for _ in 0..90 {
            mascot.tick(STEP);
        }
        let seat = rows as i32 - 3;
        let at = (
            (mid.0 as i32 + dc).clamp(0, cols as i32 - 1) as u16,
            (seat + dr).clamp(0, rows as i32 - 1) as u16,
        );
        mascot.pointer(Some(at));
        for _ in 0..60 {
            mascot.tick(STEP);
        }
        let mut buf = Buffer::empty(area);
        mascot.render(&mut buf, area);
        strips.push(
            (0..rows)
                .map(|y| {
                    (0..cols)
                        .map(|x| buf.cell((x, y)).map_or(" ", |c| c.symbol()))
                        .collect()
                })
                .collect(),
        );
    }

    let mut out = io::stdout().lock();
    for y in 0..strips[0].len() {
        let row: Vec<String> = strips.iter().map(|s| s[y].clone()).collect();
        writeln!(out, "{}", row.join(" ").trim_end())?;
    }
    let tags: Vec<String> = looks.iter().map(|(n, _, _)| format!("{:<19}", n)).collect();
    writeln!(out, "{}", tags.join(" "))?;
    Ok(())
}

/// Renders one still, with `settle` frames of easing first.
fn still(dress: impl FnOnce(&mut Mascot), cols: u16, rows: u16) -> Vec<String> {
    let area = Rect::new(0, 0, cols, rows);
    let mut mascot = Mascot::new(0x5eed);
    mascot.skin_mut().follow = false;
    mascot.confine(area);
    dress(&mut mascot);
    for _ in 0..60 {
        mascot.tick(STEP);
    }
    let mut buf = Buffer::empty(area);
    mascot.render(&mut buf, area);
    (0..rows)
        .map(|y| {
            (0..cols)
                .map(|x| buf.cell((x, y)).map_or(" ", |c| c.symbol()))
                .collect()
        })
        .collect()
}

fn band(strips: &[Vec<String>], names: &[&str], cols: u16) -> io::Result<()> {
    let mut out = io::stdout().lock();
    for y in 0..strips[0].len() {
        let row: Vec<String> = strips.iter().map(|s| s[y].clone()).collect();
        writeln!(out, "{}", row.join(" ").trim_end())?;
    }
    let tags: Vec<String> = names
        .iter()
        .map(|n| format!("{:<width$}", n, width = cols as usize))
        .collect();
    writeln!(out, "{}\n", tags.join(" "))
}

/// `--faces` — the expressions, then the costumes. The point of the sheet is to
/// answer one question: does it read as feeling anything?
fn faces() -> io::Result<()> {
    let (cols, rows) = (19u16, 11u16);
    let moods: [(&str, Feeling); 7] = [
        ("blank", Feeling::BLANK),
        ("curious", Feeling::CURIOUS),
        ("pleased", Feeling::PLEASED),
        ("worried", Feeling::WORRIED),
        ("cross", Feeling::CROSS),
        ("weary", Feeling::WEARY),
        ("swamped", Feeling::SWAMPED),
    ];
    let strips: Vec<Vec<String>> = moods
        .iter()
        .map(|(_, f)| still(|m| m.cue(Cue::Feel(*f)), cols, rows))
        .collect();
    let names: Vec<&str> = moods.iter().map(|(n, _)| *n).collect();
    band(&strips, &names, cols)?;

    band(&strips, &names, cols)
}

/// `--film roll|walk|fly` — frames sampled through one behaviour, so the motion
/// can be judged without holding a terminal open. Reading a still tells you
/// nothing about whether a roll looks like a roll.
fn film(mode: &str) -> io::Result<()> {
    // Wide, for the gaits that are supposed to cover ground.
    let (cols, rows) = if mode == "fly" { (22u16, 11) } else { (70u16, 11) };
    let area = Rect::new(0, 0, cols, rows);
    let mut mascot = Mascot::new(0x5eed);
    mascot.confine(area);

    // Where the pointer sits, and how long to run before sampling starts.
    let (at, warm, gap) = match mode {
        "fly" => ((cols / 2, 0), 150, 6),
        // Inside the roll threshold, so this actually films a walk.
        "walk" => ((cols / 2 + 12, rows - 2), 6, 18),
        _ => ((cols - 2, rows - 2), 6, 18),
    };
    for _ in 0..warm {
        mascot.pointer(Some(at));
        mascot.tick(STEP);
    }

    let mut strips = Vec::new();
    let mut trace = Vec::new();
    for _ in 0..7 {
        for _ in 0..gap {
            mascot.pointer(Some(at));
            mascot.tick(STEP);
        }
        trace.push(mascot.at().0);
        let mut buf = Buffer::empty(area);
        mascot.render(&mut buf, area);
        strips.push(
            (0..rows)
                .map(|y| {
                    (0..cols)
                        .map(|x| buf.cell((x, y)).map_or(" ", |c| c.symbol()))
                        .collect::<String>()
                })
                .collect::<Vec<_>>(),
        );
    }
    let mut out = io::stdout().lock();
    if mode == "fly" {
        for y in 0..strips[0].len() {
            let row: Vec<String> = strips.iter().map(|s| s[y].clone()).collect();
            writeln!(out, "{}", row.join(" ").trim_end())?;
        }
    } else {
        // Stacked, because a gait meant to cover ground has to be judged against
        // ground rather than against itself.
        for strip in &strips {
            for line in strip.iter().filter(|l| !l.trim().is_empty()) {
                writeln!(out, "{}", line.trim_end())?;
            }
            writeln!(out)?;
        }
    }
    let step = gap as f32 / 60.0;
    let pace: Vec<String> = trace
        .windows(2)
        .map(|w| format!("{:>4.0}", (w[1] as f32 - w[0] as f32) / step))
        .collect();
    writeln!(out, "col: {:?}", trace)?;
    writeln!(out, "cells/sec:{}", pace.join(""))
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
    // Crossterm's capture only reports motion while a button is held; 1003
    // reports it unconditionally, which is the entire input here.
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
