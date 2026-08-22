//! A one-eyed creature that lives in a terminal frame.
//!
//! Built as a library first, because the interesting constraints all come from
//! the host rather than from the creature:
//!
//! * **It has to be able to say it is finished.** A host that decides whether to
//!   request a frame at all — as a portfolio served over SSH must — cannot afford
//!   a mascot that always claims to be moving. [`Mascot::moving`] going false is
//!   the whole reason the repose machine below exists.
//! * **It has to be a pure function of its inputs.** No wall clock, no thread
//!   randomness. Given the same seed and the same sequence of `tick` calls it
//!   draws the same pixels, so a snapshot pinned to an exact moment stays
//!   byte-identical.
//! * **It composites over a finished frame.** No background is painted and empty
//!   cells are left alone, so it can sit over whatever the host has already drawn
//!   without needing a panel of its own.
//!
//! The host drives it with three calls a frame — [`Mascot::pointer`],
//! [`Mascot::tick`], [`Mascot::render`] — plus [`Mascot::cue`] for things it
//! could not work out for itself.

mod blob;
mod glyph;
mod layer;
mod think;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;

use blob::{v2, Blob, Drive, Face, Vec2, CORE, PX, PY};
use glyph::Atlas;
use layer::{Layer, SUB_X, SUB_Y};
use think::Thought;

/// Seconds of stillness before it settles into holding one shape.
const SETTLE: f32 = 0.7;
/// How long a nudged gaze holds before attention returns to the pointer.
const GLANCE: f32 = 2.2;
/// Gap between ambient lines. Long, because a mascot that keeps talking is a
/// mascot people close the tab on.
const CHATTER: f32 = 26.0;

/// Colours and habits. Everything here is the host's taste, not the creature's.
pub struct Skin {
    pub body: Color,
    pub thought: Color,
    /// What a fading thought fades toward — the host's own background.
    pub ground: Color,
    /// Whether the body chases the pointer or only watches it. Watching without
    /// moving is much cheaper and, on a quiet screen, often reads better.
    pub follow: bool,
    /// Seconds of stillness before it falls asleep. Sleeping is not only a
    /// charming idle: it is the state in which nothing moves at all, so it is
    /// also how the host gets its slow poll rate back.
    pub sleep_after: f32,
}

impl Default for Skin {
    fn default() -> Self {
        Skin {
            body: Color::Rgb(238, 236, 228),
            thought: Color::Rgb(140, 146, 158),
            ground: Color::Rgb(8, 9, 11),
            follow: true,
            sleep_after: 40.0,
        }
    }
}

/// How it feels.
///
/// Four axes rather than a list of expressions, so any two states blend and
/// there is never a frame to switch to. The `brow` axis does most of the acting:
/// driven down it reads cross or unimpressed, driven up it reads worried or
/// eager, and it is the difference between a creature and a shape with a dot on
/// it.
#[derive(Clone, Copy)]
pub struct Feeling {
    /// -1 low, +1 bright.
    pub valence: f32,
    /// 0 calm, 1 wired.
    pub arousal: f32,
    /// 0 open, 1 narrowed — effort, scrutiny, suspicion.
    pub focus: f32,
    /// -1 lids bowed into a scowl, +1 bowed the other way into worry or a
    /// squeezed-shut smile. The eye's shape is the only place emotion is
    /// allowed to live, so this axis does most of the work.
    pub brow: f32,
}

impl Feeling {
    pub const fn new(valence: f32, arousal: f32, focus: f32, brow: f32) -> Feeling {
        Feeling { valence, arousal, focus, brow }
    }

    /// The default, and deliberately almost nothing. Xiaohei's expression is
    /// specified as blank, dull, calm and serious — dry, not cute — so the
    /// baseline is a plain open eye and every other state is a small departure
    /// from it. Presets pitched for a cartoon make it read as a children's toy.
    pub const BLANK: Feeling = Feeling::new(0.0, 0.1, 0.0, 0.0);
    pub const CONTENT: Feeling = Feeling::new(0.2, 0.12, 0.05, -0.1);
    pub const CURIOUS: Feeling = Feeling::new(0.1, 0.45, 0.0, 0.3);
    pub const PLEASED: Feeling = Feeling::new(0.6, 0.35, 0.3, -0.35);
    pub const WORRIED: Feeling = Feeling::new(-0.3, 0.55, 0.1, 0.6);
    pub const CROSS: Feeling = Feeling::new(-0.15, 0.35, 0.45, -0.7);
    pub const WEARY: Feeling = Feeling::new(-0.4, 0.05, 0.6, -0.2);
    /// Peering at something closely.
    pub const STUDIOUS: Feeling = Feeling::new(0.05, 0.2, 0.55, -0.25);
    /// Too much on at once.
    pub const SWAMPED: Feeling = Feeling::new(-0.25, 0.8, 0.4, 0.5);
    pub const ALARMED: Feeling = Feeling::new(-0.45, 1.0, 0.0, 0.75);

