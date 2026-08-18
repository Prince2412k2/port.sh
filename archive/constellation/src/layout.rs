//! Where the stars go.
//!
//! Two problems, solved separately.
//!
//! **Placement** is a small force simulation: every star is sprung toward the
//! anchor of each project that claims it and repelled by every other star. A
//! skill used by one project settles into that project's patch of sky; a skill
//! used by four is pulled four ways at once and comes to rest between them.
//! That is the entire provenance argument — a shared skill is *visibly* shared,
//! because it could not have ended up anywhere else.
//!
//! **Figures** are a minimum spanning tree per constellation. Real constellation
//! lines are a drawn convention, not a fact about stars, and an MST is the
//! honest version of that convention: connect the sky's own nearest neighbours
//! and stop. Anything denser starts asserting relationships between skills that
//! the sheet never claimed.
//!
//! Both are deterministic. There is no clock and no RNG in here — jitter comes
//! from hashing the star's own id — so the sky is identical on every machine
//! and for every visitor, and a screenshot stays true.

use crate::data::Sky;

/// Iterations of the force loop. Well past settling; it costs microseconds.
const STEPS: usize = 700;

/// Spring constant toward each claiming constellation's anchor.
const PULL: f64 = 0.055;

/// Rest length of that spring, in sky units — so a star is pulled toward a
/// *ring* around its project rather than toward the middle of it.
///
/// Two things fall out of this and both are load-bearing. A constellation stops
/// being a blob and becomes a figure with a shape you can recognise, which is
/// what constellations are for. And the middle of every project is empty, which
/// is where the project's own name and description go — the text is not laid
/// over the stars, the stars are arranged around the text.
///
/// The ring is an ellipse, not a circle, because the thing that goes in the
/// middle of it is a paragraph. Text is far wider than it is tall and a
/// terminal is wider than it is tall; a circular ring sized to clear a
/// paragraph horizontally is then far taller than the frame. These two numbers
/// are roughly the aspect of the card they exist to make room for.
pub const REST_X: f64 = 42.0;
pub const REST_Y: f64 = 25.0;

/// Extra weight on the spring toward the project a star was *learned* in.
///
/// Without it a skill shared by two projects settles exactly halfway between
/// them, which puts it inside neither figure — you focus a project and its most
/// important skill is off the edge of the frame, sitting in the gap. The sheet
/// already names which project the story happened in, so leaning the star two
/// to one toward it keeps the figure whole while still visibly reaching for
/// wherever else the skill turned up.
const HOME_WEIGHT: f64 = 2.6;

/// Repulsion between any two stars, and the distance past which it is ignored.
const PUSH: f64 = 26.0;
const PUSH_CUTOFF: f64 = 30.0;

/// Hard floor on separation, applied after the forces each step. Two stars
/// closer than this resolve to one glyph and one label survives — a constraint
/// is more reliable here than tuning the forces until it stops happening.
const MIN_GAP: f64 = 4.6;

/// How far a star may be jittered off its seed position, in sky units.
const JITTER: f64 = 3.5;

pub struct Layout {
    /// Resting position of each star, in sky units.
    pub pos: Vec<[f64; 2]>,
    /// Per constellation: the MST over its own stars, as pairs of star indices.
    pub edges: Vec<Vec<(usize, usize)>>,
    /// Bounding box of every star, for the initial fit.
    pub min: [f64; 2],
    pub max: [f64; 2],
}

