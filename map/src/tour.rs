//! The experience tour: a camera that flies between places on its own.
//!
//! ## Why the flight is an arc and not a straight line
//!
//! The obvious way to move the camera from one stop to the next is to
//! interpolate centre and zoom independently over a fixed duration. It looks
//! wrong, and it looks wrong in a specific way: at street zoom the ground is
//! moving past at hundreds of screen-widths per second, so the middle of every
//! journey is a grey blur, while the beginning and end crawl. The eye cannot
//! track it and gets no sense of where the two places are relative to each
//! other -- which, for a tour whose whole subject is *where these places are*,
//! throws away the point.
//!
//! What fixes it is to zoom out as you travel and back in as you arrive, along
//! a path chosen so the world moves at a constant *perceived* speed -- measured
//! in screen-widths per second rather than metres per second. Van Wijk and Nuij
//! ("Smooth and Efficient Zooming and Panning", InfoVis 2003) derive the
//! optimal such path in closed form, and `Flight` is that derivation. The arc
//! is not decoration: the altitude at the top of it is exactly the altitude at
//! which both endpoints are comfortably on screen, so the flight *shows you the
//! relationship between the two places* on its way past.
//!
//! ## Why the lean happens last
//!
//! Travel is flat and arrival is three-dimensional. The camera drops to
//! straight-down in the first third of the flight, crosses the region as a
//! plain 2D map -- which is the legible way to see a path across a region --
//! and only tilts up once it has stopped moving. Tilting while travelling
//! muddies both: the tilt reads as wobble, and the motion hides the tilt.
//! Separating them means each one is unmistakable.

use crate::geo::Viewport;
use crate::place::Place;
use crate::view;

/// Van Wijk's ρ: how much zooming out the path will trade for shorter travel.
/// The paper's own user study lands on ~1.42 as the value people prefer; lower
/// makes the arc flatter and the journey longer, higher makes it shoot up.
const RHO: f64 = 1.42;

/// Path length (in ρ-units) covered per second. The paper calls this V. It sets
/// the pace of every flight at once — the whole point of the parametrisation is
/// that one number is enough.
const SPEED: f64 = 2.6;

/// Even a very long flight should not outstay its welcome, and even a hop
/// across town needs long enough to read as movement rather than a cut.
const MIN_FLIGHT: f64 = 1.5;
const MAX_FLIGHT: f64 = 4.5;

/// How long the world takes to rise into 2.5D once the camera has stopped.
const SETTLE: f64 = 1.15;

/// The opening descent is stretched against a normal hop, and allowed to run
/// past `MAX_FLIGHT`. Every other flight begins somewhere the viewer has
/// already been given time to read; this one begins at a whole country, and it
/// is the only chance to establish where in the world any of this is.
const OPEN_STRETCH: f64 = 1.7;
const OPEN_MAX: f64 = 6.0;

/// Fraction of the flight spent flattening out at the start.
const LEVEL_BY: f64 = 0.35;

/// Van Wijk & Nuij's optimal zoom/pan path between two (centre, width) states.
///
/// `width` is the viewport's width in world units — the same units as the
/// centre, which is what makes the two axes commensurable and the whole
/// derivation possible.
#[derive(Clone, Copy, Debug)]
pub struct Flight {
    c0: [f64; 2],
    d: [f64; 2],
    w0: f64,
    w1: f64,
    /// Ground distance in world units. Zero for a pure zoom.
    u1: f64,
    r0: f64,
    /// Total path length, ρ-units.
    pub s: f64,
}

