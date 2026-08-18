//! The camera. Sky units in, subpixels out.
//!
//! Deliberately much smaller than termap's `geo.rs`. There is no projection to
//! get right — the sky is flat and its coordinates mean nothing outside this
//! program — so the only real content is the cell-aspect correction and the
//! anchored zoom, both of which are the same here as they are on the map.

/// Height/width ratio of a terminal cell. Braille packs 2x4 dots into a cell,
/// so on a 1:2 cell the dots come out square and no correction is needed.
/// Fonts vary; this is the one knob to turn if the sky looks squashed.
pub const CELL_ASPECT: f64 = 2.0;

/// A braille dot is `cell_w/2` wide and `cell_h/4` tall, so its aspect is
/// `CELL_ASPECT/2`; undistorting means scaling y by the reciprocal. Lands on
/// exactly 1.0 for the usual 1:2 cell.
pub const PIXEL_ASPECT: f64 = 2.0 / CELL_ASPECT;

/// Far enough out to hold the whole sheet with room around it, close enough in
/// to put one constellation across the frame.
pub const MIN_ZOOM: f64 = -2.5;
pub const MAX_ZOOM: f64 = 5.0;

#[derive(Clone, Copy, Debug)]
pub struct View {
    /// Sky coordinate at the centre of the frame.
    pub center: [f64; 2],
    /// Subpixels per sky unit, as a power of two, so a zoom step is a doubling.
    pub zoom: f64,
    /// Canvas size in subpixels, not cells.
    pub sw: f64,
    pub sh: f64,
}

impl View {
    pub fn new() -> Self {
        View { center: [0.0, 0.0], zoom: 1.0, sw: 1.0, sh: 1.0 }
    }

    #[inline]
    pub fn scale(&self) -> f64 {
        2f64.powf(self.zoom)
    }

    #[inline]
    pub fn project(&self, p: [f64; 2]) -> [f64; 2] {
        let s = self.scale();
        [
            (p[0] - self.center[0]) * s + self.sw * 0.5,
            (p[1] - self.center[1]) * s * PIXEL_ASPECT + self.sh * 0.5,
        ]
    }

    #[inline]
    pub fn unproject(&self, s: [f64; 2]) -> [f64; 2] {
        let k = self.scale();
        [
            (s[0] - self.sw * 0.5) / k + self.center[0],
            (s[1] - self.sh * 0.5) / (k * PIXEL_ASPECT) + self.center[1],
        ]
    }

    /// Pan by a screen delta, in subpixels.
    pub fn pan(&mut self, dx: f64, dy: f64) {
        let s = self.scale();
        self.center[0] += dx / s;
        self.center[1] += dy / (s * PIXEL_ASPECT);
    }

    /// Zoom while holding one screen point still.
    ///
    /// Without the anchor, zooming with the wheel walks whatever you were
    /// looking at off the edge of the frame, which makes a wheel feel broken
    /// even though it is doing exactly what it was told.
    pub fn zoom_at(&mut self, anchor: [f64; 2], dz: f64) {
        let before = self.unproject(anchor);
        self.zoom = (self.zoom + dz).clamp(MIN_ZOOM, MAX_ZOOM);
        let after = self.unproject(anchor);
        self.center[0] += before[0] - after[0];
        self.center[1] += before[1] - after[1];
    }

    /// Frame a box, with `pad` subpixels of margin on every side.
    pub fn fit(&mut self, min: [f64; 2], max: [f64; 2], pad: f64) {
        self.center = [(min[0] + max[0]) * 0.5, (min[1] + max[1]) * 0.5];
        let w = (max[0] - min[0]).max(1e-6);
        let h = (max[1] - min[1]).max(1e-6);
        let usable_w = (self.sw - pad * 2.0).max(8.0);
        let usable_h = (self.sh - pad * 2.0).max(8.0);
        let s = (usable_w / w).min(usable_h / (h * PIXEL_ASPECT));
        self.zoom = s.log2().clamp(MIN_ZOOM, MAX_ZOOM);
    }

