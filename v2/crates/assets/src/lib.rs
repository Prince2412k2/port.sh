use portfolio_v2_scene::{ArtCell, CellArt, Rgba8};

#[path = "../../../../portfolio/src/portraits.rs"]
#[allow(dead_code)]
mod baked;

pub fn home_portrait(max_cols: u16, max_rows: u16, x: u16, y: u16) -> Option<CellArt> {
    let portrait = baked::PORTRAITS
        .iter()
        .filter(|portrait| portrait.id == "snufkin-home")
        .filter(|portrait| portrait.cols <= max_cols && portrait.rows <= max_rows)
        .max_by_key(|portrait| portrait.cols * portrait.rows)?;
    let frame = portrait.frames.last()?;
    let cells = frame
        .iter()
        .map(|&(glyph, foreground, background)| ArtCell {
            glyph,
            foreground: rgba(foreground),
            background: (!is_ground(glyph, background)).then(|| rgba(background)),
            bold: false,
            detail: 0,
        })
        .collect();
    Some(CellArt {
        x,
        y,
        cols: portrait.cols,
        rows: portrait.rows,
        cells,
    })
}

fn is_ground(glyph: char, background: baked::Ink) -> bool {
    if glyph != ' ' && glyph != '\u{2800}' {
        return false;
    }
    match background {
        baked::Ink::C(r, g, b) => (r, g, b) == baked::BAKED_BG,
        baked::Ink::I(index) => matches!(index, 0 | 16 | 232 | 233),
    }
}

fn rgba(ink: baked::Ink) -> Rgba8 {
    let (r, g, b) = match ink {
        baked::Ink::C(r, g, b) => (r, g, b),
        baked::Ink::I(index) => xterm(index),
    };
    Rgba8(r, g, b, 255)
}

fn xterm(index: u8) -> (u8, u8, u8) {
    const BASE: [(u8, u8, u8); 16] = [
        (0, 0, 0),
        (205, 0, 0),
        (0, 205, 0),
        (205, 205, 0),
        (0, 0, 238),
        (205, 0, 205),
        (0, 205, 205),
        (229, 229, 229),
        (127, 127, 127),
        (255, 0, 0),
        (0, 255, 0),
        (255, 255, 0),
        (92, 92, 255),
        (255, 0, 255),
        (0, 255, 255),
        (255, 255, 255),
    ];
    match index {
        0..=15 => BASE[index as usize],
        16..=231 => {
            let n = index - 16;
            let component = |value: u8| if value == 0 { 0 } else { 55 + value * 40 };
            (component(n / 36), component((n / 6) % 6), component(n % 6))
        }
        _ => {
            let value = 8 + (index - 232) * 10;
            (value, value, value)
        }
    }
}
