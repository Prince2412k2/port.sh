//! Web Mercator projection and the viewport that maps world coords to subpixels.
//!
//! Everything downstream works in *world* coordinates: Mercator normalised to
//! 0..1 with (0,0) at the north-west corner. Converting lon/lat costs a `ln` and
//! a `sin`, so features are converted once at load and never again.

use std::f64::consts::PI;

/// Subpixels across one tile at zoom 0. Matches the slippy-map convention so
/// zoom numbers mean roughly what they mean in every other map tool.
const TILE: f64 = 256.0;

/// Height/width ratio of a terminal cell. Braille packs 2x4 dots into a cell,
/// so on a 1:2 cell the dots come out square and no correction is needed.
/// Fonts vary; this is the one knob to turn if the map looks squashed.
pub const CELL_ASPECT: f64 = 2.0;

/// Vertical scale correction. A braille dot is `cell_w/2` wide and `cell_h/4`
/// tall, so its aspect is `CELL_ASPECT/2`; undistorting the map means scaling y
/// by the reciprocal. Lands on exactly 1.0 for the usual 1:2 cell.
pub const PIXEL_ASPECT: f64 = 2.0 / CELL_ASPECT;

/// Low enough to frame a whole country. A city-sized floor silently clamps
/// the initial fit and makes a national view unreachable.
/// Closest a point may sit to the eye before the perspective divide is pinned.
const NEAR_CLIP: f64 = 1.0;

pub const MIN_ZOOM: f64 = 2.5;
pub const MAX_ZOOM: f64 = 18.0;

pub fn lonlat_to_world(lon: f64, lat: f64) -> [f64; 2] {
    let x = (lon + 180.0) / 360.0;
    let s = (lat.to_radians()).sin().clamp(-0.999_999, 0.999_999);
    let y = 0.5 - ((1.0 + s) / (1.0 - s)).ln() / (4.0 * PI);
    [x, y]
}

pub fn world_to_lonlat(x: f64, y: f64) -> (f64, f64) {
    let lon = x * 360.0 - 180.0;
    let n = PI * (1.0 - 2.0 * y);
    let lat = n.sinh().atan().to_degrees();
    (lon, lat)
}

/// Ground metres per world unit at a given latitude.
pub fn meters_per_world_unit(lat: f64) -> f64 {
    40_075_016.686 * lat.to_radians().cos()
}

#[derive(Clone, Copy, Debug)]
pub struct Viewport {
    /// Map centre, world coords.
    pub center: [f64; 2],
    pub zoom: f64,
    /// Canvas size in subpixels, not cells.
    pub sw: f64,
    pub sh: f64,
    /// Camera rotation about the vertical axis, radians. 0 = north up.
    pub bearing: f64,
    /// Camera pitch, radians. 0 = straight down, which is the 2D map.
    pub tilt: f64,
    /// Perspective strength, 0 = parallel.
    ///
    /// Convergence is the depth cue a parallel projection cannot produce: it is
    /// what makes a street recede. The cost is a near plane -- anything at or
    /// behind the camera has to be clipped before the divide, or it wraps
    /// across the screen -- so this stays dialled down at wide zooms where it
    /// buys little and risks much.
    pub persp: f64,
}

impl Viewport {
    pub fn new(center: [f64; 2], zoom: f64) -> Self {
        Self { center, zoom, sw: 1.0, sh: 1.0, bearing: 0.0, tilt: 0.0, persp: 0.0 }
    }

    /// True when the camera is looking straight down and unrotated, in which
    /// case the tilted path collapses to the plain 2D one.
    #[inline]
    pub fn is_flat(&self) -> bool {
        self.tilt.abs() < 1e-9 && self.bearing.abs() < 1e-9
    }

    /// Subpixels per world unit. One subpixel stands in for one slippy-map
    /// pixel, so zoom levels line up with what other map tools call z.
    #[inline]
    pub fn scale(&self) -> f64 {
        TILE * 2f64.powf(self.zoom)
    }

    #[inline]
    pub fn project(&self, w: [f64; 2]) -> [f64; 2] {
        self.project3(w, 0.0).0
    }