pub fn solve(sky: &Sky) -> Layout {
    let n = sky.stars.len();
    let mut pos = vec![[0.0; 2]; n];

    // Where each star sits in the queue of skills its project has entirely to
    // itself, so those can be dealt evenly around the ring. Seeding the angle
    // from the star's name instead leaves a project with three skills liable to
    // put all three in the same arc, and the ring never recovers — repulsion
    // spreads stars apart, but it has no opinion about going the long way round.
    //
    // Only the exclusive ones get a slot. A shared skill is going to be pulled
    // out of the ring toward whatever else claims it, so allotting it a place
    // on the circle just leaves a gap there — the ring comes out as an arc with
    // bites missing. Those seed at their own centre of gravity instead and
    // arrive where they were always going to end up.
    let mut slot = vec![None; n];
    for c in 0..sky.cons.len() {
        let own: Vec<usize> = (0..n)
            .filter(|&i| sky.stars[i].members.len() == 1 && sky.stars[i].members[0] == c)
            .collect();
        for (k, &i) in own.iter().enumerate() {
            slot[i] = Some((k, own.len()));
        }
    }

    for (i, star) in sky.stars.iter().enumerate() {
        // Seed at the same weighted centroid the springs are pulling toward,
        // so the simulation starts near the answer and cannot fold a
        // constellation inside out on its way there.
        let mut c = [0.0; 2];
        let total = HOME_WEIGHT + (star.members.len() - 1) as f64;
        for (n, &m) in star.members.iter().enumerate() {
            let w = if n == 0 { HOME_WEIGHT } else { 1.0 };
            c[0] += sky.cons[m].at[0] * w;
            c[1] += sky.cons[m].at[1] * w;
        }

        let (jx, jy) = jitter(&star.id);
        pos[i] = match slot[i] {
            // Dealt around the ring of the project that owns it, with a little
            // play from its own name so the result is not visibly a clock face.
            Some((k, of)) => {
                let home = sky.cons[star.members[0]].at;
                let theta = std::f64::consts::TAU * (k as f64 + 0.16 * jx) / of as f64;
                [
                    home[0] + theta.cos() * REST_X + jy * JITTER,
                    home[1] + theta.sin() * REST_Y + jx * JITTER,
                ]
            }
            // Shared: start at the balance of everything claiming it, which is
            // roughly where the springs are going to leave it anyway.
            None => [c[0] / total + jx * JITTER, c[1] / total + jy * JITTER],
        };
    }

    let mut force = vec![[0.0; 2]; n];
    for step in 0..STEPS {
        // Cooling. Early steps move far enough to escape a bad seed; late ones
        // only polish, so the result does not depend on iteration count.
        let heat = 1.0 - 0.9 * (step as f64 / STEPS as f64);

        for f in force.iter_mut() {
            *f = [0.0; 2];
        }

        for i in 0..n {
            // Divided by the number of claims, so every star is sprung with
            // the same total stiffness. Summing the springs instead makes a
            // skill shared by four projects four times as rigid as one used
            // once — it stops being placed by the sky and starts anchoring
            // it, shoving its neighbours out of shape on the way.
            let star = &sky.stars[i];
            let total = HOME_WEIGHT + (star.members.len() - 1) as f64;
            for (n, &m) in star.members.iter().enumerate() {
                let w = if n == 0 { HOME_WEIGHT } else { 1.0 };
                let k = PULL * w / total;
                let a = sky.cons[m].at;
                let (dx, dy) = (a[0] - pos[i][0], a[1] - pos[i][1]);
                // Floored, not guarded: a star exactly on the anchor has no
                // direction to be pushed out along, and dividing by nothing
                // sends it to infinity in one step.
                // Distance measured in units of the ellipse: 1.0 is on the
                // ring, less is inside it, more is outside. Floored rather
                // than guarded — a star exactly on the anchor has no direction
                // to be pushed out along, and dividing by nothing sends it to
                // infinity in a single step.
                let e = ((dx / REST_X).powi(2) + (dy / REST_Y).powi(2))
                    .sqrt()
                    .max(0.02);
                // Positive outside the ring, negative inside it.
                let stretch = 1.0 - 1.0 / e;
                force[i][0] += dx * k * stretch;
                force[i][1] += dy * k * stretch;
            }
        }

        for i in 0..n {
            for j in (i + 1)..n {
                let dx = pos[i][0] - pos[j][0];
                let dy = pos[i][1] - pos[j][1];
                let d2 = dx * dx + dy * dy;
                if d2 > PUSH_CUTOFF * PUSH_CUTOFF {
                    continue;
                }
                // Floor the distance rather than the force: two stars seeded on
                // top of each other would otherwise divide by nothing and leave
                // the frame in one step.
                let d = d2.sqrt().max(0.5);
                let f = PUSH / (d * d);
                force[i][0] += dx / d * f;
                force[i][1] += dy / d * f;
                force[j][0] -= dx / d * f;
                force[j][1] -= dy / d * f;
            }
        }

        for i in 0..n {
            pos[i][0] += force[i][0] * heat;
            pos[i][1] += force[i][1] * heat;
        }

        separate(&mut pos);
    }

    let edges = (0..sky.cons.len())
        .map(|c| mst(&sky.members_of(c), &pos))
        .collect();

    let mut min = [f64::INFINITY; 2];
    let mut max = [f64::NEG_INFINITY; 2];
    for p in &pos {
        min[0] = min[0].min(p[0]);
        min[1] = min[1].min(p[1]);
        max[0] = max[0].max(p[0]);
        max[1] = max[1].max(p[1]);
    }

    Layout { pos, edges, min, max }
}

