//! The creature: a field, jelly physics, and one eye.
//!
//! A blob has no canonical silhouette, so it cannot be drawn *wrong*. That frees
//! the outline to deform as hard as the physics wants and still be in character.
//!
//! One eye rather than two, because a pair at this scale has to match exactly or
//! it reads as damage. One eye can never be asymmetric, and it puts all the
//! contrast into a single landmark instead of halving it.
//!
//! # Where the animation principles actually live
//!
//! Almost none of them are separate features. One **velocity-aligned
//! deformation** with its own spring gives squash and stretch, smear and
//! follow-through at once: kicked negative by an impact it pinches across the
//! direction of travel, driven by speed it draws out along it, and being a spring
//! it overshoots and rings rather than snapping. That single mechanism is most of
//! the list.
//!
//! The rest is timing, and timing lives in the gait machine: a wind-up before
//! every shove, a longer one before a reversal, a held over-strong pose on
//! landing, a crouch before the wings take over, a lean into the turn, and the
//! eye arriving before the body does. Slow-in and slow-out are what the springs
//! already do, so they are never written down anywhere.

use std::ops::{Add, Mul, Sub};

/// Visual units per sample. A cell is eight units wide and sixteen tall, the
/// 1:2 shape of a real cell, sampled eight ways each direction — so a sample is
/// one unit across and two down. Distances are in visual units, so a circle is
/// round on screen.
pub const PX: f32 = 1.0;
pub const PY: f32 = 2.0;

/// Resting radius, in visual units — sixteen cells across and eight tall.
pub const CORE: f32 = 64.0;
const EYE_R: f32 = 24.0;
const GLINT_R: f32 = 7.5;

/// How far the outline strays from a circle, and in how many lobes. Xiaohei is
/// specified as slightly irregular and hand-drawn; a bean that is exactly a bean
/// reads as a logo. The phase is fixed per creature, so the lumpiness belongs to
/// it rather than being an animation.
const RIPPLE: f32 = 0.075;
const LOBES: f32 = 3.0;
const WELD: f32 = 24.0;

const GRAVITY: f32 = 1250.0;
/// Nearly all of an impact is absorbed. Sand, not rubber.
const BOUNCE: f32 = 0.13;
const GROUND_DRAG: f32 = 5.0;
/// Rolling resistance, which is a fraction of the friction of shoving a blob
/// along. That difference is the entire reason a thing that can roll, rolls.
const ROLL_DRAG: f32 = 0.4;
const ROLL_TORQUE: f32 = 2400.0;
/// Terminal speed of a roll — about eight body-widths a second, which is fast
/// enough to be obviously the quick way to travel.
const ROLL_TOP: f32 = 600.0;
const AIR_DRAG: f32 = 0.5;

/// Under-damped on purpose. The ringing is the jelly.
const DEF_STIFF: f32 = 105.0;
const DEF_DAMP: f32 = 6.2;

const LURCH_GAP: f32 = 0.34;
const LURCH_RUN: f32 = 175.0;
const LURCH_UP: f32 = 225.0;
/// The wind-up. Short enough to read as a gather rather than a pause.
const WIND: f32 = 0.10;
/// A reversal is a bigger event than a step, so it gathers for longer.
const WIND_TURN: f32 = 0.22;
/// How long the exaggerated landing pose is held before the spring resumes.
const IMPACT: f32 = 0.09;

const SLACK: f32 = 40.0;
/// Past this it stops walking, shuts its eye and rolls.
const ROLL_AT: f32 = CORE * 2.6;
/// A stretch toward something just out of reach — short, because a long one
/// stops looking like a body and starts looking like a tentacle.
const STRETCH: f32 = 26.0;
/// Seconds of failing to reach before it gives up and takes off.
const PATIENCE: f32 = 1.1;
/// Below this speed it counts as stopped.
const STILL: f32 = 3.0;
/// Speed at which motion starts leaving something behind it.
const SMEAR_AT: f32 = 150.0;

#[derive(Clone, Copy, Debug, Default)]
pub struct Vec2 {
    pub x: f32,
    pub y: f32,
}

pub const fn v2(x: f32, y: f32) -> Vec2 {
    Vec2 { x, y }
}