    /// Oblique projection of a world point at elevation `h` (world units).
    ///
    /// Deliberately parallel rather than perspective: there is no vanishing
    /// point, no near-plane clipping and no divide, which at braille resolution
    /// costs nothing visually and removes a whole class of degenerate cases.
    ///
    /// Returns the screen position and a normalised depth, 0 nearest.
    #[inline]
    pub fn project3(&self, w: [f64; 2], h: f64) -> ([f64; 2], f32) {
        let s = self.scale();
        let dx = (w[0] - self.center[0]) * s;
        let dy = (w[1] - self.center[1]) * s;

        if self.is_flat() {
            return (
                [dx + self.sw * 0.5, dy * PIXEL_ASPECT + self.sh * 0.5],
                0.5,
            );
        }

        let (sb, cb) = self.bearing.sin_cos();
        let mx = dx * cb + dy * sb;
        let my = -dx * sb + dy * cb;

        let (st, ct) = self.tilt.sin_cos();
        // Looking down the tilted plane foreshortens distance, and height on
        // the ground turns into height on the screen.
        let mut sx = mx;
        let mut sy = my * ct * PIXEL_ASPECT - h * s * st * PIXEL_ASPECT;

        if self.persp > 1e-9 {
            // Distance from the eye, along the view direction. The eye sits
            // back from the near edge of the slab by `eye`; anything closer
            // than NEAR_CLIP would flip through the divide and smear across the
            // frame, so it is pinned instead.
            let eye = self.sh / self.persp.max(1e-6);
            // Far is *negative* my -- world y grows southward while the camera
            // looks up the screen -- so distance from the eye subtracts it.
            // Adding it magnifies what should recede.
            let dist = eye - my * st;
            // Rejected, not clamped. Pinning a point that has passed the eye
            // gives it an enormous scale factor and flings it across the frame
            // in a radial fan -- the classic near-plane artifact. Callers skip
            // anything whose depth is not finite.
            if dist < NEAR_CLIP {
                return ([f64::NAN, f64::NAN], f32::INFINITY);
            }
            let k = eye / dist;
            sx *= k;
            sy *= k;
        }

        // Depth spans the visible slab of the map plane, normalised so the fog
        // curve sees the same 0..1 range it does in 2D. Negated because world y
        // grows southward while the tilted camera looks *up* the screen toward
        // the horizon: small y is the top of the screen, and the top is far.
        let span = (self.sw + self.sh).max(1.0);
        let depth = (0.5 - my * st / span).clamp(0.0, 1.0) as f32;

        ([sx + self.sw * 0.5, sy + self.sh * 0.5], depth)
    }

    /// World point into the rotated map plane, in subpixels. This is the frame
    /// the camera works in: x across, y into the screen.
    #[inline]
    pub fn plane_of(&self, w: [f64; 2]) -> [f64; 2] {
        let s = self.scale();
        let dx = (w[0] - self.center[0]) * s;
        let dy = (w[1] - self.center[1]) * s;
        let (sb, cb) = self.bearing.sin_cos();
        [dx * cb + dy * sb, -dx * sb + dy * cb]
    }

    /// Inverse of `plane_of`.
    #[inline]
    pub fn world_of_plane(&self, m: [f64; 2]) -> [f64; 2] {
        let s = self.scale();
        let (sb, cb) = self.bearing.sin_cos();
        let dx = m[0] * cb - m[1] * sb;
        let dy = m[0] * sb + m[1] * cb;
        [dx / s + self.center[0], dy / s + self.center[1]]
    }

