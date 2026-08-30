use serde::Serialize;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct LogicalViewport {
    pub cols: u16,
    pub rows: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum PaletteRole {
    Page,
    Ink,
    Faint,
    Ghost,
    Amber,
    Cyan,
    Certificate,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub enum Theme {
    #[default]
    Dark,
    Light,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct VisualScene {
    pub viewport: LogicalViewport,
    pub theme: Theme,
    pub primitives: Vec<Primitive>,
    pub hits: Vec<HitRegion>,
    pub details: Vec<Detail>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RenderPackage {
    #[default]
    Canonical,
    Crt,
    Vhs,
    Ink,
}

impl RenderPackage {
    pub fn next(self) -> Self {
        match self {
            Self::Canonical => Self::Crt,
            Self::Crt => Self::Vhs,
            Self::Vhs => Self::Ink,
            Self::Ink => Self::Canonical,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ColorMode {
    #[default]
    Color,
    Monochrome,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct RenderVariant {
    pub package: RenderPackage,
    pub color: ColorMode,
    pub reduced_motion: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RenderFrame {
    pub variant: RenderVariant,
    pub fallback: CellSurface,
    pub details: Vec<Detail>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DetailClass {
    MapGeometry,
    MapLabel,
    MapMarker,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Detail {
    pub id: String,
    pub class: DetailClass,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub enum Primitive {
    GlyphRun(GlyphRun),
    CellArt(CellArt),
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct GlyphRun {
    pub x: u16,
    pub y: u16,
    pub text: String,
    pub foreground: PaletteRole,
    pub bold: bool,
    /// Zero is screen/UI content; map details use one-based `VisualScene::details` indices.
    pub detail: u16,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CellArt {
    pub x: u16,
    pub y: u16,
    pub cols: u16,
    pub rows: u16,
    pub cells: Vec<ArtCell>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct ArtCell {
    pub glyph: char,
    pub foreground: Rgba8,
    pub background: Option<Rgba8>,
    pub bold: bool,
    pub detail: u16,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct HitRegion {
    pub id: String,
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Rgba8(pub u8, pub u8, pub u8, pub u8);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct MaterialId(pub u16);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct LayerId(pub u16);

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Cell {
    pub glyph: char,
    pub foreground: Rgba8,
    pub background: Rgba8,
    pub material: MaterialId,
    pub layer: LayerId,
    pub depth: u16,
    pub bold: bool,
    pub detail: u16,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CellSurface {
    pub cols: u16,
    pub rows: u16,
    pub cells: Vec<Cell>,
}

pub fn compose(scene: &VisualScene) -> CellSurface {
    let palette = Palette::new(scene.theme);
    let mut surface = CellSurface {
        cols: scene.viewport.cols,
        rows: scene.viewport.rows,
        cells: vec![
            Cell {
                glyph: ' ',
                foreground: palette.ink,
                background: palette.page,
                material: MaterialId(0),
                layer: LayerId(0),
                depth: u16::MAX,
                bold: false,
                detail: 0,
            };
            scene.viewport.cols as usize * scene.viewport.rows as usize
        ],
    };

    for primitive in &scene.primitives {
        match primitive {
            Primitive::GlyphRun(run) => {
                for (offset, glyph) in run.text.chars().enumerate() {
                    if let Some(cell) = surface.at_mut(run.x.saturating_add(offset as u16), run.y) {
                        cell.glyph = glyph;
                        cell.foreground = palette.role(run.foreground);
                        cell.bold = run.bold;
                        cell.detail = run.detail;
                        cell.layer = LayerId(2);
                        cell.depth = 0;
                    }
                }
            }
            Primitive::CellArt(art) => {
                for (offset, source) in art.cells.iter().enumerate() {
                    let x = art.x.saturating_add(offset as u16 % art.cols);
                    let y = art.y.saturating_add(offset as u16 / art.cols);
                    if let Some(cell) = surface.at_mut(x, y) {
                        cell.glyph = source.glyph;
                        cell.foreground = source.foreground;
                        if let Some(background) = source.background {
                            cell.background = background;
                        }
                        cell.bold = source.bold;
                        cell.detail = source.detail;
                        cell.layer = LayerId(1);
                        cell.depth = 1;
                    }
                }
            }
        }
    }
    surface
}

impl CellSurface {
    fn at_mut(&mut self, x: u16, y: u16) -> Option<&mut Cell> {
        if x >= self.cols || y >= self.rows {
            return None;
        }
        self.cells
            .get_mut(y as usize * self.cols as usize + x as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compose_preserves_one_based_detail_attribution() {
        let scene = VisualScene {
            viewport: LogicalViewport { cols: 2, rows: 1 },
            theme: Theme::Dark,
            primitives: vec![Primitive::GlyphRun(GlyphRun {
                x: 0,
                y: 0,
                text: "ab".into(),
                foreground: PaletteRole::Ink,
                bold: false,
                detail: 1,
            })],
            hits: Vec::new(),
            details: vec![Detail {
                id: "map-label:station".into(),
                class: DetailClass::MapLabel,
            }],
        };
        let surface = compose(&scene);
        assert_eq!(surface.cells[0].detail, 1);
        assert_eq!(surface.cells[1].detail, 1);
        assert_eq!(scene.details[0].id, "map-label:station");
    }
}

#[derive(Clone, Copy)]
struct Palette {
    page: Rgba8,
    ink: Rgba8,
    faint: Rgba8,
    ghost: Rgba8,
    amber: Rgba8,
    cyan: Rgba8,
    certificate: Rgba8,
}

impl Palette {
    fn new(theme: Theme) -> Self {
        match theme {
            Theme::Dark => Self {
                page: rgb(8, 9, 11),
                ink: rgb(232, 232, 226),
                faint: rgb(97, 97, 95),
                ghost: rgb(60, 60, 59),
                amber: rgb(255, 176, 64),
                cyan: rgb(110, 224, 255),
                certificate: rgb(217, 119, 87),
            },
            Theme::Light => Self {
                page: rgb(238, 234, 224),
                ink: rgb(34, 32, 30),
                faint: rgb(152, 149, 143),
                ghost: rgb(185, 181, 173),
                amber: rgb(168, 96, 8),
                cyan: rgb(0, 104, 140),
                certificate: rgb(174, 74, 48),
            },
        }
    }

    fn role(self, role: PaletteRole) -> Rgba8 {
        match role {
            PaletteRole::Page => self.page,
            PaletteRole::Ink => self.ink,
            PaletteRole::Faint => self.faint,
            PaletteRole::Ghost => self.ghost,
            PaletteRole::Amber => self.amber,
            PaletteRole::Cyan => self.cyan,
            PaletteRole::Certificate => self.certificate,
        }
    }
}

const fn rgb(r: u8, g: u8, b: u8) -> Rgba8 {
    Rgba8(r, g, b, 255)
}
