//! An interactive portfolio, served over SSH.

mod acp;
mod about;
mod boot;
mod ask;
mod cert;
mod coffee;
mod context;
mod emblems;
mod gates;
mod health;
mod home;
mod json;
mod mcp;
mod museum;
mod net;
mod paint;
mod portraits;
mod reach;
mod servers;
mod session;
mod shell;
mod web;
mod wire;
mod taste;
mod visits;
mod snapshot;

use std::io::{self, IsTerminal, Write};
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
    let mut serve = false;
    let mut web = false;
    let mut web_addr = "0.0.0.0".to_string();
    let mut web_port: u16 = 8080;
    let mut ssh_addr = "0.0.0.0".to_string();
    let mut ssh_port: u16 = 2222;
    let mut host_key: Option<String> = None;

    while let Some(a) = args.next() {
        match a.as_str() {
            "-h" | "--help" => {
                println!("portfolio -- an interactive CV in a terminal\n");
                println!("  --section NAME   open on home | experience | projects | skills |");
                println!("                   taste | ask");
                println!("  --snapshot WxH   draw one frame to stdout and exit");
                println!("  --plain          snapshot without colour");
                println!("  --at SECONDS     how far into the section's animation to draw");
                println!("  --probe          check which agent tier is answering, and exit");
                println!();
                println!("  --serve                  run the SSH server instead of a local terminal");
                println!("  --ssh-addr ADDR          bind address for --serve (default 0.0.0.0)");
                println!("  --ssh-port PORT          bind port for --serve (default 2222)");
                println!("  --host-key PATH          where the SSH host key lives, generated on first run");
                println!("                           (default $PORTFOLIO_HOST_KEY or ./data/ssh_host_key)");
                println!();
                println!("  --web                    run the web terminal instead (same app, no shell)");
                println!("  --web-addr ADDR          bind address for --web (default 0.0.0.0)");
                println!("  --web-port PORT          bind port for --web (default 8080)");
                println!();
                println!("Environment:");
                println!("  TERMAP_DATA              where the basemap and heightmap live");
                println!("  PORTFOLIO_HOST_KEY       the SSH host key, generated on first run");
                println!("  PORTFOLIO_IDLE_SECS      idle timeout, 0 to disable (default 900)");
                println!("  PORTFOLIO_MESSAGES       where `/reach` appends (default data/messages.jsonl)");
                println!();
                println!("The agent behind `ask` is `opencode acp`, pinned to the first model in");
                println!("data/models.txt that will answer. It needs that provider's API key in");
                println!("the environment; without one, every other section still works.");
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
            "--probe" => {
                // One pass of the hourly check, printed. The same code the
                // server runs, so this answers "which tier is up right now"
                // rather than "which tier does the file list first".
                // The gates first. They are compiled in, so this is the only way
                // to read the running binary's policy rather than the policy in
                // whatever source tree happens to be checked out.
                println!("gates");
                for t in gates::TOOLS {
                    println!("  tool  {:<12} {}", t.name, if t.open { "on" } else { "off" });
                }
                for (cap, open) in gates::capabilities() {
                    println!("  acp   {:<12} {}", cap, if open { "on" } else { "off" });
                }
                println!("  budget  {} questions, {} tool calls", gates::GATES.turns, gates::GATES.tool_calls);
                println!("  advertised  {}", gates::client_capabilities());
                println!();

                for t in health::tiers() {
                    println!("tier {}  via {}", t.name, t.server.label());
                    for m in &t.models {
                        println!("  {m}");
                    }
                }
                println!();
                health::check();
                match health::note() {
                    Some(t) => println!("using: {t}"),
                    None => println!("using: nothing -- no tier answered"),
                }
                return Ok(());
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
            "--serve" => serve = true,
            "--web" => web = true,
            "--web-addr" => web_addr = args.next().unwrap_or(web_addr),
            "--web-port" => web_port = args.next().and_then(|v| v.parse().ok()).unwrap_or(web_port),
            "--ssh-addr" => ssh_addr = args.next().unwrap_or(ssh_addr),
            "--ssh-port" => ssh_port = args.next().and_then(|v| v.parse().ok()).unwrap_or(ssh_port),
            "--host-key" => host_key = args.next(),
            "--section" => start = args.next(),
            _ => {}
        }
    }

    if let Some(mut o) = shot {
        o.section = start.or(o.section);
        return snapshot::render(&o);
    }

    if web {
        visits::boot();
        health::watch();
        // Both at boot rather than on the first visit.
        //
        // They were started from `ask::wake`, which is late enough to lose a
        // race that showed up in a real log: `place index ready` printed *after*
        // `offering our tools`, so a visitor who asked about a place in that
        // first second would have had `locate_place` answer `found:false` and
        // been told the box could not place a city it knows perfectly well.
        // Idempotent, so the call in `wake` stays as the belt to this braces.
        mcp::serve();
        mcp::warm_index();
        let rt = tokio::runtime::Runtime::new()?;
        return rt
            .block_on(web::serve(&web_addr, web_port))
            .map_err(|e| io::Error::other(e.to_string()));
    }

    if serve {
        let path = host_key
            .or_else(|| std::env::var("PORTFOLIO_HOST_KEY").ok())
            .unwrap_or_else(|| "data/ssh_host_key".to_string());
        visits::boot();
        health::watch();
        // Both at boot rather than on the first visit.
        //
        // They were started from `ask::wake`, which is late enough to lose a
        // race that showed up in a real log: `place index ready` printed *after*
        // `offering our tools`, so a visitor who asked about a place in that
        // first second would have had `locate_place` answer `found:false` and
        // been told the box could not place a city it knows perfectly well.
        // Idempotent, so the call in `wake` stays as the belt to this braces.
        mcp::serve();
        mcp::warm_index();
        let rt = tokio::runtime::Runtime::new()?;
        return rt
            .block_on(net::serve(&ssh_addr, ssh_port, std::path::Path::new(&path)))
            .map_err(|e| io::Error::other(e.to_string()));
    }

    // No terminal on the other end: `ssh host | cat`, a health check, a script.
    // Raw mode would fail and the error would be the only thing they ever saw.
    if !io::stdout().is_terminal() {
        return plain_text();
    }

    let mut term = setup()?;
    install_panic_hook();
    let result = run(&mut term, start);
    restore(&mut term)?;
    result
}

/// How long a session may sit with nobody touching it.
///
/// sshd's `ClientAliveInterval` only catches a client that has stopped
/// answering; a laptop left open on the taste page answers keepalives all
/// afternoon and holds a session slot the whole time. Overridable, and off
/// entirely with 0, because on a local terminal this is just rude.
fn idle_limit() -> Duration {
    let secs = std::env::var("PORTFOLIO_IDLE_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(900);
    Duration::from_secs(secs)
}

fn run(term: &mut Term, start: Option<String>) -> io::Result<()> {
    let mut shell = Shell::new();
    let idle_after = idle_limit();
    let mut last_input = Instant::now();
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
        if event::poll(Duration::from_millis(shell.frame_ms()))? {
            // Drain the queue before drawing again: with any-motion tracking on,
            // a fast drag delivers dozens of events per frame and rendering each
            // one only adds latency.
            loop {
                match event::read()? {
                    Event::Key(k) => shell.on_key(k),
                    Event::Mouse(m) => shell.on_mouse(m),
                    _ => {}
                }
                last_input = Instant::now();
                if shell.quit || !event::poll(Duration::ZERO)? {
                    break;
                }
            }
        }
        if shell.quit {
            return Ok(());
        }
        if !idle_after.is_zero() && last_input.elapsed() > idle_after {
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

/// What comes out when there is nowhere to draw.
///
/// Not an error and not nothing: this is a CV, and a CV that only exists
/// inside an animation is a CV that cannot be piped, grepped, or read by
/// anything that is not a person at a terminal.
fn plain_text() -> io::Result<()> {
    let a = about::load();
    let mut out = io::stdout().lock();
    writeln!(out, "{}", a.name)?;
    writeln!(out, "{}   {}", a.role, a.where_)?;
    writeln!(out)?;
    for line in paint::wrap(&a.pitch, 72) {
        writeln!(out, "{line}")?;
    }
    writeln!(out)?;
    for line in paint::wrap(&a.now, 72) {
        writeln!(out, "{line}")?;
    }
    writeln!(out)?;
    for s in [&a.github, &a.email].iter().filter(|s| !s.is_empty()) {
        writeln!(out, "{s}")?;
    }
    writeln!(out)?;
    writeln!(out, "This is the plain-text version. The interactive one needs a terminal:")?;
    // Built on about.txt's own ssh line rather than a second hardcoded one --
    // the port is a deployment detail, and one copy of it is one that can go
    // stale instead of two that can quietly disagree.
    writeln!(out, "  {}", a.ssh.replacen("ssh ", "ssh -t ", 1))?;
    Ok(())
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
