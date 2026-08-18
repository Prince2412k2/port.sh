//! Per-layer drawing style, and the depth model.
//!
//! Depth runs 0 (nearest, brightest) to 1 (farthest, dimmest). In a flat 2D map
//! there is no camera, so depth is assigned by *importance*: a motorway is
//! "close", a park boundary is "far away". Layering the focus falloff on top
//! makes it interactive -- whatever the mouse is near comes forward.

use crate::data::Layer;

#[derive(Clone, Copy)]
pub struct LayerStyle {
    pub depth: f32,
    pub width: f64,
    pub alpha: f32,
    /// 0 means "not a fill"; otherwise 1..64 dither density.
    pub density: u8,
    pub dash: Option<(f64, f64)>,
    pub tint: u8,
    /// Glyph family: solid quadrants for things you trace (roads, coastline),
    /// braille stipple for things you read as ground (water, landuse).
    pub mat: u8,
    /// Below this zoom the layer is dropped entirely, to keep wide views legible.
    pub min_zoom: f64,
}

pub fn style(layer: Layer) -> LayerStyle {
    use crate::canvas::{
        MAT_DOT, MAT_SOLID, TINT_BORDER, TINT_COAST, TINT_GREEN, TINT_LANDMARK, TINT_MAJOR, TINT_MEDIUM,
        TINT_MINOR, TINT_MONO, TINT_RAIL, TINT_WATER,
    };
    match layer {
        Layer::Landuse => LayerStyle {
            depth: 0.85,
            width: 1.0,
            alpha: 0.70,
            density: 2,
            dash: None,
            tint: TINT_GREEN,
            min_zoom: 8.0,
            mat: MAT_DOT,
        },
        Layer::Water => LayerStyle {
            depth: 0.55,
            width: 1.0,
            alpha: 0.90,
            density: 2,
            dash: None,
            tint: TINT_WATER,
            min_zoom: 0.0,
            mat: MAT_DOT,
        },
        Layer::Coast => LayerStyle {
            depth: 0.28,
            width: 1.8,
            alpha: 1.0,
            density: 0,
            dash: None,
            tint: TINT_COAST,
            min_zoom: 0.0,
            mat: MAT_SOLID,
        },
        Layer::Rail => LayerStyle {
            depth: 0.48,
            width: 1.0,
            alpha: 0.85,
            density: 0,
            dash: Some((3.0, 3.0)),
            tint: TINT_RAIL,
            min_zoom: 10.5,
            mat: MAT_SOLID,
        },
        Layer::RoadMinor => LayerStyle {
            depth: 0.80,
            width: 1.0,
            alpha: 0.32,
            density: 0,
            dash: None,
            tint: TINT_MINOR,
            min_zoom: 13.5,
            mat: MAT_SOLID,
        },
        Layer::RoadMedium => LayerStyle {
            depth: 0.62,
            width: 1.5,
            alpha: 0.70,
            density: 0,
            dash: None,
            tint: TINT_MEDIUM,
            min_zoom: 11.5,
            mat: MAT_SOLID,
        },
        Layer::RoadMajor => LayerStyle {
            depth: 0.12,
            width: 2.6,
            alpha: 1.0,
            density: 0,
            dash: None,
            tint: TINT_MAJOR,
            min_zoom: 0.0,
            mat: MAT_SOLID,
        },
        Layer::Place => LayerStyle {
            depth: 0.10,
            width: 1.0,
            alpha: 1.0,
            density: 0,
            dash: None,
            tint: TINT_MONO,
            // Zero, not a city-scale floor: state and country names are the
            // whole point of a national view.
            min_zoom: 0.0,
            mat: MAT_DOT,
        },
        // Dashed, like every printed map's convention for an administrative
        // line, and mid-depth so it sits behind roads but ahead of terrain.
        Layer::Boundary => LayerStyle {
            depth: 0.20,
            width: 1.2,
            alpha: 1.0,
            density: 0,
            dash: Some((4.0, 3.0)),
            tint: TINT_BORDER,
            mat: MAT_DOT,
            min_zoom: 0.0,
        },
        // Extruded by `scene::draw_buildings`, never by the generic path.
        Layer::Building => LayerStyle {
            depth: 0.30,
            width: 1.0,
            alpha: 1.0,
            density: 0,
            dash: None,
            tint: TINT_MONO,
            mat: MAT_DOT,
            min_zoom: 14.0,
        },
        // Never drawn directly; the fields only matter so the table is total.
        Layer::Land => LayerStyle {
            depth: 0.99,
            width: 1.0,
            alpha: 0.0,
            density: 0,
            dash: None,
            tint: TINT_MONO,
            min_zoom: 0.0,
            mat: MAT_DOT,
        },
        Layer::Landmark => LayerStyle {
            depth: 0.05,
            width: 1.0,
            alpha: 1.0,
            density: 0,
            dash: None,
            tint: TINT_LANDMARK,
            min_zoom: 10.5,
            mat: MAT_DOT,
        },
    }
}

/// Interactive depth-of-field. Off is a plain layered map; the other two pull
/// whatever is near the cursor forward and push the rest back.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FocusMode {
    Off,
    Subtle,
    Strong,
}

impl FocusMode {
    pub fn next(self) -> Self {
        match self {
            FocusMode::Off => FocusMode::Subtle,
            FocusMode::Subtle => FocusMode::Strong,
            FocusMode::Strong => FocusMode::Off,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            FocusMode::Off => "off",
            FocusMode::Subtle => "subtle",
            FocusMode::Strong => "strong",
        }
    }

    fn strength(self) -> f32 {
        match self {
            FocusMode::Off => 0.0,
            FocusMode::Subtle => 0.20,
            FocusMode::Strong => 0.62,
        }
    }
}

/// Everything needed to turn a base layer depth into the depth actually used
/// for a given point on screen.
pub struct DepthField {
    pub mode: FocusMode,
    /// Focus point in subpixel coords.
    pub focus: [f64; 2],
    /// Distance at which the falloff has fully taken hold.
    pub radius: f64,
}

impl DepthField {
    #[inline]
    pub fn at(&self, base: f32, p: [f64; 2]) -> f32 {
        let s = self.mode.strength();
        if s == 0.0 {
            return base;
        }
        let dx = p[0] - self.focus[0];
        let dy = p[1] - self.focus[1];
        let d = ((dx * dx + dy * dy).sqrt() / self.radius).min(1.0) as f32;
        // Smoothstep, so the near field is a soft pool rather than a hard disc.
        let t = d * d * (3.0 - 2.0 * d);
        (base + s * t - s * 0.25).clamp(0.0, 1.0)
    }
}

/// Within a layer, higher-ranked features draw a little heavier. This is what
/// keeps a primary road visually ahead of a secondary one without giving each
/// OSM class its own layer.
pub fn rank_weight(rank: u16) -> f32 {
    (0.72 + 0.28 * (rank as f32 - 80.0) / 140.0).clamp(0.65, 1.0)
}