/// Push apart any pair closer than `MIN_GAP`, symmetrically.
fn separate(pos: &mut [[f64; 2]]) {
    let n = pos.len();
    for i in 0..n {
        for j in (i + 1)..n {
            let dx = pos[i][0] - pos[j][0];
            let dy = pos[i][1] - pos[j][1];
            let d = (dx * dx + dy * dy).sqrt();
            if d >= MIN_GAP {
                continue;
            }
            // Exactly coincident has no direction to separate along, so the
            // hash supplies one. Deterministic, like everything else here.
            let (ux, uy) = if d < 1e-6 {
                let a = (i * 37 + j) as f64 * 0.618_033_988_7 * std::f64::consts::TAU;
                (a.cos(), a.sin())
            } else {
                (dx / d, dy / d)
            };
            let shove = (MIN_GAP - d) * 0.5;
            pos[i][0] += ux * shove;
            pos[i][1] += uy * shove;
            pos[j][0] -= ux * shove;
            pos[j][1] -= uy * shove;
        }
    }
}

/// Prim's, over the members of one constellation. They are at most a dozen, so
/// the O(n^2) form is both faster and shorter than anything with a heap in it.
fn mst(members: &[usize], pos: &[[f64; 2]]) -> Vec<(usize, usize)> {
    if members.len() < 2 {
        return Vec::new();
    }
    let mut inside = vec![members[0]];
    let mut outside: Vec<usize> = members[1..].to_vec();
    let mut edges = Vec::with_capacity(members.len() - 1);

    while !outside.is_empty() {
        let mut best = (f64::INFINITY, 0usize, 0usize);
        for (oi, &o) in outside.iter().enumerate() {
            for &i in &inside {
                let dx = pos[o][0] - pos[i][0];
                let dy = pos[o][1] - pos[i][1];
                let d = dx * dx + dy * dy;
                if d < best.0 {
                    best = (d, oi, i);
                }
            }
        }
        let (_, oi, from) = best;
        let to = outside.remove(oi);
        edges.push((from, to));
        inside.push(to);
    }
    edges
}

/// Two deterministic values in -1..1 from a string, via FNV-1a.
///
/// The jitter has to be stable across runs and machines — a portfolio whose
/// sky is subtly different in every screenshot is a portfolio nobody can
/// describe — so it is derived from the star's own name rather than a clock.
fn jitter(id: &str) -> (f64, f64) {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in id.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x1000_0000_01b3);
    }
    let a = ((h >> 11) as f64 / (1u64 << 53) as f64) * 2.0 - 1.0;
    let b = ((h.rotate_left(29) >> 11) as f64 / (1u64 << 53) as f64) * 2.0 - 1.0;
    (a, b)
}

