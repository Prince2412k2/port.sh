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
    /// Country to region. Untilted, no ground fills: a reference map.
    Flat,
    /// City scale. A slight lean.
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

/// The smallest a feature may be on screen and still be drawn, in subpixels.
///
/// Rank alone cannot carry level of detail here, and the shipped basemap shows
/// why: over India at z4.6 the road ranks fall into three tiers, and the floor
/// either lets 103362 features through or two. There is no setting between
/// "every lane in the country" and "nothing", because the classification was
/// never made for this scale.
///
/// Screen extent is the question that actually matters and it does not depend
/// on how the data was tagged. The numbers are around one subpixel, which
/// sounds like it would do nothing and removes 98% of the features: the
/// basemap stores roads as short fragments, and at country zoom almost every
/// fragment is smaller than the smallest mark the terminal can make. A
/// hundred thousand of those are the even stipple that made the frame
/// unreadable. Over India at z4.6 this is 103362 features down to 2025, and
/// what survives is what was long enough to be a line.
///
/// Roads only. A short *water* feature at country zoom is a lake, which is a
/// thing in itself; a short road is a fragment of a thing.
pub fn min_extent(layer: Layer, zoom: f64) -> f64 {
    if !matches!(layer, Layer::RoadMajor | Layer::RoadMedium | Layer::RoadMinor | Layer::Rail) {
        return 0.0;
    }
    match zoom {
        z if z < 6.0 => 1.5,
        z if z < 8.0 => 1.0,
        z if z < 10.0 => 0.5,
        _ => 0.0,
    }
}




/// Ground fills are texture, and texture at region scale is just noise.
pub fn draws_fills(mode: Mode) -> bool {
    mode != Mode::Flat
}