    /// Blend toward another feeling. Used for arriving at a mood rather than
    /// cutting to it, and for mixing a role's temperament with a reaction.
    pub fn toward(self, other: Feeling, t: f32) -> Feeling {
        let m = |a: f32, b: f32| a + (b - a) * t;
        Feeling::new(
            m(self.valence, other.valence),
            m(self.arousal, other.arousal),
            m(self.focus, other.focus),
            m(self.brow, other.brow),
        )
    }

    /// Mood in the silhouette. Drawn up when alert, settled low when flat — the
    /// body carries half the expression and the eye cannot do it alone.
    fn carriage(&self) -> f32 {
        (self.arousal * 0.5 + self.valence * 0.35 - self.focus * 0.2).clamp(-0.8, 0.9)
    }

    fn face(&self) -> Face {
        let low = (-self.valence).max(0.0);
        let high = self.valence.max(0.0);
        Face {
            open: (1.0 - self.focus * 0.55 + self.arousal * 0.30 - low * 0.1).clamp(0.12, 1.3),
            lid_top: (self.focus * 0.22 + low * 0.14).clamp(0.0, 0.55),
            lid_low: (high * 0.32).clamp(0.0, 0.5),
            tilt: self.brow * 0.35,
            arc: (-self.brow * 0.45 + high * 0.2).clamp(-0.55, 0.55),
            glint: (1.0 - self.arousal * 0.5).clamp(0.3, 1.1),
        }
    }
}

/// Things the host knows and the creature cannot see for itself.
pub enum Cue {
    /// Something went wrong. It flinches.
    Alarm,
    /// Something landed well.
    Delight,
    /// Work started or finished.
    Busy(bool),
    /// Hold the host's own words above its head. This is the channel worth
    /// spending: a creature repeating what the app is actually doing is doing
    /// something no sprite can.
    Says(String),
    /// Attention should go here, in host cells, whatever the pointer is doing.
    Look(u16, u16),
    /// Wake up.
    Rouse,
    /// Feel this way for a while, then drift back to the role's temperament.
    Feel(Feeling),
}

/// How much of itself is still running.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Repose {
    /// Moving or reacting. Frames wanted.
    Awake,
    /// Holding one exact shape. Frames only for the odd blink.
    Settled,
    /// Eye shut, nothing running, no frames at all.
    Asleep,
}

pub struct Mascot {
    skin: Skin,
    area: Rect,
    layer: Layer,
    atlas: Atlas,
    blob: Blob,
    thought: Thought,
    repose: Repose,
    /// Seconds since anything happened.
    quiet: f32,
    /// Kept in host cells rather than converted on arrival, so the host is free
    /// to report a pointer before it has told us the area.
    pointer: Option<(u16, u16)>,
    glance: Option<(u16, u16)>,
    glance_for: f32,
    /// The mood it returns to when nothing is happening.
    resting: Feeling,
    /// Where the face is headed. Eased toward `resting` unless a cue has pushed
    /// it somewhere.
    feeling: Feeling,
    /// Seconds left of a cue-driven mood before the role reclaims the face.
    mood_for: f32,
    /// Ambient lines for the current role, and when the next one is due.
    lines: Vec<String>,
    said: usize,
    next_line: f32,
    clock: f32,
}

impl Mascot {
    pub fn new(seed: u64) -> Mascot {
        Mascot::with_skin(seed, Skin::default())
    }

    pub fn with_skin(seed: u64, skin: Skin) -> Mascot {
        Mascot {
            skin,
            area: Rect::new(0, 0, 0, 0),
            layer: Layer::new(0, 0),
            atlas: Atlas::new(),
            blob: Blob::new(Vec2::default(), seed),
            thought: Thought::default(),
            repose: Repose::Awake,
            quiet: 0.0,
            pointer: None,
            glance: None,
            glance_for: 0.0,
            resting: Feeling::BLANK,
            feeling: Feeling::BLANK,
            mood_for: 0.0,
            lines: Vec::new(),
            said: 0,
            next_line: 14.0,
            clock: 0.0,
        }
    }