/// What the simulation settled on, as text.
///
/// The `at` anchors in the sheet are the one thing in this program meant to be
/// tuned by hand, and tuning them blind is guesswork — a constellation can look
/// wrong because its anchor is badly placed or because half its stars are
/// shared and being pulled elsewhere, and those want opposite fixes.
pub fn report(sky: &Sky, lay: &Layout, only: Option<&str>) -> String {
    let mut out = String::new();

    if let Some(id) = only {
        let Some(c) = sky.con_by_id(id) else {
            return format!("no constellation called {id:?}\n");
        };
        let a = sky.cons[c].at;
        out.push_str(&format!(
            "{} at {:.0},{:.0}\n\n{:<24} {:>8} {:>8} {:>8}   claimed by\n",
            sky.cons[c].id, a[0], a[1], "star", "x", "y", "from at"
        ));
        let mut m = sky.members_of(c);
        m.sort_by(|&x, &y| {
            let d = |s: usize| (lay.pos[s][0] - a[0]).hypot(lay.pos[s][1] - a[1]);
            d(x).partial_cmp(&d(y)).unwrap()
        });
        for s in m {
            let star = &sky.stars[s];
            let names: Vec<&str> = star.members.iter().map(|&i| sky.cons[i].id.as_str()).collect();
            out.push_str(&format!(
                "{:<24} {:>8.1} {:>8.1} {:>8.1}   {}\n",
                star.id,
                lay.pos[s][0],
                lay.pos[s][1],
                (lay.pos[s][0] - a[0]).hypot(lay.pos[s][1] - a[1]),
                names.join(", ")
            ));
        }
        return out;
    }

    out.push_str(&format!(
        "{:<16} {:>5} {:>7} {:>7} {:>7} {:>7}   shared\n",
        "constellation", "stars", "median", "p75", "max", "anchor"
    ));
    for (c, con) in sky.cons.iter().enumerate() {
        let members = sky.members_of(c);
        let mut d: Vec<f64> = members
            .iter()
            .map(|&s| (lay.pos[s][0] - con.at[0]).hypot(lay.pos[s][1] - con.at[1]))
            .collect();
        d.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let at = |q: f64| d[((d.len() as f64 * q) as usize).min(d.len() - 1)];
        let shared = members
            .iter()
            .filter(|&&s| sky.stars[s].members.len() > 1)
            .count();
        out.push_str(&format!(
            "{:<16} {:>5} {:>7.1} {:>7.1} {:>7.1} {:>3.0},{:<3.0}   {shared}\n",
            con.id,
            members.len(),
            at(0.5),
            at(0.75),
            d[d.len() - 1],
            con.at[0],
            con.at[1],
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data;

    fn shipped() -> (data::Sky, Layout) {
        let sky = data::parse(include_str!("../data/skills.sky")).unwrap();
        let lay = solve(&sky);
        (sky, lay)
    }

    #[test]
    fn no_two_stars_share_a_position() {
        let (_, lay) = shipped();
        for i in 0..lay.pos.len() {
            for j in (i + 1)..lay.pos.len() {
                let dx = lay.pos[i][0] - lay.pos[j][0];
                let dy = lay.pos[i][1] - lay.pos[j][1];
                let d = (dx * dx + dy * dy).sqrt();
                assert!(d >= MIN_GAP - 0.05, "stars {i} and {j} are {d:.2} apart");
            }
        }
    }

    #[test]
    fn every_constellation_is_one_connected_figure() {
        let (sky, lay) = shipped();
        for c in 0..sky.cons.len() {
            let members = sky.members_of(c);
            assert_eq!(
                lay.edges[c].len(),
                members.len() - 1,
                "{} is not a tree",
                sky.cons[c].id
            );
            // A spanning tree reaches everything; a merely acyclic set does not.
            let mut seen = vec![members[0]];
            let mut grew = true;
            while grew {
                grew = false;
                for &(a, b) in &lay.edges[c] {
                    for (x, y) in [(a, b), (b, a)] {
                        if seen.contains(&x) && !seen.contains(&y) {
                            seen.push(y);
                            grew = true;
                        }
                    }
                }
            }
            assert_eq!(seen.len(), members.len(), "{} is not connected", sky.cons[c].id);
        }
    }

    #[test]
    fn a_shared_star_lands_between_its_projects() {
        let (sky, lay) = shipped();
        // `go` is claimed by four projects. It should sit nearer the middle of
        // them than any single-project star does — that is the whole reason the
        // layout is a simulation rather than a table of coordinates.
        let go = sky.star_by_id("go").unwrap();
        let solo = sky.star_by_id("curses").unwrap();
        let spread = |s: usize| -> f64 {
            sky.stars[s]
                .members
                .iter()
                .map(|&m| {
                    let a = sky.cons[m].at;
                    ((a[0] - lay.pos[s][0]).powi(2) + (a[1] - lay.pos[s][1]).powi(2)).sqrt()
                })
                .fold(0.0, f64::max)
        };
        assert!(spread(go) > spread(solo));
    }

    #[test]
    fn the_layout_is_reproducible() {
        let sky = data::parse(include_str!("../data/skills.sky")).unwrap();
        let a = solve(&sky);
        let b = solve(&sky);
        assert_eq!(a.pos, b.pos);
    }
}
