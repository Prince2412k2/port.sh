//! The skills sheet: an endless lattice of tool marks that lifts under the
//! cursor like paper over a magnet.
//!
//! The whole effect is one scalar field. Every tile sits at a fixed place on
//! the lattice and is displaced *upward on screen* by `lift()`, a gaussian bump
//! centred on the pointer. Neighbours are further down the same curve, so they
//! rise less, and the lattice bulges rather than one tile popping.
//!
//! Nothing draws the surface itself. An earlier version threaded a mesh between
//! the anchors so there was something visibly bending, which worked but put a
//! net over every mark — and the marks are the content. Taking it away costs
//! the bend some of its legibility and buys back the whole frame, so the tiles
//! grew to carry it instead: at this size the displacement is plain enough in
//! the break it makes in the lattice's own rows.
//!
//! Two things still have to be true or it reads as tiles moving rather than as
//! a surface:
//!
//! * **Order is by lattice row, not by screen row.** A lifted tile is still at
//!   the same depth; it just rises. Sorting by screen position would push it
//!   *behind* the row in front of it, which is exactly backwards.
//! * **Brightness follows height.** Displacement alone is ambiguous — a tile
//!   two rows up could just be a tile two rows up. Lighting the raised ones
//!   resolves it.
//!
//! There is no camera to fly. The lattice is infinite, so panning is a drift
//! applied to the whole field, and nothing is ever "off screen somewhere" the
//! way it is on a map — every cell of the frame always has sheet in it.

use crate::logos::{Logo, LOGOS};

/// Cells between anchors. Wider than a mark so the sheet reads as spaced tiles
/// rather than a mosaic, and taller than one so a lifted tile has somewhere to
/// go before it collides with the row behind.
pub const PITCH_X: f64 = 24.0;
pub const PITCH_Y: f64 = 12.0;

/// How far a tile directly under the pointer rises, in rows.
const PEAK: f64 = 6.0;
/// Width of the bump, in visual units (see `visual_dist`). Wide enough to take
/// in a tile's neighbours: a bump narrower than the pitch lifts one mark on its
/// own, which is a tooltip, not a sheet.
const SIGMA: f64 = 38.0;

/// Vertical squash of the lattice. The plane is tilted away from the viewer, so
/// a step "back" covers less screen than a step sideways.
const SQUASH: f64 = 0.82;

/// A terminal cell is about twice as tall as it is wide, so vertical distances
/// have to be doubled before they can be compared with horizontal ones.
#[inline]
fn visual_dist(dx: f64, dy: f64) -> f64 {
    (dx * dx + (dy * 2.0) * (dy * 2.0)).sqrt()
}

pub struct Sheet {
    /// Camera drift, in cells. The lattice is infinite; this just slides it.
    pub drift: (f64, f64),
    /// Pointer, in cells relative to the sheet's area. None = no magnet.
    pub cursor: Option<(f64, f64)>,
    /// Frame area, in cells.
    pub w: f64,
    pub h: f64,
}

impl Sheet {
    /// Screen position of lattice cell `(i, j)`, before any lift.
    #[inline]
    fn anchor(&self, i: i64, j: i64) -> (f64, f64) {
        // Odd rows are offset half a pitch, so the lattice reads as a weave
        // rather than as columns, and no two marks ever line up vertically.
        let stagger = if j.rem_euclid(2) == 0 { 0.0 } else { PITCH_X * 0.5 };
        (
            i as f64 * PITCH_X + stagger - self.drift.0,
            j as f64 * PITCH_Y * SQUASH - self.drift.1,
        )
    }

    /// How far the sheet rises at a screen position, in rows.
    #[inline]
    pub fn lift(&self, x: f64, y: f64) -> f64 {
        let Some((cx, cy)) = self.cursor else { return 0.0 };
        let d = visual_dist(x - cx, y - cy);
        let t = d / SIGMA;
        PEAK * (-t * t).exp()
    }

    /// Lattice cells whose anchors could touch the frame, with a margin so a
    /// tile rising in from below is already being drawn when it appears.
    fn visible(&self) -> (i64, i64, i64, i64) {
        let mx = PITCH_X;
        let my = PITCH_Y * SQUASH + PEAK;
        let i0 = ((self.drift.0 - mx) / PITCH_X).floor() as i64;
        let i1 = ((self.drift.0 + self.w + mx) / PITCH_X).ceil() as i64;
        let j0 = ((self.drift.1 - my) / (PITCH_Y * SQUASH)).floor() as i64;
        let j1 = ((self.drift.1 + self.h + my) / (PITCH_Y * SQUASH)).ceil() as i64;
        (i0, i1, j0, j1)
    }

