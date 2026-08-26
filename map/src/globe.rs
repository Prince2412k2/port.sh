//! The whole world, as a sphere, drawn from the outlines we happen to have.
//!
//! This is the view above the map rather than a wider version of it. Mercator
//! is the wrong projection for looking at a planet -- it has no edge, so zooming
//! out gives a wider and wider rectangle and never gives you a globe -- and the
//! renderer under it is built around a ground plane, which a sphere is not.
//! So the projection here is its own thing: orthographic, from a camera far
//! enough away that the near hemisphere is a disc.
//!
//! What it draws is deliberately almost nothing. A limb, a graticule, and the
//! outline of every region there is detailed data for. That last part is the
//! point of the whole view: the globe is an index of what this map knows, so
//! adding a region's `.tmap` puts it on the globe with no further work, and a
//! planet with one country drawn on it says what it is without a legend.
//!
//! Terminal-first, in the same sense as the ground modes. A sphere at this
//! resolution cannot be shaded -- eighty cells across is not enough for tone to
//! read as curvature -- so the form is carried by structure instead: the limb
//! is the silhouette, the graticule is the surface, and both fade towards the
//! edge where the surface is turning away. Curvature comes from the way the
//! parallels bunch, which is geometry the projection gives for free.

use crate::canvas::{
    Canvas, MAT_DOT, MAT_SHADE, MAT_SOLID, TINT_BORDER, TINT_COAST, TINT_GREEN, TINT_MONO,
};
use crate::data::Layer;
use crate::raster::{self, Pen};

/// Zoom below which the world is a sphere.
///
/// Above it the curvature across a frame is too slight to be worth the
/// projection, and the ground is what you came for.
///
/// Chosen by looking at the frame on the other side of it rather than by
/// reasoning about curvature: at 4.0 the first ground view came up already
/// cropping India, which reads as a jump. At 3.5 it holds the whole country,
/// so the handoff says "the globe filled up, and here is the place that was
/// facing you".
pub const UNTIL: f64 = 3.5;

/// Where the app opens: far enough out to see the planet, close enough that it
/// is not a marble. The camera is pointed at whatever the data covers, so the
/// first thing on screen is a world with the one place we know about facing
/// you, and zooming in goes there.
pub const OPENING: f64 = 1.6;

/// Whether this zoom is a globe.
pub fn shows(zoom: f64) -> bool {
    zoom < UNTIL
}

/// How much of the frame the disc takes, across the globe's band of zoom.
///
/// Ramped rather than fixed so that zooming in on the planet does something --
/// the disc grows towards the frame and the handoff to the ground reads as
/// arriving rather than as a cut. It is not scale-continuous with Mercator and
/// deliberately so: matching the sphere's centre scale to the map's makes the
/// disc double in size every zoom step, which leaves about one step between a
/// marble and a wall, and no room to turn the thing round and look at it.
fn fill(zoom: f64) -> f64 {
    0.60 + 0.34 * (zoom.max(0.0) / UNTIL).clamp(0.0, 1.0)
}

/// Degrees between graticule lines.
///
/// Thirty, not fifteen. At the size a terminal globe actually is, fifteen
/// degrees puts meridians a cell apart near the poles and the whole thing
/// silts up into a grey disc -- which is the failure this file is most at risk
/// of, and the reason so little is drawn.
const GRATICULE: i32 = 30;

/// Degrees between points along a graticule line. Small enough that a great
/// circle reads as a curve rather than a polygon.
const ARC_STEP: f64 = 3.0;

/// Where the camera is over, and how big the disc is.
#[derive(Clone, Copy, Debug)]
pub struct Globe {
    /// Longitude under the camera.
    pub lon: f64,
    /// Latitude under the camera.
    pub lat: f64,
    /// Disc radius, in subpixels.
    pub radius: f64,
    /// Disc centre, in subpixels.
    pub cx: f64,
    pub cy: f64,
}