impl Flight {
    pub fn new(c0: [f64; 2], w0: f64, c1: [f64; 2], w1: f64) -> Flight {
        let d = [c1[0] - c0[0], c1[1] - c0[1]];
        let u1 = (d[0] * d[0] + d[1] * d[1]).sqrt();

        // Below about a thousandth of a screen width the two centres are the
        // same place, and the general solution divides by u1. Degenerating to a
        // pure exponential zoom is not an approximation — it is the limit.
        if u1 < w0 * 1e-3 {
            return Flight { c0, d, w0, w1, u1: 0.0, r0: 0.0, s: (w1 / w0).ln().abs() / RHO };
        }

        let (rho2, rho4) = (RHO * RHO, RHO * RHO * RHO * RHO);
        let dw = w1 * w1 - w0 * w0;
        let b0 = (dw + rho4 * u1 * u1) / (2.0 * w0 * rho2 * u1);
        let b1 = (dw - rho4 * u1 * u1) / (2.0 * w1 * rho2 * u1);
        // ln(sqrt(b²+1) − b) rather than the algebraically equal −asinh(b):
        // written this way it stays accurate for large |b|, which is exactly
        // the case here (b grows with the distance in screen widths).
        let r0 = ((b0 * b0 + 1.0).sqrt() - b0).ln();
        let r1 = ((b1 * b1 + 1.0).sqrt() - b1).ln();

        Flight { c0, d, w0, w1, u1, r0, s: (r1 - r0) / RHO }
    }

    /// Seconds this flight should take.
    pub fn duration(&self) -> f64 {
        (self.s / SPEED).clamp(MIN_FLIGHT, MAX_FLIGHT)
    }

    /// State at normalised progress `f` in 0..1. Returns centre and width.
    pub fn at(&self, f: f64) -> ([f64; 2], f64) {
        let f = f.clamp(0.0, 1.0);

        if self.u1 == 0.0 {
            // Geometric in width, which is linear in zoom — so a pure zoom
            // still changes scale at a constant perceived rate.
            let w = self.w0 * (self.w1 / self.w0).powf(f);
            return ([self.c0[0] + self.d[0] * f, self.c0[1] + self.d[1] * f], w);
        }

        let s = f * self.s;
        let cosh_r0 = self.r0.cosh();
        let x = RHO * s + self.r0;
        // u is the fraction of the way along the ground track, not a distance:
        // it is already divided by u1, which is why d can be scaled by it.
        let u = self.w0 / (RHO * RHO * self.u1) * (cosh_r0 * x.tanh() - self.r0.sinh());
        let w = self.w0 * cosh_r0 / x.cosh();
        ([self.c0[0] + self.d[0] * u, self.c0[1] + self.d[1] * u], w)
    }
}

/// Viewport width in world units. The bridge between zoom (what the camera
/// holds) and width (what the flight is derived in).
pub fn width_of(vp: &Viewport) -> f64 {
    vp.sw / vp.scale()
}