    pub fn skin_mut(&mut self) -> &mut Skin {
        &mut self.skin
    }

    /// The mood it drifts back to. A section change in the host is a change of
    /// this, which is how the same creature reads differently per screen without
    /// needing a costume.
    pub fn settle_into(&mut self, resting: Feeling) {
        self.resting = resting;
        self.mood_for = 0.0;
        self.rouse();
    }


    pub fn flying(&self) -> bool {
        self.blob.flying()
    }

    pub fn rolling(&self) -> bool {
        self.blob.rolling()
    }

    /// Where it is, in host cells. Useful for keeping other things out from
    /// under it, and for telling whether it is getting anywhere.
    pub fn at(&self) -> (u16, u16) {
        let p = self.blob.at();
        (
            self.area.x + (p.x / (SUB_X as f32 * PX)) as u16,
            self.area.y + (p.y / (SUB_Y as f32 * PY)) as u16,
        )
    }

    /// Ambient dialogue for the current part. Delivered in order rather than at
    /// random, so a visitor who waits hears all of it, and only while it is
    /// otherwise still — a line interrupting its own reaction reads as noise.
    pub fn lines<I, S>(&mut self, lines: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.lines = lines.into_iter().map(Into::into).collect();
        self.said = 0;
        self.next_line = self.clock + 5.0;
    }

    /// The region it may occupy. Cheap to call every frame; it only does work
    /// when the shape actually changed.
    pub fn confine(&mut self, area: Rect) {
        if self.area == area {
            return;
        }
        let fresh = self.area.width == 0;
        self.area = area;
        self.layer = Layer::new(area.width as i32 * SUB_X, area.height as i32 * SUB_Y);
        let middle = v2(
            area.width as f32 * SUB_X as f32 * PX * 0.5,
            area.height as f32 * SUB_Y as f32 * PY * 0.5,
        );
        if fresh {
            self.blob.pin(middle);
        } else {
            // A resize moves the ground out from under it. Nudging rather than
            // teleporting keeps the reaction visible instead of instantaneous.
            self.rouse();
        }
        self.corral();
    }

    /// Where the pointer is, in host cells. `None` when it is unknown or gone.
    pub fn pointer(&mut self, at: Option<(u16, u16)>) {
        if self.pointer != at {
            self.pointer = at;
            self.rouse();
        }
    }

    pub fn cue(&mut self, cue: Cue) {
        match cue {
            Cue::Alarm => {
                self.mood(Feeling::ALARMED, 2.6);
                // Straight up. A flinch is vertical — being shoved sideways
                // reads as having been hit by something instead.
                self.blob.kick(v2(0.0, -26.0));
                self.thought.show("!", Some(1.6));
                self.rouse();
            }
            Cue::Delight => {
                self.mood(Feeling::PLEASED, 2.2);
                self.blob.kick(v2(0.0, -34.0));
                self.thought.show("\u{2713}", Some(1.4));
                self.rouse();
            }
            Cue::Busy(on) => {
                if on {
                    self.mood(Feeling::SWAMPED, f32::MAX);
                } else {
                    self.mood(self.resting, 0.0);
                }
                if on {
                    self.thought.show("\u{2026}", None);
                } else {
                    self.thought.clear();
                }
                self.rouse();
            }
            Cue::Says(words) => {
                self.thought.show(words, Some(4.5));
                self.rouse();
            }
            Cue::Look(col, row) => {
                self.glance = Some((col, row));
                self.glance_for = GLANCE;
                self.rouse();
            }
            Cue::Rouse => self.rouse(),
            Cue::Feel(how) => self.mood(how, 3.0),
        }
    }

    /// Push the face somewhere and hold it there for `secs`, after which the
    /// role's own temperament reclaims it.
    fn mood(&mut self, how: Feeling, secs: f32) {
        self.feeling = how;
        self.mood_for = secs;
        self.rouse();
    }

    fn rouse(&mut self) {
        if self.repose == Repose::Asleep {
            // Waking is a beat, not a switch: a small stretch upward so there is
            // something to see between shut and watching.
            self.blob.kick(v2(0.0, -12.0));
        }
        self.repose = Repose::Awake;
        self.quiet = 0.0;
    }

