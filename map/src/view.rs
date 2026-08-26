//! What the map should look like at a given zoom.
//!
//! A single renderer used at every scale ends up wrong at most of them: the
//! settings that make a street corner read as three-dimensional turn a view of
//! a whole state into noise. So the mode is a function of zoom, and it decides
//! how much geometry survives, whether the camera leans, and whether the world
//! has height.

use crate::data::Layer;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// Country to region. Flat, sparse, no terrain: a reference map.
    Flat,
    /// City scale. Ground relief and a slight lean.
    Relief,
    /// Neighbourhood. Building masses begin.
    Half3D,
    /// Street. Full extrusion and perspective.
    Full3D,
}

impl Mode {
    pub fn of(zoom: f64) -> Mode {
        match zoom {
            z if z < 10.0 => Mode::Flat,
            z if z < 14.0 => Mode::Relief,
            z if z < 15.5 => Mode::Half3D,
            _ => Mode::Full3D,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Mode::Flat => "FLAT",
            Mode::Relief => "RELIEF",
            Mode::Half3D => "2.5D",
            Mode::Full3D => "3D",
        }
    }

    pub fn terrain(self) -> bool {
        !matches!(self, Mode::Flat)
    }

    pub fn buildings(self) -> bool {
        matches!(self, Mode::Half3D | Mode::Full3D)
    }
}

/// Camera lean for a zoom, when the view is left on automatic.
///
/// Ramped rather than switched: the world rising into three dimensions as you
/// zoom is the point, and a hard cut at a zoom boundary would throw that away.
pub fn auto_tilt(zoom: f64) -> f64 {
    let t = match zoom {
        z if z < 10.0 => 0.0,
        z if z < 14.0 => (z - 10.0) / 4.0 * 24.0,
        z if z < 15.5 => 24.0 + (z - 14.0) / 1.5 * 20.0,
        z => (44.0 + (z - 15.5) * 4.0).min(58.0),
    };
    t.to_radians()
}

/// Perspective strength for a zoom. Held at zero until there is vertical
/// structure for convergence to act on -- at region scale it only distorts.
pub fn auto_persp(zoom: f64) -> f64 {
    match zoom {
        z if z < 14.0 => 0.0,
        z if z < 15.5 => (z - 14.0) / 1.5 * 0.45,
        z => (0.45 + (z - 15.5) * 0.12).min(0.85),
    }
}

/// Minimum feature rank drawn, per layer and zoom.
///
/// The complaint this answers: a view of a whole state rendered every trunk
/// road as stipple and called the result depth. Far views should be sparse and
/// legible, and detail should arrive as you approach it.
pub fn rank_floor(layer: Layer, zoom: f64) -> u16 {
    match layer {
        Layer::RoadMajor => match zoom {
            z if z < 4.5 => 219,
            z if z < 6.5 => 214,
            z if z < 8.5 => 200,
            _ => 0,
        },
        Layer::RoadMedium => match zoom {
            z if z < 11.0 => 188,
            z if z < 12.5 => 160,
            _ => 0,
        },
        _ => 0,
    }
}

/// How the ground surface is drawn.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Ground {
    /// Shaded stipple, displaced by elevation. The 3D reading.
    #[default]
    Ribbon,
    /// The same stipple with no vertical displacement at all: hillshade, the
    /// way a printed map does relief. Honest where the heightmap is coarse,
    /// because a wash cannot be the wrong shape.
    Shade,
    /// Ribbon with iso-elevation lines drawn over it. Spacing *is* slope.
    Contour,
}

impl Ground {
    pub fn label(self) -> &'static str {
        match self {
            Ground::Ribbon => "relief",
            Ground::Shade => "shade",
            Ground::Contour => "contour",
        }
    }

    pub fn next(self) -> Ground {
        match self {
            Ground::Ribbon => Ground::Contour,
            Ground::Contour => Ground::Shade,
            Ground::Shade => Ground::Ribbon,
        }
    }

    pub fn displaces(self) -> bool {
        self != Ground::Shade
    }
}

/// Vertical exaggeration for a zoom.
///
/// Terrain on a map is always exaggerated -- India's tallest ground is under
/// 0.1% of the width of the country, so at region scale a true profile is a
/// flat line and 14x is the usual cartographic lie. Held at 14 it becomes a
/// different kind of lie: measured off the shipped heightmap, the ground under
/// Ghatkopar spans 104 m, and 14x of that is 1.46 km of apparent relief in a
/// view 11 km wide. Mumbai's low hills came out as a mountain range, and the
/// question that produced this function was "are those mountains?".
///
/// So it tapers. Far out the lie is doing its job; close in there is real
/// vertical structure to read -- buildings, a lean, a street that visibly
/// climbs -- and the ground can afford to be nearly true.
pub fn exaggeration(zoom: f64) -> f64 {
    match zoom {
        z if z <= 9.0 => 14.0,
        z if z >= 14.0 => 2.0,
        z => 14.0 - (z - 9.0) / 5.0 * 12.0,
    }
}

/// Ground fills are texture, and texture at region scale is just noise.
pub fn draws_fills(mode: Mode) -> bool {
    mode != Mode::Flat
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The number that made Mumbai into a mountain range.
    ///
    /// Measured off the shipped heightmap: the ground under Ghatkopar spans
    /// 104 m, and the view in the report was 11 km wide. At a flat 14x that is
    /// 1.46 km of apparent relief -- more than a tenth of the frame, at 69
    /// degrees of tilt, for ground that barely rises.
    #[test]
    fn a_hundred_metre_rise_is_not_drawn_as_a_kilometre() {
        let ghatkopar_m = 104.0;
        assert!(
            ghatkopar_m * exaggeration(12.6) < 700.0,
            "104 m came out as {} m of apparent relief",
            ghatkopar_m * exaggeration(12.6)
        );
        // But the lie still does its job where it has to: at country scale a
        // true profile is a flat line.
        assert_eq!(exaggeration(5.0), 14.0);
    }

    /// Monotonic, and bounded at both ends. A taper that overshoots would
    /// invert the terrain, which is a worse bug than the one it replaces.
    #[test]
    fn the_taper_only_ever_falls() {
        let mut last = f64::MAX;
        let mut z = 3.0;
        while z <= 20.0 {
            let e = exaggeration(z);
            assert!(e <= last + 1e-9, "exaggeration rose at z{z}");
            assert!((2.0..=14.0).contains(&e), "z{z} gave {e}");
            last = e;
            z += 0.1;
        }
    }

    #[test]
    fn the_ground_styles_cycle_and_only_one_lies_flat() {
        let mut g = Ground::default();
        let mut seen = Vec::new();
        for _ in 0..3 {
            seen.push(g);
            g = g.next();
        }
        assert_eq!(g, Ground::default(), "the cycle does not close");
        assert_eq!(seen.len(), 3);
        assert!(!Ground::Shade.displaces());
        assert!(Ground::Ribbon.displaces() && Ground::Contour.displaces());
    }
}