    /// The ground slab, in plane coords: `[half_width, far_y, near_y]`.
    ///
    /// Tilting a projection that has no vanishing point does not, on its own,
    /// look like anything: the ground covers the whole frame and there is no
    /// cue for the eye to read. Bounding the ground to a finite slab gives it
    /// edges, and the edges are what make the tilt legible.
    ///
    /// The extents are derived from where the edges should *land on screen*
    /// rather than computed forward. Under perspective the two differ a lot --
    /// sizing the slab with parallel maths leaves it hugging the top of the
    /// frame with the near half of the view empty.
    pub fn plate(&self) -> [f64; 3] {
        // Target rows for the far and near edges, relative to centre.
        let far_py = -self.sh * 0.34;
        let near_py = self.sh * 0.46;

        let ct = self.tilt.cos().max(0.20);
        let a = PIXEL_ASPECT * ct;

        let (far_y, near_y, near_k) = if self.persp > 1e-9 {
            let st = self.tilt.sin();
            let eye = self.sh / self.persp.max(1e-6);
            let solve = |py: f64| {
                let denom = eye * a + py * st;
                if denom.abs() < 1e-9 { 0.0 } else { py * eye / denom }
            };
            let n = solve(near_py);
            (solve(far_py), n, eye / (eye - n * st).max(NEAR_CLIP))
        } else {
            (far_py / a, near_py / a, 1.0)
        };

        // The near edge is magnified most, so it decides the width that fits.
        let half_w = self.sw * 0.44 / near_k.max(1e-6);
        [half_w, far_y, near_y]
    }

    /// Screen point back to a world point on the ground plane (elevation 0).
    /// Inverting the tilt is what keeps drag-pan honest once the camera moves.
    #[inline]
    pub fn unproject(&self, p: [f64; 2]) -> [f64; 2] {
        let s = self.scale();
        let px = p[0] - self.sw * 0.5;
        let py = p[1] - self.sh * 0.5;

        if self.is_flat() {
            return [px / s + self.center[0], py / (s * PIXEL_ASPECT) + self.center[1]];
        }

        let ct = self.tilt.cos();
        let (mx, my) = if self.persp > 1e-9 {
            // Solve for the ground point whose divide lands on this pixel:
            // sy = (my*ct*A) * eye/(eye + my*st), which is linear in my.
            let st = self.tilt.sin();
            let eye = self.sh / self.persp.max(1e-6);
            let a = PIXEL_ASPECT * ct;
            let denom = eye * a + py * st;
            let my = if denom.abs() < 1e-9 { 0.0 } else { py * eye / denom };
            let k = eye / (eye - my * st).max(NEAR_CLIP);
            (px / k.max(1e-6), my)
        } else {
            (px, py / (PIXEL_ASPECT * ct.max(1e-3)))
        };

        let (sb, cb) = self.bearing.sin_cos();
        let dx = mx * cb - my * sb;
        let dy = mx * sb + my * cb;
        [dx / s + self.center[0], dy / s + self.center[1]]
    }

    /// World-space bounds of what is currently on screen, grown by `pad`
    /// subpixels so features crossing the edge still get drawn.
    pub fn world_bounds(&self, pad: f64) -> [f64; 4] {
        // All four corners, and that is the whole point of this function.
        //
        // `unproject` applies the bearing, so with the camera turned the screen
        // rectangle maps to a *rotated quad* in world space and two opposite
        // corners stop bounding it. They are symmetric about the centre, so
        // past roughly fifty degrees -- the exact angle depends on the aspect
        // of the viewport -- they cross over and minx comes out greater than
        // maxx. The box is then inverted rather than merely wrong, and
        // `Feature::visible_in` passes only features big enough to span the
        // whole inverted range. Which is exactly how this failed: rotating past
        // 57 degrees dropped every road and left the coastline and the state
        // borders, because those were the only things large enough to qualify,
        // and a few degrees later it dropped those too.
        let corners = [
            self.unproject([-pad, -pad]),
            self.unproject([self.sw + pad, -pad]),
            self.unproject([-pad, self.sh + pad]),
            self.unproject([self.sw + pad, self.sh + pad]),
        ];
        let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
        for c in corners {
            if !c[0].is_finite() || !c[1].is_finite() {
                continue;
            }
            b[0] = b[0].min(c[0]);
            b[1] = b[1].min(c[1]);
            b[2] = b[2].max(c[0]);
            b[3] = b[3].max(c[1]);
        }
        // A tilted camera can put a corner past the horizon, where unprojecting
        // has no answer to give. Cull nothing rather than everything: a frame
        // that draws too much is a frame somebody can see.
        if b[0] > b[2] || b[1] > b[3] {
            return [f64::MIN, f64::MIN, f64::MAX, f64::MAX];
        }
        b
    }