impl Globe {
    /// A globe centred in `canvas`, sized for the zoom it is standing in for.
    pub fn fit(canvas: &Canvas, lon: f64, lat: f64, zoom: f64) -> Globe {
        let (w, h) = (canvas.sw as f64, canvas.sh as f64);
        Globe {
            lon,
            lat,
            radius: (w.min(h) * 0.5) * fill(zoom),
            cx: w * 0.5,
            cy: h * 0.5,
        }
    }

    /// Orthographic projection.
    ///
    /// Returns the screen point and the cosine of the angular distance from the
    /// point under the camera. That cosine is the whole visibility test -- it is
    /// positive on the near hemisphere and negative on the far one -- and it
    /// doubles as the depth cue, because a point at the limb is exactly the
    /// point whose surface has turned edge-on to the viewer.
    pub fn project(&self, lon: f64, lat: f64) -> ([f64; 2], f64) {
        let (l, p) = (lon.to_radians(), lat.to_radians());
        let (l0, p0) = (self.lon.to_radians(), self.lat.to_radians());
        let (dl_s, dl_c) = (l - l0).sin_cos();
        let (ps, pc) = p.sin_cos();
        let (p0s, p0c) = p0.sin_cos();

        let facing = p0s * ps + p0c * pc * dl_c;
        let x = pc * dl_s;
        let y = p0c * ps - p0s * pc * dl_c;
        // Screen y grows downward and latitude grows up, hence the negation.
        // Subpixels are square on the usual cell -- see `geo::PIXEL_ASPECT` --
        // so a sphere comes out round with no correction.
        ([self.cx + x * self.radius, self.cy - y * self.radius], facing)
    }

    /// Whether a point is on the hemisphere facing us.
    #[inline]
    pub fn faces(&self, lon: f64, lat: f64) -> bool {
        self.project(lon, lat).1 > 0.0
    }
}

/// How bright something is, given how far round the curve it has gone.
///
/// Not a fade for its own sake. A surface at the limb is edge-on and genuinely
/// shows less of itself, so dimming it is the honest reading -- and it is the
/// only cue at this resolution that says the disc is a ball rather than a
/// circle. Exaggerated well past the physical falloff, because the physical one
/// is imperceptible across eighty cells.
fn limb_fade(facing: f64) -> f32 {
    (0.18 + 0.82 * facing.clamp(0.0, 1.0).powf(0.6)) as f32
}

/// Draw the sphere. Returns how many segments were laid down.
pub fn draw(g: &Globe, canvas: &mut Canvas, overlays: &[std::rc::Rc<crate::data::Tile>]) -> usize {
    let mut n = 0;
    n += limb(g, canvas);
    n += graticule(g, canvas);
    n += regions(g, canvas, overlays);
    n
}

fn pen(alpha: f32, tint: u8, mat: u8, depth: f32) -> Pen {
    Pen { width: 1.0, alpha, depth, tint, mat, pick: u32::MAX, occlude: false }
}

/// The edge of the disc.
///
/// Drawn first and drawn solid. Everything else here is a hairline, so the one
/// mark that says "this is a body and it ends" has to be the heaviest thing on
/// the screen -- silhouette over interior, the same rule the hachures follow.
fn limb(g: &Globe, canvas: &mut Canvas) -> usize {
    let steps = ((g.radius * 2.2) as usize).clamp(64, 720);
    let mut last: Option<[f64; 2]> = None;
    let mut n = 0;
    for i in 0..=steps {
        let t = i as f64 / steps as f64 * std::f64::consts::TAU;
        let p = [g.cx + t.cos() * g.radius, g.cy - t.sin() * g.radius];
        if let Some(a) = last {
            // Braille, not blocks, despite this being the one line that wants
            // weight. A quadrant is 2x2 to braille's 2x4, so a solid limb
            // stair-steps into a cog at exactly the angles where a circle most
            // needs to look like one. Full coverage on the finer grid reads
            // heavier than a coarse block does, and stays round.
            raster::line(canvas, a, p, &pen(0.95, TINT_COAST, MAT_DOT, 0.5));
            n += 1;
        }
        last = Some(p);
    }
    n
}

