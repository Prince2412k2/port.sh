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
        // No step across 215 anywhere in here, and that is the whole point.
        // The ranks in this basemap fall into three tiers with nothing between
        // them -- measured at z4.8 on a 190x48 frame, a floor of 215 admits
        // 2072 features, 216 admits 135, and 222 admits four. A ladder that
        // steps over 215/216 therefore multiplies the frame by fifteen in one
        // wheel notch, which is what this did at z4.5: 67 features became
        // 1498 and the frame went from 2.1 ms to 7.9.
        //
        // So rank stops trying to control density down here and `min_extent`
        // does it instead, because screen extent is continuous and rank is not.
        Layer::RoadMajor => match zoom {
            z if z < 8.5 => 214,
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
    // Continuous, because this is now the only thing holding density down at
    // region zoom and a step here is a cliff on screen. Five subpixels at z4
    // easing to nothing by z10: features arrive as the frame grows enough to
    // hold them, a few at a time, instead of fifteen hundred at once.
    ((10.0 - zoom) * 0.85).clamp(0.0, 5.0)
}




/// Vertical exaggeration of the terrain, by zoom.
///
/// Large, and unapologetically so. At a 50 km frame the Dhauladhar stands
/// about 4 km out of its valleys, which is eight percent of the width of the
/// view -- true to scale it is a wrinkle, and the terminal has perhaps fifty
/// rows to spend on the whole height of the frame. The exaggeration is what
/// turns a wrinkle into a skyline. It comes down as the view closes in,
/// because at street scale the ground is genuinely close to flat and the same
/// factor would throw the map off the top of the screen.
pub fn exaggeration(zoom: f64) -> f64 {
    match zoom {
        z if z <= 9.0 => 14.0,
        z if z >= 14.0 => 2.0,
        z => 14.0 - (z - 9.0) / 5.0 * 12.0,
    }
}

/// How firmly the ground is drawn, by zoom: 0 not at all, 1 as a solid surface.
///
/// Ramped rather than switched, because the pass turns opaque at full strength
/// and a surface that appears all at once takes the roads behind it with it.
pub fn ground_strength(zoom: f64) -> f32 {
    /// First light. Below this a 30 arcsec heightmap has little to say that is
    /// not already the shape of the coastline.
    const DAWN: f64 = 6.0;
    /// Full strength, and opaque from here up.
    const NOON: f64 = 7.0;
    (((zoom - DAWN) / (NOON - DAWN)) as f32).clamp(0.0, 1.0)
}

/// Ground fills are texture, and texture at region scale is just noise.
pub fn draws_fills(mode: Mode) -> bool {
    mode != Mode::Flat
}

/// Density has to change smoothly with zoom, and rank cannot make it.
#[cfg(test)]
mod density_tests {
    use super::*;

    /// No wheel notch may more than double the work.
    ///
    /// The rank tiers below are measured, not invented: at z4.8 on a 190x48
    /// frame a floor of 215 admits 2072 features, 216 admits 135, 222 admits
    /// four. Rank alone cannot step between those without a cliff, so this
    /// checks the rank floor and the extent ramp *together* -- which is the
    /// only level at which the question has an answer.
    #[test]
    fn no_wheel_notch_doubles_the_map() {
        let admitted = |zoom: f64| -> f64 {
            let floor = rank_floor(Layer::RoadMajor, zoom);
            let tier = if floor <= 215 {
                2072.0
            } else if floor <= 221 {
                135.0
            } else {
                4.0
            };
            // Longer features are rarer, roughly as the square of the length
            // once they are fragments of a network. Good enough to catch a
            // cliff, which is all this is for.
            let e = min_extent(Layer::RoadMajor, zoom).max(0.2);
            tier / (e * e)
        };
        let mut prev = admitted(4.0);
        let mut z = 4.1;
        while z <= 12.0 {
            let now = admitted(z);
            assert!(
                now <= prev * 2.0,
                "z{z:.1} admits {now:.0} against {prev:.0} one notch earlier"
            );
            prev = now;
            z += 0.1;
        }
    }
}