    /// Centre and zoom so `b` (world minx,miny,maxx,maxy) fills the viewport
    /// with a little air around it.
    pub fn fit(&mut self, b: [f64; 4]) {
        self.center = [(b[0] + b[2]) * 0.5, (b[1] + b[3]) * 0.5];
        let (w, h) = ((b[2] - b[0]).max(1e-9), (b[3] - b[1]).max(1e-9));
        let zx = (self.sw * 0.92 / (TILE * w)).log2();
        let zy = (self.sh * 0.92 / (TILE * h * PIXEL_ASPECT)).log2();
        self.zoom = zx.min(zy).clamp(MIN_ZOOM, MAX_ZOOM);
    }

    pub fn pan_subpixels(&mut self, dx: f64, dy: f64) {
        let s = self.scale();
        self.center[0] += dx / s;
        self.center[1] += dy / (s * PIXEL_ASPECT);
        self.clamp();
    }

    /// Zoom while keeping the world point under `anchor` pinned to `anchor`.
    /// This is what makes scroll-wheel zoom feel like a real map instead of a
    /// slideshow.
    pub fn zoom_at(&mut self, dz: f64, anchor: [f64; 2]) {
        let before = self.unproject(anchor);
        self.zoom = (self.zoom + dz).clamp(MIN_ZOOM, MAX_ZOOM);
        let after = self.unproject(anchor);
        self.center[0] += before[0] - after[0];
        self.center[1] += before[1] - after[1];
        self.clamp();
    }

    fn clamp(&mut self) {
        self.center[0] = self.center[0].clamp(0.0, 1.0);
        self.center[1] = self.center[1].clamp(0.0, 1.0);
    }

    pub fn center_lonlat(&self) -> (f64, f64) {
        world_to_lonlat(self.center[0], self.center[1])
    }