    /// Keep the body inside its region. Only the centre is clamped — letting the
    /// edges overhang is what stops it looking parked in a box.
    fn corral(&mut self) {
        if self.area.width == 0 {
            return;
        }
        let w = self.area.width as f32 * SUB_X as f32 * PX;
        let h = self.area.height as f32 * SUB_Y as f32 * PY;
        let margin = CORE * 0.55;
        let at = self.blob.at();
        let held = v2(
            at.x.clamp(margin.min(w * 0.5), (w - margin).max(w * 0.5)),
            at.y.clamp(margin.min(h * 0.5), (h - margin).max(h * 0.5)),
        );
        if (held - at).len() > 0.01 {
            self.blob.pin(held);
        }
    }

    pub fn tick(&mut self, dt: f32) {
        if self.area.width == 0 {
            return;
        }
        self.clock += dt;
        self.thought.tick(dt);
        if self.glance_for > 0.0 {
            self.glance_for -= dt;
            if self.glance_for <= 0.0 {
                self.glance = None;
            }
        }
        if self.mood_for > 0.0 {
            self.mood_for -= dt;
        } else {
            // Drift back to the part it is playing, rather than holding a
            // reaction for ever.
            self.feeling = self.feeling.toward(self.resting, (dt * 1.5).min(1.0));
        }

        let focus = self.glance.or(self.pointer).map(|(c, r)| self.local(c, r));
        let floor = self.area.height as f32 * SUB_Y as f32 * PY - CORE * 0.86;
        let mut face = self.feeling.face();
        let carriage = self.feeling.carriage();
        let drive = match self.repose {
            Repose::Awake => Drive {
                target: if self.skin.follow { focus } else { None },
                focus,
                face,
                wobble: true,
                blink: true,
                floor,
                carriage,
            },
            Repose::Settled => Drive {
                target: None,
                focus,
                face,
                wobble: false,
                blink: true,
                floor,
                carriage,
            },
            Repose::Asleep => {
                face.open = 0.05;
                face.arc = 0.0;
                Drive {
                    target: None,
                    focus: None,
                    face,
                    wobble: false,
                    blink: false,
                    floor,
                    carriage,
                }
            }
        };
        self.blob.tick(dt, &drive);
        self.corral();

        // The repose machine. Everything above only describes one frame; this is
        // what lets a quiet session cost nothing.
        let stirring = self.blob.restless() || self.thought.moving() || self.glance_for > 0.0;
        if stirring {
            self.quiet = 0.0;
        } else {
            self.quiet += dt;
        }
        if !self.lines.is_empty()
            && self.repose == Repose::Settled
            && self.clock >= self.next_line
        {
            let line = self.lines[self.said % self.lines.len()].clone();
            self.said += 1;
            self.thought.show(line, Some(5.5));
            self.next_line = self.clock + CHATTER;
        }

        self.repose = match self.repose {
            Repose::Awake if self.quiet > SETTLE => Repose::Settled,
            Repose::Settled if self.quiet > self.skin.sleep_after => {
                self.thought.show("z", None);
                Repose::Asleep
            }
            other => other,
        };
    }

    /// Does it want another frame? **Fold this into the host's own decision about
    /// whether to render**, or it will hold the app at its fastest frame rate for
    /// ever — which over a remote session is somebody else's bandwidth.
    pub fn moving(&self) -> bool {
        match self.repose {
            Repose::Awake => true,
            Repose::Settled => self.blob.blinking() || self.thought.moving(),
            Repose::Asleep => false,
        }
    }

    pub fn asleep(&self) -> bool {
        self.repose == Repose::Asleep
    }

    /// Composite over whatever the host has already drawn. Empty cells are left
    /// alone, so nothing behind it is erased.
    pub fn render(&mut self, buf: &mut Buffer, area: Rect) {
        self.confine(area);
        if self.area.width == 0 || self.area.height == 0 {
            return;
        }
        self.paint();
        self.layer.resolve(
            buf,
            area,
            (area.x as i32, area.y as i32),
            &self.atlas,
            self.skin.body,
        );
        self.speak(buf, area);
    }