/// Meridians and parallels, on the near side only.
///
/// The surface, such as it is. A bare limb is a circle; it is the graticule
/// bending across it that makes the circle read as a ball, and the bunching of
/// the parallels towards the poles is the curvature -- geometry the projection
/// hands over for nothing, which is exactly the kind of cue worth spending
/// cells on when there are so few.
fn graticule(g: &Globe, canvas: &mut Canvas) -> usize {
    let mut n = 0;
    let mut arc = |a: (f64, f64), b: (f64, f64), tint: u8, alpha: f32, canvas: &mut Canvas| {
        let (pa, fa) = g.project(a.0, a.1);
        let (pb, fb) = g.project(b.0, b.1);
        // Both ends on the near side. A segment straddling the limb is dropped
        // rather than clipped: the gap is under a cell wide and clipping it
        // costs a solve that nothing here would notice the absence of.
        if fa <= 0.0 || fb <= 0.0 {
            return;
        }
        let lit = alpha * limb_fade(fa.min(fb));
        raster::line(canvas, pa, pb, &pen(lit, tint, MAT_DOT, 0.5));
        n += 1;
    };

    for m in (-180..180).step_by(GRATICULE as usize) {
        let lon = m as f64;
        // The prime meridian gets a little more weight, so the globe has one
        // line you can orient from.
        let alpha = if m == 0 { 0.42 } else { 0.24 };
        let mut lat = -90.0;
        while lat < 90.0 {
            let next = (lat + ARC_STEP).min(90.0);
            arc((lon, lat), (lon, next), TINT_MONO, alpha, canvas);
            lat = next;
        }
    }
    for p in (-60..=60).step_by(GRATICULE as usize) {
        let lat = p as f64;
        let alpha = if p == 0 { 0.42 } else { 0.24 };
        let mut lon = -180.0;
        while lon < 180.0 {
            let next = (lon + ARC_STEP).min(180.0);
            arc((lon, lat), (next, lat), TINT_MONO, alpha, canvas);
            lon = next;
        }
    }
    n
}

/// Every region there is detail for.
///
/// Read straight off the always-resident overlays rather than from a list, so
/// this needs no registry and cannot fall out of step with one: drop another
/// region's `.tmap` beside `states.tmap` and it appears here. Boundaries only --
/// building footprints are also overlays and have no business on a planet.
fn regions(
    g: &Globe,
    canvas: &mut Canvas,
    overlays: &[std::rc::Rc<crate::data::Tile>],
) -> usize {
    let mut n = 0;
    for tile in overlays {
        for &i in &tile.by_layer[Layer::Boundary.index()] {
            let f = &tile.features[i as usize];
            for pair in f.pts.windows(2) {
                let (a, b) = (
                    crate::geo::world_to_lonlat(pair[0][0], pair[0][1]),
                    crate::geo::world_to_lonlat(pair[1][0], pair[1][1]),
                );
                let (pa, fa) = g.project(a.0, a.1);
                let (pb, fb) = g.project(b.0, b.1);
                if fa <= 0.0 || fb <= 0.0 {
                    continue;
                }
                // Tone, not line work, and this is the one decision in the file
                // that took seeing it to make.
                //
                // The overlay holds every internal boundary a region has -- for
                // India that is seventy-five state outlines -- and at a
                // centimetre across they do not read as borders, they read as
                // a scribble. Drawn in braille it was a patch of speckle the
                // eye tried and failed to resolve. The honest mark at this size
                // is not a smaller line, it is a different kind of mark: a
                // filled mass that says *a place is here* and leaves the
                // borders for the zoom that can show them.
                let lit = 0.75 * limb_fade(fa.min(fb));
                raster::line(canvas, pa, pb, &pen(lit, TINT_BORDER, MAT_SHADE, 0.4));
                n += 1;
            }
        }
    }
    n
}