    /// Ground metres represented by one subpixel at the viewport centre.
    pub fn meters_per_subpixel(&self) -> f64 {
        let (_, lat) = self.center_lonlat();
        meters_per_world_unit(lat) / self.scale()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vp() -> Viewport {
        let mut v = Viewport::new(lonlat_to_world(72.8777, 19.0760), 12.0);
        v.sw = 400.0;
        v.sh = 200.0;
        v
    }

    #[test]
    fn lonlat_world_roundtrip() {
        for (lon, lat) in [(72.8777, 19.0760), (-0.1276, 51.5072), (139.69, 35.68)] {
            let w = lonlat_to_world(lon, lat);
            let (lon2, lat2) = world_to_lonlat(w[0], w[1]);
            assert!((lon - lon2).abs() < 1e-9, "lon {lon} -> {lon2}");
            assert!((lat - lat2).abs() < 1e-9, "lat {lat} -> {lat2}");
        }
    }

    #[test]
    fn project_unproject_roundtrip() {
        let v = vp();
        for p in [[0.0, 0.0], [123.0, 45.0], [400.0, 200.0]] {
            let back = v.project(v.unproject(p));
            assert!((back[0] - p[0]).abs() < 1e-6);
            assert!((back[1] - p[1]).abs() < 1e-6);
        }
    }

    /// The whole point of cursor-anchored zoom: whatever is under the pointer
    /// must stay under the pointer.
    #[test]
    fn zoom_keeps_anchor_fixed() {
        for anchor in [[0.0, 0.0], [37.0, 180.0], [400.0, 200.0]] {
            let mut v = vp();
            let before = v.unproject(anchor);
            for dz in [0.3, 0.3, -1.1, 2.0] {
                v.zoom_at(dz, anchor);
            }
            let after = v.unproject(anchor);
            assert!(
                (before[0] - after[0]).abs() < 1e-9 && (before[1] - after[1]).abs() < 1e-9,
                "anchor {anchor:?} drifted {before:?} -> {after:?}"
            );
        }
    }

    /// Drag-pan depends on unproject inverting project on the ground plane, at
    /// any camera angle.
    #[test]
    fn tilted_unproject_roundtrip() {
        for (tilt, bearing) in [(0.0, 0.0), (0.6, 0.0), (0.0, 0.9), (1.1, 2.4)] {
            let mut v = vp();
            v.tilt = tilt;
            v.bearing = bearing;
            for p in [[10.0, 10.0], [200.0, 100.0], [390.0, 190.0]] {
                let back = v.project(v.unproject(p));
                assert!(
                    (back[0] - p[0]).abs() < 1e-6 && (back[1] - p[1]).abs() < 1e-6,
                    "tilt {tilt} bearing {bearing}: {p:?} -> {back:?}"
                );
            }
        }
    }

    /// Raising a point above the ground moves it up the screen and nowhere else.
    #[test]
    fn elevation_lifts_on_screen() {
        let mut v = vp();
        v.tilt = 0.7;
        let w = lonlat_to_world(72.88, 19.08);
        let (flat, _) = v.project3(w, 0.0);
        let (high, _) = v.project3(w, 0.0005);
        assert!((flat[0] - high[0]).abs() < 1e-9, "elevation moved x");
        assert!(high[1] < flat[1], "elevation did not lift: {flat:?} -> {high:?}");
    }

    /// Under perspective, something farther away must project *smaller*. The
    /// sign of the distance term is easy to get backwards and the symptom --
    /// distant geometry blowing up to fill the frame -- looks like a clipping
    /// bug rather than an arithmetic one.
    #[test]
    fn perspective_shrinks_with_distance() {
        let mut v = vp();
        v.tilt = 0.8;
        v.persp = 0.6;
        // Two segments of equal world length, one near the camera, one far.
        // Both must sit in front of the eye; a larger offset puts the near one
        // behind it, which is a rejection rather than a projection.
        let span = 0.00004;
        let near = v.center[1] + 0.00005;
        let far = v.center[1] - 0.00005;
        let width = |y: f64| {
            let a = v.project([v.center[0] - span, y]);
            let b = v.project([v.center[0] + span, y]);
            (b[0] - a[0]).abs()
        };
        assert!(
            width(far) < width(near),
            "far {} should be narrower than near {}",
            width(far),
            width(near)
        );
    }

    /// Geometry that has passed behind the eye must be rejected outright.
    /// Clamping it instead produces a radial fan of smeared lines across the
    /// frame, which reads as a clipping bug but is really a divide by a
    /// near-zero distance.
    #[test]
    fn behind_the_eye_is_rejected() {
        let mut v = vp();
        v.tilt = 0.8;
        v.persp = 0.6;
        // Far enough south to be well behind a camera looking north.
        let (_, depth) = v.project3([v.center[0], v.center[1] + 0.01], 0.0);
        assert!(!depth.is_finite(), "point behind the eye was not rejected");
    }

    #[test]
    fn zoom_stays_in_range() {
        let mut v = vp();
        for _ in 0..100 {
            v.zoom_at(-1.0, [200.0, 100.0]);
        }
        assert_eq!(v.zoom, MIN_ZOOM);
        for _ in 0..100 {
            v.zoom_at(1.0, [200.0, 100.0]);
        }
        assert_eq!(v.zoom, MAX_ZOOM);
    }

    #[test]
    fn fit_frames_the_bounds() {
        let mut v = vp();
        let a = lonlat_to_world(72.79, 18.89);
        let b = lonlat_to_world(72.99, 19.27);
        let bounds = [a[0], a[1], b[0], b[1]];
        v.fit(bounds);

        // Both corners land on screen, and at least one axis nearly fills it.
        let p0 = v.project([bounds[0], bounds[1]]);
        let p1 = v.project([bounds[2], bounds[3]]);
        assert!(p0[0] >= 0.0 && p0[1] >= 0.0, "top-left off screen: {p0:?}");
        assert!(p1[0] <= v.sw && p1[1] <= v.sh, "bottom-right off screen: {p1:?}");
        let fill = ((p1[0] - p0[0]) / v.sw).max((p1[1] - p0[1]) / v.sh);
        assert!(fill > 0.85, "fit left too much slack: {fill}");
    }
}

#[cfg(test)]
mod bounds_tests {
    use super::*;

    fn viewport() -> Viewport {
        let mut v = Viewport::new(lonlat_to_world(72.8777, 19.0760), 12.0);
        v.sw = 400.0;
        v.sh = 200.0;
        v
    }