pub fn zoom_of(w: f64, sw: f64) -> f64 {
    (sw / (256.0 * w.max(1e-12)))
        .log2()
        .clamp(crate::geo::MIN_ZOOM, crate::geo::MAX_ZOOM)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Phase {
    /// The camera belongs to whoever last touched it.
    Rest,
    Flying,
    /// Arrived; the ground is rising into 3D.
    Settling,
}

pub struct Tour {
    pub places: Vec<Place>,
    /// Where the camera is going, or where it is.
    pub at: usize,
    /// Which stop the card is showing. Lags `at` across the first half of a
    /// flight so the caption fades out on the place you are leaving rather than
    /// switching to the new name over the old view.
    shown: usize,
    pub active: bool,
    phase: Phase,
    /// Seconds into the current phase.
    t: f64,
    dur: f64,
    flight: Option<Flight>,
    from_tilt: f64,
    from_bearing: f64,
}

impl Tour {
    pub fn new(places: Vec<Place>) -> Tour {
        Tour {
            places,
            at: 0,
            shown: 0,
            active: false,
            phase: Phase::Rest,
            t: 0.0,
            dur: 0.0,
            flight: None,
            from_tilt: 0.0,
            from_bearing: 0.0,
        }
    }

    /// True while the camera is under the tour's control and still moving, so
    /// the event loop knows it has to keep drawing.
    pub fn moving(&self) -> bool {
        self.phase != Phase::Rest
    }

    /// Begin a flight to `i` from wherever the camera currently is.
    pub fn go(&mut self, vp: &Viewport, i: usize) {
        let Some(p) = self.places.get(i) else { return };
        let target_w = vp.sw / (256.0 * 2f64.powf(p.zoom));
        let f = Flight::new(vp.center, width_of(vp), p.world, target_w);

        self.shown = self.at;
        self.at = i;
        self.flight = Some(f);
        self.dur = f.duration();
        self.t = 0.0;
        self.phase = Phase::Flying;
        self.from_tilt = vp.tilt;
        self.from_bearing = vp.bearing;
        self.active = true;
    }

    pub fn next(&mut self, vp: &Viewport) {
        if self.places.is_empty() {
            return;
        }
        self.go(vp, (self.at + 1) % self.places.len());
    }

    pub fn prev(&mut self, vp: &Viewport) {
        if self.places.is_empty() {
            return;
        }
        let n = self.places.len();
        self.go(vp, (self.at + n - 1) % n);
    }

    /// Open the tour: fly in from whatever wide view the camera has been put
    /// on, rather than cutting to the first stop.
    ///
    /// It is the same flight as any other, with two differences. It is stretched,
    /// because it covers ten zoom levels and there is nothing before it to
    /// establish the geography. And there is no place to fade *out* of, so the
    /// caption stays away for the whole descent and only arrives when the ground
    /// does — `shown` is deliberately out of range to say "nothing".
    pub fn open(&mut self, vp: &Viewport, i: usize) {
        self.go(vp, i);
        self.dur = (self.dur * OPEN_STRETCH).min(OPEN_MAX);
        self.shown = usize::MAX;
    }

    /// Advance by `dt` seconds, writing the camera. Returns true if it moved.
    pub fn tick(&mut self, dt: f64, vp: &mut Viewport) -> bool {
        if self.phase == Phase::Rest {
            return false;
        }
        self.t += dt;
        let Some(p) = self.places.get(self.at).cloned() else {
            self.phase = Phase::Rest;
            return false;
        };
        let f = (self.t / self.dur.max(1e-6)).clamp(0.0, 1.0);

        match self.phase {
            Phase::Flying => {
                let flight = self.flight.expect("flying without a flight");
                let (c, w) = flight.at(ease(f));
                vp.center = c;
                vp.zoom = zoom_of(w, vp.sw);

                // Flat by a third of the way, and flat for the rest of the
                // trip. See the module note: travel is 2D, arrival is not.
                let level = 1.0 - ease((f / LEVEL_BY).min(1.0));
                vp.tilt = self.from_tilt * level;
                vp.bearing = angle_lerp(self.from_bearing, p.bearing, ease(f));
                vp.persp = 0.0;

                if f >= 1.0 {
                    self.phase = Phase::Settling;
                    self.t = 0.0;
                    self.dur = SETTLE;
                    // Both of these, not just the tilt. The settle re-runs the
                    // bearing interpolation, so leaving `from_bearing` at the
                    // value the *flight* started from snaps the camera back to
                    // where it took off and rotates it round a second time.
                    self.from_tilt = vp.tilt;
                    self.from_bearing = vp.bearing;
                }
            }
            Phase::Settling => {
                let k = ease(f);
                vp.tilt = self.from_tilt + (p.tilt - self.from_tilt) * k;
                vp.bearing = angle_lerp(self.from_bearing, p.bearing, k);
                // Convergence arrives with the lean rather than independently:
                // perspective on a flat map only distorts it.
                let lean = if p.tilt > 1e-6 { (vp.tilt / p.tilt).clamp(0.0, 1.0) } else { 0.0 };
                vp.persp = view::auto_persp(vp.zoom) * lean;
                if f >= 1.0 {
                    self.phase = Phase::Rest;
                    self.shown = self.at;
                }
            }
            Phase::Rest => {}
        }
        true
    }

    /// Which stop the caption should show, and how opaque it should be.
    ///
    /// The card is absent for the middle of a flight. Holding it up while the
    /// world rushes past underneath asks the reader to do two things at once,
    /// and the name means nothing until you can see what it is attached to.
    pub fn card(&self) -> Option<(&Place, f32)> {
        if !self.active {
            return None;
        }
        match self.phase {
            Phase::Rest => self.places.get(self.at).map(|p| (p, 1.0)),
            Phase::Settling => {
                let f = (self.t / self.dur.max(1e-6)).clamp(0.0, 1.0);
                self.places.get(self.at).map(|p| (p, ease(f) as f32))
            }
            Phase::Flying => {
                let f = (self.t / self.dur.max(1e-6)).clamp(0.0, 1.0);
                const OUT: f64 = 0.20;
                const IN: f64 = 0.62;
                if f < OUT {
                    let a = 1.0 - ease(f / OUT);
                    self.places.get(self.shown).map(|p| (p, a as f32))
                } else if f > IN {
                    let a = ease((f - IN) / (1.0 - IN));
                    self.places.get(self.at).map(|p| (p, a as f32))
                } else {
                    None
                }
            }
        }
    }
}

/// Smootherstep. Van Wijk's parametrisation already gives constant perceived
/// velocity along the path, so this is only here to take the corners off the
/// start and stop — without it the flight begins and ends with a visible jerk.
fn ease(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

/// Interpolate the short way round. Straight lerp from 350° to 10° takes the
/// camera the long way through 180° and spins the world backwards.
fn angle_lerp(a: f64, b: f64, t: f64) -> f64 {
    use std::f64::consts::{PI, TAU};
    let d = (b - a).rem_euclid(TAU);
    let d = if d > PI { d - TAU } else { d };
    a + d * t
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vp_at(center: [f64; 2], zoom: f64) -> Viewport {
        let mut vp = Viewport::new(center, zoom);
        vp.sw = 400.0;
        vp.sh = 176.0;
        vp
    }

    #[test]
    fn a_flight_starts_and_ends_where_it_was_told_to() {
        let (c0, c1) = ([0.70, 0.42], [0.71, 0.43]);
        let f = Flight::new(c0, 1e-4, c1, 5e-5);
        let (a, wa) = f.at(0.0);
        let (b, wb) = f.at(1.0);
        for i in 0..2 {
            assert!((a[i] - c0[i]).abs() < 1e-12, "start {i}: {a:?}");
            assert!((b[i] - c1[i]).abs() < 1e-9, "end {i}: {b:?}");
        }
        assert!((wa - 1e-4).abs() < 1e-12);
        assert!((wb - 5e-5).abs() < 1e-9);
    }

    /// The whole reason for the arc. A long hop at street zoom must climb, and
    /// climb to roughly the altitude that frames both endpoints.
    #[test]
    fn a_long_flight_climbs_and_a_short_one_barely_does() {
        let w = 2.4e-5; // ~zoom 16 on a 200-column terminal
        let far = Flight::new([0.700, 0.420], w, [0.7016, 0.4192], w);
        let peak = (0..=40).map(|i| far.at(i as f64 / 40.0).1).fold(0.0, f64::max);
        assert!(peak > 8.0 * w, "long flight did not climb: {:.1}x", peak / w);

        let near = Flight::new([0.700, 0.420], w, [0.70002, 0.42001], w);
        let peak = (0..=40).map(|i| near.at(i as f64 / 40.0).1).fold(0.0, f64::max);
        assert!(peak < 3.0 * w, "short flight over-climbed: {:.1}x", peak / w);
    }

    /// The closed form should actually be the geodesic it claims to be.
    ///
    /// Differentiating the solution gives du/ds = w·sech(r)/ρ and
    /// dw/ds = −ρ·w·tanh(r), so
    ///
    ///     ρ²(du/w)² + (dw/(ρw))²  =  sech²(r) + tanh²(r)  =  1
    ///
    /// — that combination of pan rate and zoom rate is the invariant, and
    /// sampling the path at even intervals must hold it constant. (Note it is
    /// *not* the sum of "screen widths panned" and "log units zoomed": those
    /// trade against each other through ρ, which is the whole idea.)
    #[test]
    fn the_path_holds_van_wijks_invariant() {
        let w = 2.4e-5;
        let f = Flight::new([0.700, 0.420], w, [0.7016, 0.4192], w);
        const N: usize = 200;
        let mut steps = Vec::new();
        let mut prev = f.at(0.0);
        for i in 1..=N {
            let cur = f.at(i as f64 / N as f64);
            let du = ((cur.0[0] - prev.0[0]).powi(2) + (cur.0[1] - prev.0[1]).powi(2)).sqrt();
            let mid_w = 0.5 * (cur.1 + prev.1);
            let pan = RHO * du / mid_w;
            let zoom = (cur.1 / prev.1).ln() / RHO;
            steps.push((pan * pan + zoom * zoom).sqrt());
            prev = cur;
        }
        let max = steps.iter().cloned().fold(0.0, f64::max);
        let min = steps.iter().cloned().fold(f64::MAX, f64::min);
        assert!(max / min < 1.02, "invariant drifts {:.3}x", max / min);
    }

    /// The property this whole module exists to buy, stated against the thing
    /// it replaces: interpolating centre and zoom independently. Both cover the
    /// same journey in the same time; only one of them is watchable.
    #[test]
    fn the_arc_beats_interpolating_centre_and_zoom_independently() {
        let (c0, c1) = ([0.700, 0.420], [0.7016, 0.4192]);
        let w = 2.4e-5;
        let f = Flight::new(c0, w, c1, w);
        const N: usize = 200;

        // Screen widths of ground crossed per step, which is what the eye has
        // to keep up with.
        let peak = |at: &dyn Fn(f64) -> ([f64; 2], f64)| {
            let mut worst: f64 = 0.0;
            let mut prev = at(0.0);
            for i in 1..=N {
                let cur = at(i as f64 / N as f64);
                let d = ((cur.0[0] - prev.0[0]).powi(2) + (cur.0[1] - prev.0[1]).powi(2)).sqrt();
                worst = worst.max(d / prev.1);
                prev = cur;
            }
            worst
        };

        let naive = |t: f64| {
            // Linear in centre, linear in zoom — the obvious implementation.
            ([c0[0] + (c1[0] - c0[0]) * t, c0[1] + (c1[1] - c0[1]) * t], w)
        };
        let arc = peak(&|t| f.at(t));
        let flat = peak(&naive);
        assert!(
            flat / arc > 6.0,
            "arc {arc:.3} vs naive {flat:.3} screen widths per step — only {:.1}x better",
            flat / arc
        );
    }

    #[test]
    fn a_pure_zoom_does_not_divide_by_zero() {
        let c = [0.7, 0.42];
        let f = Flight::new(c, 1e-4, c, 1e-5);
        for i in 0..=10 {
            let (p, w) = f.at(i as f64 / 10.0);
            assert!(p[0].is_finite() && p[1].is_finite() && w.is_finite());
        }
        assert!((f.at(1.0).1 - 1e-5).abs() < 1e-12);
    }

    #[test]
    fn width_and_zoom_are_inverses() {
        let vp = vp_at([0.7, 0.42], 14.25);
        assert!((zoom_of(width_of(&vp), vp.sw) - 14.25).abs() < 1e-9);
    }

    #[test]
    fn a_tour_lands_on_the_place_it_was_sent_to() {
        let places = crate::place::parse(include_str!("../data/places.txt")).unwrap();
        let target = places[3].clone();
        let mut tour = Tour::new(places);
        let mut vp = vp_at([0.70, 0.42], 12.0);

        tour.go(&vp, 3);
        // Run to completion at a fixed step — deterministic, no wall clock.
        for _ in 0..600 {
            if !tour.tick(0.05, &mut vp) {
                break;
            }
        }
        assert!(!tour.moving(), "tour never came to rest");
        assert!((vp.center[0] - target.world[0]).abs() < 1e-9, "{:?}", vp.center);
        assert!((vp.center[1] - target.world[1]).abs() < 1e-9, "{:?}", vp.center);
        assert!((vp.zoom - target.zoom).abs() < 1e-6, "zoom {}", vp.zoom);
        assert!((vp.tilt - target.tilt).abs() < 1e-6, "tilt {}", vp.tilt);
        assert!((vp.bearing - target.bearing).abs() < 1e-6, "bearing {}", vp.bearing);
    }

    /// Travel is flat and arrival is not. If this stops holding, the two
    /// motions have started competing again.
    #[test]
    fn the_camera_is_flat_while_it_travels_and_leaning_when_it_stops() {
        let places = crate::place::parse(include_str!("../data/places.txt")).unwrap();
        let mut tour = Tour::new(places);
        let mut vp = vp_at([0.70, 0.42], 16.0);
        vp.tilt = 0.9;

        tour.go(&vp, 0);
        let mut mid_tilt = f64::MAX;
        let mut t = 0.0;
        while tour.moving() {
            tour.tick(0.05, &mut vp);
            t += 0.05;
            // Sample the middle of the flight only.
            if t > tour.dur * 0.5 && tour.phase == Phase::Flying {
                mid_tilt = mid_tilt.min(vp.tilt);
            }
        }
        assert!(mid_tilt.abs() < 1e-9, "camera was leaning mid-flight: {mid_tilt}");
        assert!(vp.tilt > 0.6, "camera never leaned on arrival: {}", vp.tilt);
    }

    #[test]
    fn the_caption_is_absent_while_the_world_rushes_past() {
        let places = crate::place::parse(include_str!("../data/places.txt")).unwrap();
        let mut tour = Tour::new(places);
        let mut vp = vp_at([0.70, 0.42], 16.0);
        tour.open(&vp, 0);
        while tour.moving() {
            tour.tick(0.05, &mut vp);
        }

        tour.go(&vp, 2);
        let mut saw_gap = false;
        while tour.moving() {
            tour.tick(0.05, &mut vp);
            if tour.card().is_none() {
                saw_gap = true;
            }
        }
        assert!(saw_gap, "the card never got out of the way");
        let (p, a) = tour.card().expect("card returns at rest");
        assert_eq!(p.id, "silver-oak");
        assert!((a - 1.0).abs() < 1e-6);
    }

    /// The camera must not jump when one phase hands over to the next. This
    /// caught a real snap: the settle restarted the bearing interpolation from
    /// the angle the flight took off at, so the world spun back to north and
    /// then rotated a second time.
    #[test]
    fn nothing_jumps_where_the_phases_meet() {
        let places = crate::place::parse(include_str!("../data/places.txt")).unwrap();
        let mut tour = Tour::new(places);
        let mut vp = vp_at([0.70, 0.42], 16.0);
        vp.bearing = 0.9;
        vp.tilt = 0.5;

        tour.go(&vp, 0);
        let (mut last_b, mut last_t, mut last_z) = (vp.bearing, vp.tilt, vp.zoom);
        let (mut wb, mut wt, mut wz) = (0.0f64, 0.0f64, 0.0f64);
        const DT: f64 = 0.02;
        while tour.moving() {
            tour.tick(DT, &mut vp);
            let db = (vp.bearing - last_b).rem_euclid(std::f64::consts::TAU);
            let db = if db > std::f64::consts::PI { db - std::f64::consts::TAU } else { db };
            wb = wb.max(db.abs());
            wt = wt.max((vp.tilt - last_t).abs());
            wz = wz.max((vp.zoom - last_z).abs());
            (last_b, last_t, last_z) = (vp.bearing, vp.tilt, vp.zoom);
        }
        // Per 20 ms tick. A discontinuity shows up as a step an order of
        // magnitude above the smooth motion around it.
        assert!(wb < 0.05, "bearing jumped {:.3} rad in one tick", wb);
        assert!(wt < 0.05, "tilt jumped {:.3} rad in one tick", wt);
        assert!(wz < 0.35, "zoom jumped {:.3} levels in one tick", wz);
    }

    #[test]
    fn the_short_way_round_is_the_way_round() {
        use std::f64::consts::PI;
        let a = 350f64.to_radians();
        let b = 10f64.to_radians();
        let mid = angle_lerp(a, b, 0.5);
        // Should pass through 0/360, not through 180.
        assert!((mid - 2.0 * PI).abs() < 1e-9 || mid.abs() < 1e-9, "{}", mid.to_degrees());
    }
}
