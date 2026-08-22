//! What it is holding above its head.
//!
//! A terminal is a text device, so a creature that thinks in glyphs belongs in
//! one in a way a sprite never quite does. This is the channel that carries
//! anything too specific for a body to say — a `?`, a spinner, or the host's own
//! words verbatim.
//!
//! It fades rather than switching, because everything else on this screen fades,
//! and because a thought that pops reads as a notification rather than as
//! something occurring to somebody.

const RISE: f32 = 0.22;
const FALL: f32 = 0.34;

#[derive(Default)]
pub struct Thought {
    held: Option<String>,
    /// Seconds the current thought has been up.
    age: f32,
    /// How long to hold before fading out. `None` holds until replaced.
    keep: Option<f32>,
    /// Ramps down after the hold expires, so the fade survives the text going.
    ghost: f32,
}

impl Thought {
    /// Replaces whatever is up. A new thought restarts the fade, which is what
    /// makes a rapid series read as one continuous mutter rather than a flicker.
    pub fn show(&mut self, text: impl Into<String>, keep: Option<f32>) {
        let text = text.into();
        if self.held.as_deref() == Some(text.as_str()) {
            // Same words again: extend the hold instead of restarting the fade.
            self.keep = keep;
            self.age = self.age.min(RISE);
            return;
        }
        self.held = Some(text);
        self.keep = keep;
        self.age = 0.0;
        self.ghost = self.ghost.max(0.0);
    }

    pub fn clear(&mut self) {
        if self.held.take().is_some() {
            self.ghost = self.alpha_at(self.age);
            self.age = 0.0;
        }
    }

    pub fn tick(&mut self, dt: f32) {
        self.age += dt;
        if self.held.is_some() {
            if let Some(keep) = self.keep {
                if self.age > keep {
                    self.clear();
                }
            }
        } else if self.ghost > 0.0 {
            self.ghost = (self.ghost - dt / FALL).max(0.0);
        }
    }

    fn alpha_at(&self, age: f32) -> f32 {
        (age / RISE).clamp(0.0, 1.0)
    }

    /// 0 to 1. Zero means there is nothing to draw at all.
    pub fn alpha(&self) -> f32 {
        match &self.held {
            Some(_) => self.alpha_at(self.age),
            None => self.ghost,
        }
    }

    pub fn words(&self) -> Option<&str> {
        self.held.as_deref()
    }

    /// Only the fades count as motion. Text sitting there at full strength needs
    /// no frames at all, and claiming otherwise would hold the host awake for
    /// the whole time a line is up.
    pub fn moving(&self) -> bool {
        match &self.held {
            Some(_) => self.age < RISE,
            None => self.ghost > 0.0,
        }
    }
}