/// Where a region's name should sit, and whether it is facing us at all.
///
/// The centroid of its outline, which for a country is close enough to the
/// middle to hang a label on and costs nothing to compute.
pub fn label_at(
    g: &Globe,
    overlays: &[std::rc::Rc<crate::data::Tile>],
) -> Option<([f64; 2], f64)> {
    let (mut sx, mut sy, mut count) = (0.0f64, 0.0f64, 0usize);
    for tile in overlays {
        for &i in &tile.by_layer[Layer::Boundary.index()] {
            for p in &tile.features[i as usize].pts {
                let (lon, lat) = crate::geo::world_to_lonlat(p[0], p[1]);
                sx += lon;
                sy += lat;
                count += 1;
            }
        }
    }
    if count == 0 {
        return None;
    }
    let (lon, lat) = (sx / count as f64, sy / count as f64);
    let (p, facing) = g.project(lon, lat);
    (facing > 0.0).then_some((p, facing))
}

/// A dot on the region, so it reads as a place and not as a shape.
pub fn mark(g: &Globe, canvas: &mut Canvas, overlays: &[std::rc::Rc<crate::data::Tile>]) {
    if let Some((p, facing)) = label_at(g, overlays) {
        raster::line(
            canvas,
            p,
            [p[0] + 0.6, p[1]],
            &pen(limb_fade(facing), TINT_GREEN, MAT_SOLID, 0.3),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn globe() -> Globe {
        Globe { lon: 78.0, lat: 22.0, radius: 100.0, cx: 200.0, cy: 100.0 }
    }

    /// The half of the world behind the planet must not be drawn on the front
    /// of it. Without the facing test a globe renders both hemispheres on top
    /// of each other and reads as a flat disc of noise.
    #[test]
    fn the_far_side_is_not_visible() {
        let g = globe();
        assert!(g.faces(78.0, 22.0), "the point under the camera must face it");
        // The antipode of (78, 22).
        assert!(!g.faces(-102.0, -22.0), "the antipode must be hidden");

        // A quarter turn away sits exactly on the limb, and the limb is not
        // the near side. Measured along the equator from an equatorial camera,
        // because ninety degrees of *longitude* is only ninety degrees of arc
        // there -- at latitude 22 it is considerably less, which is what the
        // first version of this test got wrong.
        // Either side of it, not on it: at exactly ninety degrees the cosine is
        // 6e-17 rather than zero, so which way the boundary tips is a question
        // about floating point and not about the globe.
        let e = Globe { lon: 0.0, lat: 0.0, ..g };
        assert!(e.faces(89.0, 0.0), "just inside the limb faces us");
        assert!(!e.faces(91.0, 0.0), "just outside it does not");
    }

    /// The point under the camera lands in the middle, and the limb lands a
    /// radius away, whichever way you look.
    #[test]
    fn the_disc_is_centred_and_a_radius_across() {
        let g = globe();
        let (p, facing) = g.project(g.lon, g.lat);
        assert!((p[0] - g.cx).abs() < 1e-9 && (p[1] - g.cy).abs() < 1e-9);
        assert!((facing - 1.0).abs() < 1e-9);

        for d in [0.0, 45.0, 90.0, 135.0, 180.0, 270.0] {
            let (q, _) = g.project(g.lon + d, 0.0);
            let r = (q[0] - g.cx).hypot(q[1] - g.cy);
            assert!(r <= g.radius + 1e-9, "{d} degrees away projected outside the disc");
        }
    }

    /// Nothing at all falls outside the disc, from any camera, anywhere on
    /// Earth. An orthographic sphere has a hard edge and a point escaping it is
    /// a projection bug that would otherwise show up as a stray mark in space.
    #[test]
    fn no_point_on_earth_lands_off_the_disc() {
        for (clon, clat) in [(0.0, 0.0), (78.0, 22.0), (-120.0, -45.0), (0.0, 90.0)] {
            let g = Globe { lon: clon, lat: clat, radius: 100.0, cx: 200.0, cy: 100.0 };
            let mut lat = -90.0;
            while lat <= 90.0 {
                let mut lon = -180.0;
                while lon < 180.0 {
                    let (p, _) = g.project(lon, lat);
                    let r = (p[0] - g.cx).hypot(p[1] - g.cy);
                    assert!(
                        r <= g.radius + 1e-6,
                        "({lon}, {lat}) from ({clon}, {clat}) landed {r} out of {}",
                        g.radius
                    );
                    lon += 7.0;
                }
                lat += 7.0;
            }
        }
    }

    /// A region on the far side of the planet is not drawn through it.
    ///
    /// The projection test above says the maths is right; this says the drawing
    /// obeys it. Without the facing check a globe paints both hemispheres on
    /// top of each other and reads as a flat disc of noise, and the failure is
    /// invisible from any single camera position -- you have to turn it round.
    #[test]
    fn a_region_on_the_far_side_is_not_drawn_through_the_planet() {
        // A small square where India is.
        let corners = [(74.0, 18.0), (82.0, 18.0), (82.0, 26.0), (74.0, 26.0), (74.0, 18.0)];
        let pts: Vec<[f64; 2]> = corners
            .iter()
            .map(|&(lon, lat)| crate::geo::lonlat_to_world(lon, lat))
            .collect();
        let tile = std::rc::Rc::new(crate::data::Tile::new(vec![crate::data::Feature::new(
            Layer::Boundary,
            10,
            true,
            Some("test".into()),
            pts,
        )]));
        let overlays = vec![tile];

        let ink = |lon: f64, lat: f64| {
            let mut canvas = Canvas::new(60, 30);
            let g = Globe::fit(&canvas, lon, lat, OPENING);
            regions(&g, &mut canvas, &overlays)
        };

        assert!(ink(78.0, 22.0) > 0, "the region should draw when it faces the camera");
        // The antipode, and a point far enough round that none of the square
        // can still be on the near side.
        assert_eq!(ink(-102.0, -22.0), 0, "drawn through the planet from behind");
        assert_eq!(ink(-120.0, -30.0), 0, "drawn from the far hemisphere");
    }

    /// Brightness falls towards the limb and never reaches zero: a mark that
    /// made it through the facing test should still be seen.
    #[test]
    fn the_edge_is_dimmer_than_the_middle_but_never_dark() {
        assert!(limb_fade(1.0) > limb_fade(0.5));
        assert!(limb_fade(0.5) > limb_fade(0.02));
        assert!(limb_fade(0.0) > 0.0);
        assert!(limb_fade(1.0) <= 1.0);
    }
}

#[cfg(test)]
mod zoom_tests {
    use super::*;

    /// The app opens on the planet, and zooming in leaves it.
    #[test]
    fn the_opening_view_is_the_globe_and_zooming_in_leaves_it() {
        assert!(shows(OPENING), "the app should open on the globe");
        assert!(shows(crate::geo::MIN_ZOOM), "and stay on it all the way out");
        assert!(!shows(UNTIL), "and hand over at the threshold");
        assert!(!shows(UNTIL + 4.0), "and stay handed over");
    }

    /// The zoom floor has to sit under the globe's band, or the globe is a
    /// view you can never reach. It used to be 2.5, which is inside it.
    #[test]
    fn the_zoom_floor_leaves_room_for_the_planet() {
        assert!(
            crate::geo::MIN_ZOOM < OPENING && OPENING < UNTIL,
            "MIN_ZOOM {} / OPENING {OPENING} / UNTIL {UNTIL} are not in order",
            crate::geo::MIN_ZOOM
        );
    }

    /// The disc grows as you come in, so zooming on the planet does something
    /// and the handoff reads as arriving rather than as a cut. Bounded at both
    /// ends: never a speck, never wider than the frame it has to sit in.
    #[test]
    fn the_disc_grows_towards_the_handoff() {
        assert!(fill(UNTIL) > fill(0.0));
        let mut z = 0.0;
        let mut last = 0.0;
        while z <= UNTIL {
            let f = fill(z);
            assert!(f >= last, "the disc shrank going in, at z{z}");
            assert!((0.4..=1.0).contains(&f), "z{z} gave {f} of the frame");
            last = f;
            z += 0.1;
        }
    }
}
