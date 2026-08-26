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
    /// Country to region. Untilted, no ground fills: a reference map. The
    /// terrain is not the mode's to decide -- see `ground_strength`.
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
    /// Masses, ridges and structural contours -- terrain drawn as a picture of
    /// a mountain rather than as a plot of a heightmap.
    ///
    /// The other four all answer "what is the elevation here" once per sample
    /// and put a mark down for the answer. That is the right question for a
    /// city, where roads and buildings are sparse and their geometry *is* the
    /// information. Ground is not sparse: every cell has an elevation, so every
    /// cell gets a mark, and a frame where every cell is equally marked is a
    /// frame with no shape in it. The eye reads texture and stops.
    ///
    /// So this one throws most of it away on purpose. Three layers, in the
    /// order the eye wants them: broad light and shade as blocks, the ridges
    /// that carry the silhouette as continuous strokes, and a few contours for
    /// reference. No per-sample stipple at all. It draws about a fifth of the
    /// marks the ribbon does and says considerably more with them.
    Massif,
    /// Shaded stipple, displaced by elevation. The 3D reading.
    ///
    /// The default, and it is braille all the way down. What was wrong with it
    /// was never the glyph -- it was that the stipple had no boundary and no
    /// tonal range, so it read as texture. It has an outline now (`Canvas::rim`)
    /// and the bottom of the ramp back, and only ground that stands thirty
    /// metres over its surroundings is drawn at all.
    #[default]
    Ribbon,
    /// The same stipple with no vertical displacement at all: hillshade, the
    /// way a printed map does relief. Honest where the heightmap is coarse,
    /// because a wash cannot be the wrong shape.
    Shade,
    /// Ribbon with iso-elevation lines drawn over it. Spacing *is* slope.
    Contour,
    /// Strokes down the line of steepest descent, and nothing else.
    ///
    /// The other three all paint the surface and then say something about it.
    /// This one paints only what it has something to say about: ground with no
    /// slope gets no marks at all, and the quiet is the point. Hachures are the
    /// pre-contour way of drawing relief -- Lehmann's, from 1799 -- and they sit
    /// exactly opposite contours in the same language. A contour runs across the
    /// slope and you read steepness from how close the lines are. A hachure runs
    /// down it and you read steepness from how dark and how long the stroke is.
    ///
    /// It is also the one mode that changes glyph family with distance rather
    /// than just fading: near strokes are laid in block, far ones in braille.
    Hachure,
}

impl Ground {
    pub fn label(self) -> &'static str {
        match self {
            Ground::Massif => "massif",
            Ground::Ribbon => "relief",
            Ground::Shade => "shade",
            Ground::Contour => "contour",
            Ground::Hachure => "hachure",
        }
    }

    pub fn next(self) -> Ground {
        match self {
            Ground::Massif => Ground::Ribbon,
            Ground::Ribbon => Ground::Contour,
            Ground::Contour => Ground::Hachure,
            Ground::Hachure => Ground::Shade,
            Ground::Shade => Ground::Massif,
        }
    }

    pub fn displaces(self) -> bool {
        self != Ground::Shade
    }

    /// Whether the surface itself gets painted, as opposed to only described.
    ///
    /// The depth buffer is written either way -- a ridge still hides the road
    /// behind it. This only says whether the stipple is laid down, which is the
    /// difference between understanding the scene and painting it.
    pub fn paints_surface(self) -> bool {
        !matches!(self, Ground::Hachure | Ground::Massif)
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

/// How firmly the ground is drawn at a zoom, 0 (not at all) to 1.
///
/// This used to be `Mode::terrain()`, which is to say a switch at z10, and two
/// things were wrong with that. The frame whose scalebar reads 10 km sits just
/// under z10 -- the scale at which a mountain range *is* the subject, and the
/// one scale that had no ground on it at all. And a switch put a whole
/// hillside on in one wheel notch: measured over Zanskar on a 150x40 frame,
/// 726 marks of ink at z9.5 and 5192 at z10.
///
/// So it comes up over a zoom instead, and is full before the bar can read
/// 10 km at all. "Before it can" and not "when it does": the bar is picked
/// from the cell size and the frame width, so which zoom shows 10 km moves
/// with the terminal and the latitude -- across 80- to 300-cell frames over
/// India it is anywhere from z8.15 to z10.1. `NOON` sits under the whole of
/// that range rather than in the middle of it.
pub fn ground_strength(zoom: f64) -> f32 {
    /// First light: below this the ground is not drawn. About a 50 km bar,
    /// which is roughly where a 30 arcsec heightmap stops having anything to
    /// say that is not already the shape of the coastline.
    const DAWN: f64 = 7.0;
    /// Full strength, under every zoom at which the bar can read 10 km.
    const NOON: f64 = 8.0;
    (((zoom - DAWN) / (NOON - DAWN)) as f32).clamp(0.0, 1.0)
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

    /// `v` has to reach every style and come back, however many there are.
    ///
    /// Written against a count when there were three, which is how adding a
    /// fourth broke it: the cycle was fine and the test was counting.
    #[test]
    fn the_ground_styles_cycle_through_all_of_themselves() {
        let all =
            [Ground::Massif, Ground::Ribbon, Ground::Contour, Ground::Hachure, Ground::Shade];
        let mut g = Ground::default();
        let mut seen = Vec::new();
        for _ in 0..all.len() {
            assert!(!seen.contains(&g), "{g:?} came round twice before the cycle closed");
            seen.push(g);
            g = g.next();
        }
        assert_eq!(g, Ground::default(), "the cycle does not close");
        for style in all {
            assert!(seen.contains(&style), "{style:?} is unreachable from the key");
        }
    }

    /// The two axes the styles vary on, and they are independent.
    ///
    /// `displaces` is whether elevation moves a mark up the screen; `paints`
    /// is whether the surface gets a stipple at all. Hachure is the one that
    /// displaces without painting -- it writes the depth buffer so a ridge
    /// still hides what is behind it, and then says nothing more.
    #[test]
    fn only_shade_lies_flat_and_only_hachure_leaves_the_surface_bare() {
        assert!(!Ground::Shade.displaces());
        for style in [Ground::Massif, Ground::Ribbon, Ground::Contour, Ground::Hachure] {
            assert!(style.displaces(), "{style:?} should read elevation");
        }
        assert!(!Ground::Hachure.paints_surface());
        for style in [Ground::Ribbon, Ground::Contour, Ground::Shade] {
            assert!(style.paints_surface(), "{style:?} should lay the stipple");
        }
    }
}