    /// Every tile that should be drawn, already in paint order.
    pub fn tiles(&self) -> Vec<Tile> {
        let (i0, i1, j0, j1) = self.visible();
        let mut out = Vec::new();
        for j in j0..=j1 {
            for i in i0..=i1 {
                let (ax, ay) = self.anchor(i, j);
                let h = self.lift(ax, ay);
                out.push(Tile {
                    logo: pick(i, j),
                    x: ax,
                    y: ay - h,
                    lift: h / PEAK,
                });
            }
        }
        // Lattice order already runs back to front: `j` ascending is depth
        // ascending, and within a row the overlap is negligible.
        out
    }

}

pub struct Tile {
    pub logo: &'static Logo,
    /// Top-left of the mark, in cells relative to the sheet area.
    pub x: f64,
    pub y: f64,
    /// 0 flat, 1 fully raised.
    pub lift: f64,
}

/// Which mark sits at a lattice cell.
///
/// The lattice is infinite and the toolkit is not, so it repeats — but hashed
/// rather than tiled, so the repeat never lines up into visible bands. Same
/// hash every run, so the sheet is the same sheet on every machine.
fn pick(i: i64, j: i64) -> &'static Logo {
    let mut h = (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (j as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
    h ^= h >> 29;
    h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h ^= h >> 32;
    &LOGOS[(h % LOGOS.len() as u64) as usize]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sheet() -> Sheet {
        Sheet { drift: (0.0, 0.0), cursor: Some((60.0, 18.0)), w: 120.0, h: 36.0 }
    }

    #[test]
    fn the_bump_peaks_under_the_pointer_and_decays() {
        let s = sheet();
        let at = s.lift(60.0, 18.0);
        assert!((at - PEAK).abs() < 1e-9, "peak was {at}");
        // Neighbours rise, but less — that is the whole effect.
        let near = s.lift(60.0 + PITCH_X, 18.0);
        let far = s.lift(60.0 + PITCH_X * 3.0, 18.0);
        assert!(near < at && near > 0.0, "near {near} vs {at}");
        assert!(far < near, "far {far} vs near {near}");
    }

    #[test]
    fn the_bump_is_round_on_screen_not_in_cells() {
        // A cell is twice as tall as it is wide, so equal *visual* offsets must
        // give equal lift even though the cell counts differ by two.
        let s = sheet();
        let across = s.lift(60.0 + 12.0, 18.0);
        let down = s.lift(60.0, 18.0 + 6.0);
        assert!((across - down).abs() < 1e-9, "{across} vs {down}");
    }

    #[test]
    fn no_pointer_means_a_flat_sheet() {
        let s = Sheet { cursor: None, ..sheet() };
        for (x, y) in [(0.0, 0.0), (60.0, 18.0), (119.0, 35.0)] {
            assert_eq!(s.lift(x, y), 0.0);
        }
    }

    #[test]
    fn the_sheet_covers_the_whole_frame() {
        // An infinite lattice has no edges, so every corner must be inside some
        // tile's reach no matter where the drift has got to.
        for drift in [(0.0, 0.0), (7.5, 3.25), (-40.0, 91.0), (1e4, -1e4)] {
            let s = Sheet { drift, ..sheet() };
            let ts = s.tiles();
            for (cx, cy) in [(0.0, 0.0), (s.w, 0.0), (0.0, s.h), (s.w, s.h)] {
                let covered = ts.iter().any(|t| {
                    cx >= t.x - PITCH_X && cx <= t.x + PITCH_X
                        && cy >= t.y - PITCH_Y * 2.0 && cy <= t.y + PITCH_Y * 2.0
                });
                assert!(covered, "corner {cx},{cy} uncovered at drift {drift:?}");
            }
        }
    }

    #[test]
    fn the_lattice_is_the_same_lattice_every_run() {
        let a: Vec<&str> = (0..40).map(|k| pick(k, k * 3).id).collect();
        let b: Vec<&str> = (0..40).map(|k| pick(k, k * 3).id).collect();
        assert_eq!(a, b);
        // and it does not collapse onto one mark
        let distinct: std::collections::HashSet<_> = a.iter().collect();
        assert!(distinct.len() > 8, "only {} marks in 40 cells", distinct.len());
    }

    #[test]
    fn drifting_a_whole_pitch_returns_the_same_arrangement() {
        let a = Sheet { drift: (0.0, 0.0), ..sheet() };
        let b = Sheet { drift: (PITCH_X, 0.0), ..sheet() };
        // Same anchors modulo the shift: the lattice is regular, so a whole
        // pitch of drift must not jitter the phase.
        let ax: Vec<f64> = a.tiles().iter().map(|t| t.x + PITCH_X).collect();
        let bx: Vec<f64> = b.tiles().iter().map(|t| t.x).collect();
        assert!(bx.iter().any(|x| ax.iter().any(|a| (a - x).abs() < 1e-6)));
    }
}
