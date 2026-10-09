use super::{env, Cell, CellSurface, Rgba8};

pub struct AnsiRenderer {
    previous: Option<CellSurface>,
    colors: u16,
    ascii: bool,
}

impl AnsiRenderer {
    pub fn from_env() -> Self {
        let color = env::var("PORTFOLIO_V2_COLOR").unwrap_or_else(|_| {
            if env::var("COLORTERM").is_ok_and(|v| v == "truecolor" || v == "24bit") {
                "truecolor".into()
            } else if env::var("TERM").is_ok_and(|v| v.contains("256color")) {
                "256".into()
            } else {
                "16".into()
            }
        });
        Self {
            previous: None,
            colors: match color.as_str() {
                "16" => 16,
                "256" => 256,
                _ => 0,
            },
            ascii: env::var_os("PORTFOLIO_V2_ASCII").is_some(),
        }
    }

    pub fn render(&mut self, surface: &CellSurface) -> String {
        let mut output = String::new();
        if self
            .previous
            .as_ref()
            .is_none_or(|old| (old.cols, old.rows) != (surface.cols, surface.rows))
        {
            self.previous = None;
            output.push_str("\x1b[2J");
        }
        let mut style = None;
        for y in 0..surface.rows {
            let mut x = 0;
            while x < surface.cols {
                let index = y as usize * surface.cols as usize + x as usize;
                if self.previous.as_ref().and_then(|old| old.cells.get(index))
                    == Some(&surface.cells[index])
                {
                    x += 1;
                    continue;
                }
                output.push_str(&format!("\x1b[{};{}H", y + 1, x + 1));
                while x < surface.cols {
                    let index = y as usize * surface.cols as usize + x as usize;
                    let cell = &surface.cells[index];
                    if self.previous.as_ref().and_then(|old| old.cells.get(index)) == Some(cell) {
                        break;
                    }
                    self.set_style(&mut output, &mut style, cell);
                    use unicode_width::UnicodeWidthChar;
                    output.push(if cell.glyph.is_control() {
                        ' '
                    } else if self.ascii {
                        ascii(cell.glyph)
                    } else if cell.glyph.width() != Some(1) {
                        '?'
                    } else {
                        cell.glyph
                    });
                    x += 1;
                }
            }
        }
        if !output.is_empty() {
            output.push_str("\x1b[0m");
        }
        self.previous = Some(surface.clone());
        output
    }

    fn set_style(
        &self,
        output: &mut String,
        current: &mut Option<(Rgba8, Rgba8, bool)>,
        cell: &Cell,
    ) {
        let next = (cell.foreground, cell.background, cell.bold);
        if *current == Some(next) {
            return;
        }
        output.push_str(if cell.bold { "\x1b[1m" } else { "\x1b[22m" });
        for (rgb, foreground) in [(cell.foreground, true), (cell.background, false)] {
            let Rgba8(r, g, b, _) = rgb;
            let prefix = if foreground { 38 } else { 48 };
            if self.colors == 0 {
                output.push_str(&format!("\x1b[{prefix};2;{r};{g};{b}m"));
            } else {
                let index = quantize(rgb, self.colors);
                if self.colors == 256 {
                    output.push_str(&format!("\x1b[{prefix};5;{index}m"));
                } else {
                    let code = if foreground { 30 } else { 40 }
                        + index % 8
                        + if index >= 8 { 60 } else { 0 };
                    output.push_str(&format!("\x1b[{code}m"));
                }
            }
        }
        *current = Some(next);
    }
}

fn ascii(glyph: char) -> char {
    if glyph.is_ascii() && !glyph.is_control() {
        return glyph;
    }
    match glyph {
        '─' | '━' | '—' | '–' => '-',
        '│' | '┃' => '|',
        '·' | '•' | '⋅' => '.',
        ' ' | '\u{2800}' => ' ',
        '›' | '→' => '>',
        '←' | '‹' => '<',
        _ => '+',
    }
}

fn quantize(Rgba8(r, g, b, _): Rgba8, count: u16) -> u16 {
    const BASIC: [[u8; 3]; 16] = [
        [0, 0, 0],
        [128, 0, 0],
        [0, 128, 0],
        [128, 128, 0],
        [0, 0, 128],
        [128, 0, 128],
        [0, 128, 128],
        [192, 192, 192],
        [128, 128, 128],
        [255, 0, 0],
        [0, 255, 0],
        [255, 255, 0],
        [0, 0, 255],
        [255, 0, 255],
        [0, 255, 255],
        [255, 255, 255],
    ];
    (0..count)
        .min_by_key(|&index| {
            let color = if index < 16 {
                BASIC[index as usize]
            } else if index >= 232 {
                [8 + (index - 232) as u8 * 10; 3]
            } else {
                let n = index - 16;
                let ramp = [0, 95, 135, 175, 215, 255];
                [
                    ramp[(n / 36) as usize],
                    ramp[(n / 6 % 6) as usize],
                    ramp[(n % 6) as usize],
                ]
            };
            (r as i32 - color[0] as i32).pow(2)
                + (g as i32 - color[1] as i32).pow(2)
                + (b as i32 - color[2] as i32).pow(2)
        })
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resize_invalidates_diff_and_idle_emits_nothing() {
        let mut renderer = AnsiRenderer {
            previous: None,
            colors: 0,
            ascii: false,
        };
        let mut state = portfolio_v2_client_core::ClientState::default();
        let frame = state.cells();
        assert!(renderer.render(&frame).contains("\x1b[2J"));
        assert!(renderer.render(&frame).is_empty());
        let mut viewport = state.viewport;
        viewport.cols -= 1;
        state.update(portfolio_v2_client_core::Action::Resize(viewport));
        assert!(renderer.render(&state.cells()).contains("\x1b[2J"));
    }
}