    /// Samples the field. Two by two supersamples per subpixel and a majority
    /// vote: nearly free, and it places the edge far more stably than a single
    /// centre sample — an outline that flips whole subpixels back and forth reads
    /// as static rather than as movement.
    fn paint(&mut self) {
        self.layer.clear();
        let (lo, hi) = self.blob.bounds();
        let (w, h) = self.layer.size();
        let i0 = (lo.x / PX).floor().max(0.0) as i32;
        let i1 = ((hi.x / PX).ceil() as i32).min(w - 1);
        let j0 = (lo.y / PY).floor().max(0.0) as i32;
        let j1 = ((hi.y / PY).ceil() as i32).min(h - 1);

        for j in j0..=j1 {
            for i in i0..=i1 {
                let mut hits = 0;
                for (ox, oy) in [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)] {
                    if self.blob.inside(v2((i as f32 + ox) * PX, (j as f32 + oy) * PY)) {
                        hits += 1;
                    }
                }
                if hits >= 2 {
                    self.layer.plot(i, j);
                }
            }
        }
    }

    fn speak(&self, buf: &mut Buffer, area: Rect) {
        let alpha = self.thought.alpha();
        let Some(words) = self.thought.words() else {
            return;
        };
        if alpha <= 0.02 || words.is_empty() {
            return;
        }
        let crown = self.blob.crown();
        let width = words.chars().count() as i32;
        let col = (crown.x / (SUB_X as f32 * PX)).round() as i32 - width / 2;
        let row = (crown.y / (SUB_Y as f32 * PY)).round() as i32 - 1;
        if row < 0 {
            return;
        }
        let colour = mix(self.skin.ground, self.skin.thought, alpha);
        for (n, ch) in words.chars().enumerate() {
            let x = area.x as i32 + col + n as i32;
            let y = area.y as i32 + row;
            if x < area.x as i32 || x >= area.right() as i32 || y >= area.bottom() as i32 {
                continue;
            }
            if let Some(cell) = buf.cell_mut((x as u16, y as u16)) {
                cell.set_char(ch).set_fg(colour);
            }
        }
    }

    fn local(&self, col: u16, row: u16) -> Vec2 {
        let dc = col as f32 - self.area.x as f32;
        let dr = row as f32 - self.area.y as f32;
        v2(
            (dc + 0.5) * SUB_X as f32 * PX,
            (dr + 0.5) * SUB_Y as f32 * PY,
        )
    }
}

/// Ramps `to` in over `from`. Falls back to a hard switch for palette colours,
/// which have no components to interpolate.
fn mix(from: Color, to: Color, t: f32) -> Color {
    match (from, to) {
        (Color::Rgb(ar, ag, ab), Color::Rgb(br, bg, bb)) => {
            let lerp = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t.clamp(0.0, 1.0)) as u8;
            Color::Rgb(lerp(ar, br), lerp(ag, bg), lerp(ab, bb))
        }
        _ if t > 0.5 => to,
        (from, _) => from,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames(mascot: &mut Mascot, n: usize) -> Buffer {
        let area = Rect::new(0, 0, 24, 12);
        let mut buf = Buffer::empty(area);
        for _ in 0..n {
            mascot.tick(1.0 / 60.0);
        }
        mascot.render(&mut buf, area);
        buf
    }

    /// The host snapshots a frame at an exact moment and expects the same bytes
    /// every time. Anything that reaches for a wall clock or thread randomness
    /// breaks this and nothing else will notice.
    #[test]
    fn same_seed_draws_the_same_frame() {
        let mut a = Mascot::new(7);
        let mut b = Mascot::new(7);
        a.confine(Rect::new(0, 0, 24, 12));
        b.confine(Rect::new(0, 0, 24, 12));
        a.pointer(Some((3, 3)));
        b.pointer(Some((3, 3)));
        assert_eq!(frames(&mut a, 120), frames(&mut b, 120));
    }

    /// The load-bearing contract. A mascot that never stops moving holds the
    /// host at its fastest frame rate for ever, which over a served session is
    /// somebody else's bandwidth.
    #[test]
    fn comes_to_rest_and_then_sleeps() {
        let mut mascot = Mascot::new(7);
        mascot.skin_mut().sleep_after = 2.0;
        mascot.confine(Rect::new(0, 0, 24, 12));
        mascot.pointer(Some((12, 6)));

        // Long enough to settle, sleep, and stop asking for frames. Stepping
        // rather than jumping, because the repose machine is driven by dt.
        for _ in 0..600 {
            mascot.tick(1.0 / 60.0);
        }
        assert!(mascot.asleep(), "should have fallen asleep");
        assert!(!mascot.moving(), "asleep must want no frames");
    }

    #[test]
    fn a_cue_wakes_it_again() {
        let mut mascot = Mascot::new(7);
        mascot.skin_mut().sleep_after = 1.0;
        mascot.confine(Rect::new(0, 0, 24, 12));
        for _ in 0..300 {
            mascot.tick(1.0 / 60.0);
        }
        assert!(mascot.asleep());
        mascot.cue(Cue::Alarm);
        assert!(!mascot.asleep());
        assert!(mascot.moving());
    }
}
