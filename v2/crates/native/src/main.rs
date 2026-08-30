use std::{
    env,
    io::{self, Write},
};

use portfolio_v2_client_core::{Action, ClientState, Viewport};
use portfolio_v2_protocol::Bootstrap;
use portfolio_v2_scene::{Cell, CellSurface, Rgba8};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let endpoint =
        env::var("PORTFOLIO_V2_ENDPOINT").unwrap_or_else(|_| "http://127.0.0.1:8322".into());
    let bootstrap: Bootstrap = ureq::get(format!("{endpoint}/api/v2/bootstrap"))
        .call()?
        .body_mut()
        .read_json()?;
    bootstrap.validate()?;

    let cols = env::var("COLUMNS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(160);
    let rows = env::var("LINES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(45);
    let mut state = ClientState::default();
    state.update(Action::Resize(Viewport {
        width: cols as f32 * 8.0,
        height: rows as f32 * 17.0,
        scale: 1.0,
        cols,
        rows,
    }));
    state.update(Action::BootstrapLoaded(bootstrap));

    let mut renderer = AnsiRenderer::default();
    let output = renderer.render(&state.cells());
    let mut stdout = io::stdout().lock();
    stdout.write_all(b"\x1b[?25l\x1b[2J")?;
    stdout.write_all(output.as_bytes())?;
    stdout.flush()?;
    Ok(())
}

#[derive(Default)]
struct AnsiRenderer {
    previous: Option<CellSurface>,
}

impl AnsiRenderer {
    fn render(&mut self, surface: &CellSurface) -> String {
        let mut output = String::new();
        let mut style: Option<(Rgba8, Rgba8, bool)> = None;
        for y in 0..surface.rows {
            let mut x = 0;
            while x < surface.cols {
                let index = y as usize * surface.cols as usize + x as usize;
                let cell = &surface.cells[index];
                if self.previous.as_ref().and_then(|old| old.cells.get(index)) == Some(cell) {
                    x += 1;
                    continue;
                }
                output.push_str(&format!("\x1b[{};{}H", y + 1, x + 1));
                while x < surface.cols {
                    let index = y as usize * surface.cols as usize + x as usize;
                    let cell = &surface.cells[index];
                    if let Some(old) = self.previous.as_ref().and_then(|old| old.cells.get(index)) {
                        if old == cell {
                            break;
                        }
                    }
                    set_style(&mut output, &mut style, cell);
                    output.push(cell.glyph);
                    x += 1;
                }
            }
        }
        output.push_str("\x1b[0m");
        self.previous = Some(surface.clone());
        output
    }
}

fn set_style(output: &mut String, current: &mut Option<(Rgba8, Rgba8, bool)>, cell: &Cell) {
    let next = (cell.foreground, cell.background, cell.bold);
    if *current == Some(next) {
        return;
    }
    let Rgba8(fr, fg, fb, _) = cell.foreground;
    let Rgba8(br, bg, bb, _) = cell.background;
    output.push_str(&format!(
        "\x1b[{};38;2;{fr};{fg};{fb};48;2;{br};{bg};{bb}m",
        if cell.bold { 1 } else { 22 }
    ));
    *current = Some(next);
}