    /// Centre on a point without changing zoom.
    pub fn look_at(&mut self, p: [f64; 2]) {
        self.center = p;
    }

    /// Put a sky point at a given place on the screen, rather than in the
    /// middle. This is how a constellation is pushed clear of the text: the
    /// camera does not point at the frame's centre, it points wherever the
    /// typography left room.
    pub fn place(&mut self, p: [f64; 2], screen: [f64; 2]) {
        let s = self.scale();
        self.center = [
            p[0] - (screen[0] - self.sw * 0.5) / s,
            p[1] - (screen[1] - self.sh * 0.5) / (s * PIXEL_ASPECT),
        ];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view() -> View {
        View { center: [12.0, -4.0], zoom: 2.25, sw: 320.0, sh: 184.0 }
    }

    #[test]
    fn project_and_unproject_are_inverses() {
        let v = view();
        for p in [[0.0, 0.0], [-80.0, 40.0], [12.0, -4.0], [99.5, -33.25]] {
            let back = v.unproject(v.project(p));
            assert!((back[0] - p[0]).abs() < 1e-9, "{back:?} vs {p:?}");
            assert!((back[1] - p[1]).abs() < 1e-9, "{back:?} vs {p:?}");
        }
    }

    #[test]
    fn the_centre_lands_in_the_middle() {
        let v = view();
        let s = v.project(v.center);
        assert!((s[0] - v.sw * 0.5).abs() < 1e-9);
        assert!((s[1] - v.sh * 0.5).abs() < 1e-9);
    }

    #[test]
    fn zoom_holds_its_anchor_still() {
        let mut v = view();
        let anchor = [40.0, 150.0];
        let before = v.unproject(anchor);
        v.zoom_at(anchor, 0.75);
        let after = v.unproject(anchor);
        assert!((before[0] - after[0]).abs() < 1e-9);
        assert!((before[1] - after[1]).abs() < 1e-9);
    }

    #[test]
    fn zoom_stays_within_its_range() {
        let mut v = view();
        for _ in 0..50 {
            v.zoom_at([0.0, 0.0], 1.0);
        }
        assert_eq!(v.zoom, MAX_ZOOM);
        for _ in 0..100 {
            v.zoom_at([0.0, 0.0], -1.0);
        }
        assert_eq!(v.zoom, MIN_ZOOM);
    }

    #[test]
    fn fit_frames_the_whole_box() {
        let mut v = view();
        let (min, max) = ([-90.0, -70.0], [90.0, 80.0]);
        v.fit(min, max, 12.0);
        for corner in [min, max, [min[0], max[1]], [max[0], min[1]]] {
            let s = v.project(corner);
            assert!(s[0] >= -0.5 && s[0] <= v.sw + 0.5, "{s:?} off in x");
            assert!(s[1] >= -0.5 && s[1] <= v.sh + 0.5, "{s:?} off in y");
        }
    }

    #[test]
    fn place_puts_a_point_where_it_was_asked_to() {
        let mut v = view();
        for target in [[60.0, 90.0], [10.0, 10.0], [300.0, 170.0]] {
            v.place([-33.0, 17.0], target);
            let got = v.project([-33.0, 17.0]);
            assert!((got[0] - target[0]).abs() < 1e-9, "{got:?} vs {target:?}");
            assert!((got[1] - target[1]).abs() < 1e-9, "{got:?} vs {target:?}");
        }
    }

    #[test]
    fn pan_moves_by_exactly_the_screen_delta() {
        let mut v = view();
        let p = [30.0, 10.0];
        let before = v.project(p);
        v.pan(-16.0, 8.0);
        let after = v.project(p);
        assert!((after[0] - (before[0] + 16.0)).abs() < 1e-9);
        assert!((after[1] - (before[1] - 8.0)).abs() < 1e-9);
    }
}
