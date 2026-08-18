//! Verbatim from termap's `labels.rs`, so that when the two apps are combined
//! the files dedupe to one rather than needing a diff reconciled. A couple of
//! its fields go unused in a sky, which is cheaper than a divergence.
#![allow(dead_code)]

//! Collision-avoiding label placement.
//!
//! Greedy and rank-ordered: important names get first pick of the space, and
//! anything that cannot find a clear slot near its anchor is either pushed out
//! on a leader line or dropped. Dropping is a feature -- a map that refuses to
//! draw an overlapping label is more readable than one that draws everything.

pub struct Candidate {
    /// Anchor in subpixel coords.
    pub anchor: [f64; 2],
    pub text: String,
    pub rank: u16,
    pub tint: u8,
    pub depth: f32,
    pub marker: Option<char>,
    pub feature: u32,
}

pub struct Placed {
    /// Top-left cell of the text run.
    pub cell: (usize, usize),
    pub text: String,
    pub tint: u8,
    pub depth: f32,
    pub bold: bool,
    pub marker: Option<(usize, usize, char)>,
    /// Subpixel endpoints of the leader, when the label had to move away.
    pub leader: Option<([f64; 2], [f64; 2])>,
    pub feature: u32,
}

pub struct Occupancy {
    w: usize,
    h: usize,
    used: Vec<bool>,
}

impl Occupancy {
    pub fn new(w: usize, h: usize) -> Self {
        Occupancy { w, h, used: vec![false; w * h] }
    }

    fn free(&self, x: isize, y: isize, len: usize) -> bool {
        if y < 0 || y >= self.h as isize || x < 0 {
            return false;
        }
        if x as usize + len > self.w {
            return false;
        }
        (0..len).all(|i| !self.used[y as usize * self.w + x as usize + i])
    }

    /// Is a run of cells still clear? Needed by callers that place text
    /// themselves rather than asking for a slot.
    pub fn free_run(&self, x: usize, y: usize, len: usize) -> bool {
        self.free(x as isize, y as isize, len)
    }

    /// Mark a rectangle as taken before placement starts.
    pub fn block(&mut self, x: usize, y: usize, w: usize, h: usize) {
        for yy in y..(y + h).min(self.h) {
            for xx in x..(x + w).min(self.w) {
                self.used[yy * self.w + xx] = true;
            }
        }
    }

    fn take(&mut self, x: usize, y: usize, len: usize) {
        // One cell of padding on each side and one row above/below, so labels
        // never sit shoulder to shoulder.
        let x0 = x.saturating_sub(2);
        let x1 = (x + len + 2).min(self.w);
        let y0 = y.saturating_sub(1);
        let y1 = (y + 2).min(self.h);
        for yy in y0..y1 {
            for xx in x0..x1 {
                self.used[yy * self.w + xx] = true;
            }
        }
    }
}

/// Offsets tried in order, as (dx from anchor cell, dy, align).
/// `align` is how much of the text width to shift left: 0 = start at dx,
/// 1 = end at dx, 2 = centre on dx.
const SLOTS: [(isize, isize, u8); 8] = [
    (2, 0, 0),   // east
    (-2, 0, 1),  // west
    (2, -1, 0),  // north-east
    (-2, -1, 1), // north-west
    (2, 1, 0),   // south-east
    (-2, 1, 1),  // south-west
    (0, -1, 2),  // above
    (0, 1, 2),   // below
];

/// How far out to push a label on a leader before giving up on it.
const LEADER_RINGS: isize = 14;

pub fn place(
    mut candidates: Vec<Candidate>,
    occ: &mut Occupancy,
    sub_x: usize,
    sub_y: usize,
) -> Vec<Placed> {
    candidates.sort_by(|a, b| b.rank.cmp(&a.rank));
    let mut out = Vec::with_capacity(candidates.len());

    for c in candidates {
        let mx = c.anchor[0] / sub_x as f64;
        let my = c.anchor[1] / sub_y as f64;
        if mx < 0.0 || my < 0.0 || mx >= occ.w as f64 || my >= occ.h as f64 {
            continue;
        }
        let (mcx, mcy) = (mx as isize, my as isize);
        let len = c.text.chars().count();

        let marker = c.marker.map(|ch| (mcx as usize, mcy as usize, ch));
        if marker.is_some() {
            occ.take(mcx.max(0) as usize, mcy.max(0) as usize, 1);
        }

        let mut found: Option<((usize, usize), bool)> = None;

        'search: for ring in 0..=LEADER_RINGS {
            for (dx, dy, align) in SLOTS {
                let push = if ring == 0 { 0 } else { ring + 1 };
                let x = mcx + dx + dx.signum() * push;
                let y = mcy + dy + dy.signum() * push;
                let x = match align {
                    1 => x - len as isize + 1,
                    2 => x - (len as isize) / 2,
                    _ => x,
                };
                if occ.free(x, y, len) {
                    found = Some(((x as usize, y as usize), ring > 0));
                    break 'search;
                }
            }
        }

        let Some(((tx, ty), needs_leader)) = found else {
            continue;
        };
        occ.take(tx, ty, len);

        let leader = needs_leader.then(|| {
            // Aim at whichever end of the text run faces the anchor.
            let text_mid_y = (ty * sub_y + sub_y / 2) as f64;
            let near_end = if (tx as f64) > mx {
                (tx * sub_x) as f64 - 1.0
            } else {
                ((tx + len) * sub_x) as f64
            };
            (c.anchor, [near_end, text_mid_y])
        });

        out.push(Placed {
            cell: (tx, ty),
            text: c.text,
            tint: c.tint,
            depth: c.depth,
            bold: c.rank >= 180,
            marker,
            leader,
            feature: c.feature,
        });
    }

    out
}