    /// Turning the camera must not empty the map.
    ///
    /// The two-corner box was symmetric about the centre, so past about fifty
    /// degrees of bearing it inverted -- minx greater than maxx -- and
    /// `visible_in` then admitted only features large enough to span the whole
    /// inverted range. Rotating dropped the roads first and the coastline a few
    /// degrees later. Swept rather than spot-checked, because the angle it
    /// broke at depended on the shape of the viewport.
    #[test]
    fn the_view_box_stays_sane_all_the_way_round() {
        let mut v = viewport();
        for deg in 0..360 {
            v.bearing = (deg as f64).to_radians();
            let b = v.world_bounds(64.0);
            assert!(b[0] <= b[2], "at {deg} degrees minx {} > maxx {}", b[0], b[2]);
            assert!(b[1] <= b[3], "at {deg} degrees miny {} > maxy {}", b[1], b[3]);
            // The centre of the screen is on screen at every bearing, so the
            // box has to contain it. An inverted box passes the two checks
            // above if it is merely empty; this is what catches that.
            let c = v.center;
            assert!(
                c[0] >= b[0] && c[0] <= b[2] && c[1] >= b[1] && c[1] <= b[3],
                "at {deg} degrees the box excludes the point under the crosshair"
            );
        }
    }

    /// And it must actually contain what is drawn, not merely be non-empty.
    ///
    /// Every screen pixel unprojects to a world point that the box has to
    /// admit, or that point is culled while visibly on screen.
    #[test]
    fn everything_on_screen_is_inside_the_view_box() {
        let mut v = viewport();
        for deg in [0, 30, 57, 63, 90, 145, 200, 315] {
            v.bearing = (deg as f64).to_radians();
            let b = v.world_bounds(0.0);
            for (fx, fy) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0), (0.5, 0.5)] {
                let p = v.unproject([v.sw * fx, v.sh * fy]);
                assert!(
                    p[0] >= b[0] && p[0] <= b[2] && p[1] >= b[1] && p[1] <= b[3],
                    "at {deg} degrees the corner ({fx}, {fy}) falls outside the box"
                );
            }
        }
    }
}

#[cfg(test)]
mod behind_the_eye_tests {
    use super::*;

    /// `project` cannot report a point it has no answer for, and callers have
    /// to use `project3` when the camera has a near plane.
    ///
    /// With perspective on, a point behind the eye is rejected rather than
    /// clamped -- pinning it would fling it across the frame in a radial fan.
    /// `project3` says so with an infinite depth beside a NaN position, but
    /// `project` throws the depth away and returns the NaN alone. Every
    /// comparison against a NaN is false, so an on-screen bounds check waves it
    /// straight through, and the NaN travels on into the drawing code.
    #[test]
    fn a_point_behind_the_eye_is_nan_and_project_does_not_say_so() {
        let mut v = Viewport::new(lonlat_to_world(72.8777, 19.0760), 16.0);
        v.sw = 400.0;
        v.sh = 200.0;
        v.tilt = 58f64.to_radians();
        v.persp = 0.85;

        // Somewhere south of the centre is somewhere behind the camera.
        let mut found = None;
        for step in 1..4000 {
            let w = [v.center[0], v.center[1] + step as f64 * 1e-6];
            if !v.project3(w, 0.0).1.is_finite() {
                found = Some(w);
                break;
            }
        }
        let w = found.expect("no point behind the eye was reachable");

        let (p, z) = v.project3(w, 0.0);
        assert!(!z.is_finite(), "project3 should refuse this point");
        assert!(p[0].is_nan() && p[1].is_nan(), "and hand back no position");

        // The trap: the same point through `project`, with the depth gone.
        let q = v.project(w);
        assert!(q[0].is_nan(), "project returns the NaN with nothing to flag it");
        // And this is why a bounds check does not catch it. Written in the
        // negated form on purpose -- clippy dislikes it for exactly the reason
        // being demonstrated, that these values may not be comparable at all,
        // and it is the shape the real check in `draw_labels` had.
        #[allow(clippy::neg_cmp_op_on_partial_ord)]
        {
            assert!(!(q[0] < 0.0), "a NaN is not less than zero");
            assert!(!(q[0] >= v.sw), "nor greater than the screen");
        }
    }
}