impl Add for Vec2 {
    type Output = Vec2;
    fn add(self, o: Vec2) -> Vec2 {
        v2(self.x + o.x, self.y + o.y)
    }
}

impl Sub for Vec2 {
    type Output = Vec2;
    fn sub(self, o: Vec2) -> Vec2 {
        v2(self.x - o.x, self.y - o.y)
    }
}

impl Mul<f32> for Vec2 {
    type Output = Vec2;
    fn mul(self, k: f32) -> Vec2 {
        v2(self.x * k, self.y * k)
    }
}

impl Vec2 {
    fn dot(self, o: Vec2) -> f32 {
        self.x * o.x + self.y * o.y
    }

    pub fn len(self) -> f32 {
        self.dot(self).sqrt()
    }

    fn norm(self) -> Vec2 {
        let l = self.len();
        if l == 0.0 {
            self
        } else {
            self * (1.0 / l)
        }
    }

    fn perp(self) -> Vec2 {
        v2(-self.y, self.x)
    }
}

fn smin(a: f32, b: f32, k: f32) -> f32 {
    let h = (k - (a - b).abs()).max(0.0) / k;
    a.min(b) - h * h * k * 0.25
}

fn capsule(p: Vec2, a: Vec2, b: Vec2, ra: f32, rb: f32) -> f32 {
    let ba = b - a;
    let t = ((p - a).dot(ba) / ba.dot(ba).max(0.0001)).clamp(0.0, 1.0);
    (p - (a + ba * t)).len() - (ra + (rb - ra) * t)
}

/// How the face is set. The pupil is not where expression lives — a hole with a
/// dot in it reads the same wherever the dot sits. Acting comes from the
/// aperture: what the lids cover, what angle they cut at, and how they bow.
#[derive(Clone, Copy)]
pub struct Face {
    pub open: f32,
    pub lid_top: f32,
    pub lid_low: f32,
    pub tilt: f32,
    /// Bows the lids. Positive dips the upper lid in the middle, the shape of a
    /// scowl; negative lifts the lower one into the arc a smile makes of an eye.
    pub arc: f32,
    pub glint: f32,
}

impl Default for Face {
    fn default() -> Self {
        Face { open: 1.0, lid_top: 0.0, lid_low: 0.0, tilt: 0.0, arc: 0.0, glint: 1.0 }
    }
}

/// What the host wants of it this tick.
#[derive(Clone, Copy)]
pub struct Drive {
    pub target: Option<Vec2>,
    pub focus: Option<Vec2>,
    pub face: Face,
    /// Mood in the silhouette rather than the face: positive draws it up tall and
    /// narrow, negative settles it wide and low. The body is half the expression.
    pub carriage: f32,
    pub wobble: bool,
    pub blink: bool,
    pub floor: f32,
}

impl Default for Drive {
    fn default() -> Self {
        Drive {
            target: None,
            focus: None,
            face: Face::default(),
            carriage: 0.0,
            wobble: true,
            blink: true,
            floor: f32::MAX,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Gait {
    Rest,
    /// Gathering itself before a shove, a reversal, or a take-off.
    Wind,
    Trudge,
    /// Eye shut, tucked round, covering ground fast.
    Roll,
    /// Coming out of a roll. A ball at speed cannot stop on the spot, so it digs
    /// in and slews, which is the follow-through the roll has earned.
    Skid,
    /// The leap that starts a flight. The wings stay folded until it is off the
    /// ground — unfurling them mid-stride and floating up reads as a cheat.
    Leap,
    Fly,
}

pub struct Blob {
    pos: Vec2,
    vel: Vec2,
    look: Vec2,
    look_vel: Vec2,
    /// Rolled angle of the body frame. The face does not use it, so the mass
    /// tumbles under a steady eye.
    spin: f32,
    /// Velocity-aligned deformation, with its own spring. Negative pinches across
    /// the direction of travel, positive draws out along it.
    def: f32,
    def_vel: f32,
    def_dir: Vec2,
    /// Seconds of held pose left. While this runs the spring is suspended.
    hold: f32,
    /// Lean into the turn.
    bank: f32,
    /// Which way it currently considers itself facing.
    heading: f32,
    /// 0 bean, 1 tucked into a ball.
    tuck: f32,
    /// Where it recently was, for the multiples that stand in for a smear.
    ghost: [Vec2; 2],
    strain: f32,
    baffled: f32,
    wing: f32,
    flap: f32,
    carriage: f32,
    /// Where the carriage is headed. Compared against, not against zero — a
    /// resting posture is rarely upright, and testing it against zero reports a
    /// settled body as permanently in motion.
    carriage_want: f32,
    grain: f32,
    wobble: f32,
    face: Face,
    gait: Gait,
    since: f32,
    clock: f32,
    lurch_at: f32,
    grounded: bool,
    rng: u32,
    blink_at: f32,
    blink_from: f32,
    blink_until: f32,
}

impl Blob {
    pub fn new(at: Vec2, seed: u64) -> Blob {
        Blob {
            pos: at,
            vel: Vec2::default(),
            look: Vec2::default(),
            look_vel: Vec2::default(),
            spin: 0.0,
            def: 0.0,
            def_vel: 0.0,
            def_dir: v2(1.0, 0.0),
            hold: 0.0,
            bank: 0.0,
            heading: 1.0,
            tuck: 0.0,
            ghost: [at, at],
            strain: 0.0,
            baffled: 0.0,
            wing: 0.0,
            flap: 0.0,
            carriage: 0.0,
            carriage_want: 0.0,
            grain: (seed % 619) as f32 * 0.0101,
            wobble: 0.0,
            face: Face::default(),
            gait: Gait::Rest,
            since: 0.0,
            clock: 0.0,
            lurch_at: 0.0,
            grounded: false,
            rng: (seed as u32) | 1,
            blink_at: 1.4,
            blink_from: 0.0,
            blink_until: 0.0,
        }
    }

    fn next(&mut self) -> u32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 17;
        self.rng ^= self.rng << 5;
        self.rng
    }

    fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (self.next() % 1024) as f32 / 1024.0 * (hi - lo)
    }

    pub fn at(&self) -> Vec2 {
        self.pos
    }

    pub fn speed(&self) -> f32 {
        self.vel.len()
    }

    pub fn blinking(&self) -> bool {
        self.clock < self.blink_until
    }

    /// How shut the lid is, 0 open and 1 closed.
    ///
    /// Snapping between the two was the whole problem: a blink is a *move*, and
    /// it closes about twice as fast as it opens. Two frames of each is enough to
    /// read, and without them the eye just flickers.
    fn wink(&self) -> f32 {
        if self.clock >= self.blink_until {
            return 0.0;
        }
        let span = (self.blink_until - self.blink_from).max(0.0001);
        let t = ((self.clock - self.blink_from) / span).clamp(0.0, 1.0);
        if t < 0.34 {
            t / 0.34
        } else {
            1.0 - (t - 0.34) / 0.66
        }
    }

    fn shut_at(&mut self, secs: f32) {
        self.blink_from = self.clock;
        self.blink_until = self.clock + secs;
    }

    pub fn flying(&self) -> bool {
        matches!(self.gait, Gait::Fly | Gait::Leap)
    }

    pub fn rolling(&self) -> bool {
        self.gait == Gait::Roll
    }

    pub fn pin(&mut self, at: Vec2) {
        self.pos = at;
        self.vel = Vec2::default();
        self.ghost = [at, at];
    }

    pub fn kick(&mut self, push: Vec2) {
        self.vel = self.vel + push;
        self.def_dir = push.norm();
        self.shock(-(push.len() * 0.0022).min(0.5), IMPACT);
    }

    /// Slams the deformation to a value and holds it there. This is the impact
    /// frame: animators hold one over-strong pose rather than easing through it,
    /// because the eye reads the extreme and not the path between them.
    fn shock(&mut self, amount: f32, hold: f32) {
        self.def = amount.clamp(-0.6, 0.6);
        self.def_vel = 0.0;
        self.hold = hold;
    }

    pub fn crown(&self) -> Vec2 {
        self.pos - v2(0.0, CORE * (1.0 + self.def.max(0.0) * 0.4) + self.strain)
    }

    pub fn restless(&self) -> bool {
        let travelling = if self.grounded { self.vel.x.abs() } else { self.vel.len() };
        travelling > STILL
            || self.gait != Gait::Rest
            || self.look_vel.len() > 1.4
            || self.def.abs() > 0.008
            || self.def_vel.abs() > 0.05
            || self.bank.abs() > 0.004
            || self.tuck > 0.01
            || self.strain > 0.6
            || self.wing > 0.02
            || self.blinking()
            || self.settling()
    }

    fn settling(&self) -> bool {
        let f = &self.face;
        f.lid_top > 0.01 && f.lid_top < 0.99
            || f.arc.abs() > 0.01 && f.arc.abs() < 0.99
            || f.open < 0.98 && f.open > 0.03
            || (self.carriage - self.carriage_want).abs() > 0.01
    }

    fn enter(&mut self, gait: Gait) {
        if self.gait != gait {
            self.gait = gait;
            self.since = 0.0;
        }
    }

    pub fn tick(&mut self, dt: f32, drive: &Drive) {
        self.clock += dt;
        self.since += dt;
        if drive.wobble {
            self.wobble += dt;
        }

        let k = 1.0 - (-dt * 9.0).exp();
        let w = drive.face;
        let f = &mut self.face;
        f.open += (w.open - f.open) * (1.0 - (-dt * 14.0).exp());
        f.lid_top += (w.lid_top - f.lid_top) * k;
        f.lid_low += (w.lid_low - f.lid_low) * k;
        f.tilt += (w.tilt - f.tilt) * k;
        f.arc += (w.arc - f.arc) * k;
        f.glint += (w.glint - f.glint) * k;
        self.carriage_want = drive.carriage;
        self.carriage += (drive.carriage - self.carriage) * k;

        let run = drive.target.map(|t| t.x - self.pos.x).unwrap_or(0.0);
        let gap = drive.target.map(|t| (t - self.pos).len()).unwrap_or(0.0);
        let overhead = drive.target.is_some_and(|t| t.y < drive.floor - CORE * 1.6);
        if overhead {
            self.baffled += dt;
        } else {
            self.baffled = (self.baffled - dt * 2.5).max(0.0);
        }

        self.step_gait(dt, drive.target, run, gap, overhead);

        self.vel.y += GRAVITY * (1.0 - self.wing * 0.985) * dt;
        self.vel = self.vel * (1.0 - AIR_DRAG * dt);
        self.pos = self.pos + self.vel * dt;

        self.grounded = self.wing < 0.3 && self.pos.y >= drive.floor - 2.0;
        if self.pos.y >= drive.floor && self.wing < 0.3 {
            let hit = self.vel.y;
            self.pos.y = drive.floor;
            self.vel.y = if hit > 150.0 { -hit * BOUNCE } else { 0.0 };
            let friction = if self.gait == Gait::Roll { ROLL_DRAG } else { GROUND_DRAG };
            self.vel.x *= 1.0 - (friction * dt).min(0.9);
            if hit > 190.0 {
                // Impact frame, plus a flinch. Screwing its eye shut on a hard
                // landing costs one line and is most of what sells the weight.
                self.def_dir = v2(0.0, 1.0);
                self.shock(-(hit * 0.0026).min(0.58), IMPACT);
                self.shut_at(0.16);
            }
        }

        self.ghost[1] = self.ghost[0];
        self.ghost[0] = self.ghost[0] + (self.pos - self.ghost[0]) * (1.0 - (-dt * 22.0).exp());

        // Rolling without slipping. Tucked up it turns faster, as a ball that
        // size would.
        let radius = CORE * (1.0 - self.tuck * 0.18);
        self.spin += self.vel.x / radius * dt * (1.0 - self.wing);

        if self.hold > 0.0 {
            self.hold -= dt;
        } else {
            let drawn = ((self.speed() - SMEAR_AT) / 700.0).clamp(0.0, 0.30);
            self.def_vel += ((drawn - self.def) * DEF_STIFF - self.def_vel * DEF_DAMP) * dt;
            self.def = (self.def + self.def_vel * dt).clamp(-0.6, 0.6);
            if self.def.abs() < 0.004 && self.def_vel.abs() < 0.04 {
                self.def = 0.0;
                self.def_vel = 0.0;
            }
        }
        if self.speed() > 30.0 && self.hold <= 0.0 {
            let want = self.vel.norm();
            self.def_dir = (self.def_dir + (want - self.def_dir) * (dt * 12.0).min(1.0)).norm();
        }

        // Bank into the turn. Leaning is what makes a change of direction an arc
        // rather than a reversal.
        let want_bank = (self.vel.x / 420.0).clamp(-0.6, 0.6) * (1.0 - self.wing * 0.6);
        self.bank += (want_bank - self.bank) * (1.0 - (-dt * 7.0).exp());
        if self.bank.abs() < 0.004 {
            self.bank = 0.0;
        }

        let want_tuck = if self.gait == Gait::Roll { 1.0 } else { 0.0 };
        self.tuck += (want_tuck - self.tuck) * (1.0 - (-dt * 4.5).exp());
        if self.tuck < 0.01 && want_tuck == 0.0 {
            self.tuck = 0.0;
        }

        let straining =
            overhead && self.grounded && self.gait != Gait::Roll && self.baffled < PATIENCE;
        self.strain +=
            ((if straining { STRETCH } else { 0.0 }) - self.strain) * (1.0 - (-dt * 5.0).exp());
        if self.strain < 0.6 && !straining {
            self.strain = 0.0;
        }

        if self.wing > 0.02 {
            self.flap += dt * (7.5 + self.wing * 5.0);
        }

        // The gaze leads the turn: the eye arrives before the body does, which is
        // most of what makes a change of direction read as intent.
        if self.gait == Gait::Roll {
            self.look_vel = self.look_vel * (-dt * 9.0).exp();
        } else if let Some(t) = drive.focus {
            let to = t - self.pos;
            let want = if to.len() > 4.0 { to.norm() } else { self.look };
            let pull = (want - self.look) * 260.0 - self.look_vel * 24.0;
            self.look_vel = self.look_vel + pull * dt;
            self.look = self.look + self.look_vel * dt;
            let l = self.look.len();
            if l > 1.0 {
                self.look = self.look * (1.0 / l);
            }
        } else {
            self.look_vel = self.look_vel * (-dt * 9.0).exp();
            if self.look_vel.len() < 1.4 {
                self.look_vel = Vec2::default();
            }
        }

        if drive.blink && self.clock >= self.blink_at {
            self.shut_at(0.19);
            let gap = if self.next() % 5 == 0 { 0.30 } else { self.range(2.6, 6.4) };
            self.blink_at = self.clock + gap;
        }
        if !drive.blink {
            self.blink_at = self.blink_at.max(self.clock + 1.0);
        }
    }

    fn step_gait(&mut self, dt: f32, target: Option<Vec2>, run: f32, gap: f32, overhead: bool) {
        match self.gait {
            Gait::Fly => {
                self.wing = (self.wing + 0.05).min(1.0);
                if !overhead {
                    self.enter(Gait::Rest);
                    return;
                }
                // Hover beside what it was reaching for rather than on top of it.
                if let Some(t) = target {
                    let perch = t + v2(-CORE * 1.15, CORE * 0.3);
                    let pull = (perch - self.pos) * 11.0 - self.vel * 4.2;
                    self.vel = self.vel + pull * dt;
                    // It rides its own wingbeat.
                    self.vel.y -= (self.flap.sin() * 125.0 + 26.0) * self.wing * dt;
                }
            }
            Gait::Leap => {
                if self.since > 0.14 {
                    self.wing = (self.wing + 0.08).min(1.0);
                }
                if self.since > 0.5 {
                    self.enter(Gait::Fly);
                }
            }
            Gait::Roll => {
                self.wing *= 0.86;
                // Something overhead beats covering ground: it uncurls, gathers
                // and takes off. Without this the roll was a trap with one exit.
                if overhead && self.baffled > PATIENCE && self.grounded {
                    self.crouch(0.5);
                    return;
                }
                if gap < ROLL_AT * 0.75 {
                    if self.vel.x.abs() > 260.0 {
                        self.def_dir = self.vel.norm();
                        // Piling up against its own momentum: compressed along
                        // travel, spread across it.
                        self.shock(-0.44, 0.2);
                        self.enter(Gait::Skid);
                    } else {
                        self.enter(Gait::Rest);
                    }
                    return;
                }
                // Continuous torque rather than shoves. A ball rolls; it does not
                // hop.
                if self.grounded {
                    self.vel.x += ROLL_TORQUE * run.signum() * dt;
                    self.vel.x = self.vel.x.clamp(-ROLL_TOP, ROLL_TOP);
                }
            }
            Gait::Wind => {
                let held = if self.def < -0.34 { WIND_TURN } else { WIND };
                if self.since < held {
                    return;
                }
                if self.baffled > PATIENCE && overhead {
                    // The take-off shove, with the wings still folded.
                    self.vel.y -= 620.0;
                    self.def_dir = v2(0.0, 1.0);
                    self.shock(0.52, 0.12);
                    self.enter(Gait::Leap);
                } else {
                    let urge = ((run.abs() - SLACK) / (CORE * 3.0)).clamp(0.3, 1.0);
                    self.vel.x += LURCH_RUN * urge * run.signum();
                    self.vel.y -= LURCH_UP * urge;
                    // Follow-through: it draws out as it leaves the ground.
                    self.def_dir = v2(run.signum() * 0.5, -1.0).norm();
                    self.shock(0.40 * urge, 0.11);
                    self.heading = run.signum();
                    self.lurch_at = self.clock + LURCH_GAP;
                    self.enter(Gait::Trudge);
                }
            }
            Gait::Skid => {
                self.wing *= 0.86;
                self.vel.x *= 1.0 - (7.0 * dt).min(0.5);
                if self.vel.x.abs() < 110.0 || self.since > 0.7 {
                    self.enter(Gait::Rest);
                }
            }
            Gait::Trudge | Gait::Rest => {
                self.wing *= 0.86;
                if self.wing < 0.02 {
                    self.wing = 0.0;
                }
                if gap > ROLL_AT && !overhead {
                    self.enter(Gait::Roll);
                    return;
                }
                if self.baffled > PATIENCE && overhead && self.grounded {
                    self.crouch(0.50);
                    return;
                }
                if run.abs() <= SLACK {
                    if self.grounded && self.speed() < STILL * 2.0 {
                        self.enter(Gait::Rest);
                    }
                    return;
                }
                if self.grounded && self.clock >= self.lurch_at {
                    let turning = run.signum() != self.heading;
                    self.crouch(if turning { 0.46 } else { 0.30 });
                }
            }
        }
    }

    /// Anticipation. Squash down and hold it until `step_gait` releases.
    ///
    /// The hold is bounded rather than infinite. It is meant to be released by
    /// the gait machine, but any path that leaves `Wind` without doing so would
    /// freeze the deformation for the rest of the session — which looks exactly
    /// like the creature being stuck in a puddle, and is impossible to diagnose
    /// from a still frame.
    fn crouch(&mut self, depth: f32) {
        self.def_dir = v2(0.0, 1.0);
        self.shock(-depth, WIND_TURN * 2.0);
        self.enter(Gait::Wind);
    }

    /// Is this point part of the creature?
    pub fn inside(&self, p: Vec2) -> bool {
        let f = &self.face;
        let core = CORE;

        // Mood in the silhouette: drawn up tall and narrow, or settled wide.
        let mut d = p - self.pos;
        d = v2(d.x / (1.0 - self.carriage * 0.10), d.y / (1.0 + self.carriage * 0.16));
        // Bank, as a shear.
        d.x -= d.y * self.bank * 0.45;

        // The one deformation that does squash, stretch and smear: along the
        // direction of travel it draws out, across it, it pinches.
        let dir = self.def_dir;
        let along = d.dot(dir) / (1.0 + self.def);
        let across = d.dot(dir.perp()) * (1.0 + self.def * 0.62);
        let q = dir * along + dir.perp() * across;
        let q = v2(q.x, q.y / (1.0 + self.strain / core));

        let (sn, cs) = self.spin.sin_cos();
        let r = v2(q.x * cs - q.y * sn, q.x * sn + q.y * cs);

        // A bean, not a ball — except tucked up, where it becomes one.
        let t = self.tuck;
        let mut body = smin(
            (r - v2(0.0, core * 0.18 * (1.0 - t))).len() - core * (0.92 + t * 0.06),
            (r - v2(core * 0.20 * (1.0 - t), -core * 0.34 * (1.0 - t))).len()
                - core * (0.60 + t * 0.32),
            core * 0.34,
        );
        let ang = r.y.atan2(r.x);
        body -= core
            * RIPPLE
            * (1.0 - t * 0.12)
            * ((ang * LOBES + self.grain).sin() + 0.45 * (ang * 5.0 - self.grain * 1.7).sin());

        for i in 0..2 {
            let phase = self.wobble * (0.55 + 0.29 * i as f32) + i as f32 * 2.3;
            let c = v2(phase.cos(), phase.sin() * 0.55) * (core * 0.44);
            body = smin(body, (r - c).len() - core * 0.5, WELD);
        }

        // Multiples: the shape it has just left, shrinking. Cheaper than a smear,
        // and it is what hand animation does anyway.
        let fast = ((self.speed() - SMEAR_AT) / 360.0).clamp(0.0, 1.0);
        if fast > 0.02 {
            for (i, g) in self.ghost.iter().enumerate() {
                let rad = core * (0.62 - i as f32 * 0.2) * fast;
                if rad > 2.0 {
                    body = smin(body, (p - *g).len() - rad, core * 0.7);
                }
            }
        }

        if self.strain > 1.0 {
            let tip = v2(0.0, -core * 0.55 - self.strain);
            body = smin(body, capsule(q, Vec2::default(), tip, core * 0.44, core * 0.24), WELD);
        }

        if self.wing > 0.05 {
            let beat = self.flap.sin();
            for side in [-1.0f32, 1.0] {
                // Rooted out at the surface on a thin neck and hard-unioned, so
                // there is a crease. Welded softly from inside the body it reads
                // as a bulge growing rather than as a wing.
                let root = v2(side * core * 0.84, -core * 0.04);
                let elbow =
                    root + v2(side * core * 0.60, -core * 0.40 + beat * core * 0.48) * self.wing;
                let tip =
                    elbow + v2(side * core * 0.64, -core * 0.08 + beat * core * 0.60) * self.wing;
                let neck = capsule(q, root, elbow, core * 0.13, core * 0.19);
                let blade = capsule(q, elbow, tip, core * 0.19, core * 0.045);
                body = body.min(smin(neck, blade, core * 0.18));
            }
        }

        if body > 0.0 {
            return false;
        }

        // A dimple, fixed in the body frame. Rolling only reads if something on
        // the surface goes round with it: a tucked-up ball is very nearly a
        // circle, and a rotating circle is indistinguishable from a still one.
        if self.tuck > 0.2 {
            let mark = v2(core * 0.30, -core * 0.66);
            if (r - mark).len() < core * 0.19 * self.tuck {
                return false;
            }
        }

        // Rolled up, the eye is shut and there is nothing else to draw.
        let lid = f.open * (1.0 - self.tuck * 0.97) * (1.0 - self.wink());
        if lid < 0.06 {
            return true;
        }

        let eye = self.pos + self.look * (core * 0.30) + v2(0.0, core * 0.10);
        let grow = lid.clamp(0.04, 1.3);
        let ry = EYE_R * grow;
        let rx = EYE_R * (1.0 + (grow - 1.0) * 0.5);
        let rel = v2((p.x - eye.x) / rx, (p.y - eye.y) / ry);
        if rel.len() - 1.0 > 0.0 {
            return true;
        }

        let slant = f.tilt * rel.x;
        let bow = f.arc * (1.0 - rel.x * rel.x);
        if rel.y < -1.0 + 2.0 * f.lid_top + slant + bow {
            return true;
        }
        if rel.y > 1.0 - 2.0 * f.lid_low + slant + bow {
            return true;
        }

        let glint = eye + self.look * (rx * 0.44);
        (p - glint).len() < GLINT_R * f.glint * grow.min(1.0)
    }

    /// Bounding box in visual units, so the sampler never walks the whole screen.
    pub fn bounds(&self) -> (Vec2, Vec2) {
        let wide = CORE * (1.9 + self.wing * 1.7);
        let tall = CORE * 1.9 + self.strain * 1.6;
        (
            v2(
                self.pos.x.min(self.ghost[1].x) - wide,
                self.pos.y.min(self.ghost[1].y) - tall,
            ),
            v2(
                self.pos.x.max(self.ghost[1].x) + wide,
                self.pos.y.max(self.ghost[1].y) + tall,
            ),
        )
    }
}
