//! Terrain relief: the pass that makes a tilt read as 3D rather than as skew.
//!
//! A world-aligned grid is sampled from the heightmap, hillshaded from finite
//! differences, then projected with elevation. Drawing it as dots rather than a
//! surface is deliberate -- the stipple already reads as "ground" against the
//! solid strokes used for roads, and a dot grid needs no triangulation, no
//! backface culling and no seams.

use crate::canvas::{Brush, Canvas, MAT_DOT, TINT_GREEN};
use crate::geo::{meters_per_world_unit, world_to_lonlat, Viewport};
use crate::terrain::Terrain;

/// Subpixels between grid samples. Denser than this and the relief turns into a
/// solid wash that competes with the roads it is supposed to sit behind.
const STEP: f64 = 3.0;

/// Vertical exaggeration. Real relief is imperceptible at map scale -- India's
/// tallest ground is under 0.1% of the width of the country -- so terrain in
/// tilted views is always exaggerated. This is the usual cartographic lie.
pub const EXAG: f64 = 14.0;

/// Highest elevation used to normalise shading, metres.
const MAX_ELEV: f32 = 6000.0;


#[derive(Default)]
pub struct Relief {
    /// Sample heights, reused across frames to avoid reallocating.
    heights: Vec<f32>,
    gw: usize,
    gh: usize,
}

impl Relief {
    /// Draw the terrain surface. Returns the number of samples plotted.
    pub fn draw(&mut self, t: &Terrain, canvas: &mut Canvas, vp: &Viewport, datum: f32) -> usize {
        // Sampling in screen space rather than world space keeps the grid
        // uniform on the display no matter how the camera is turned.
        self.gw = (canvas.sw as f64 / STEP).ceil() as usize + 3;
        self.gh = (canvas.sh as f64 / STEP).ceil() as usize + 3;
        self.heights.clear();
        self.heights.resize(self.gw * self.gh, 0.0);

        // Fraction of the screen left as sky when tilted.
        //
        // A parallel projection has no vanishing point, so the ground plane
        // covers the whole frame and terrain runs edge to edge with nothing to
        // read a silhouette against. Clipping the far distance manufactures a
        // horizon: ground stops, peaks rise past it into black, and the eye
        // finally gets the cue it was missing.
        let over = 1.0;
        let plate = vp.plate();
        let bounded = !vp.is_flat();

        let mut world = vec![[0.0f64; 2]; self.gw * self.gh];
        for gy in 0..self.gh {
            for gx in 0..self.gw {
                // Alternate rows by half a sample. A square grid turns every
                // steep slope into aligned vertical ribbons at terminal
                // resolution; this triangular grid has the same sample count
                // and spacing but no screen-wide column alias.
                let stagger = if gy & 1 == 0 { 0.0 } else { STEP * 0.5 };
                let sx = (gx as f64 - 1.0) * STEP + stagger;
                let sy = (gy as f64 - 1.0) * STEP * over - canvas.sh as f64 * (over - 1.0);
                let w = vp.unproject([sx, sy]);
                let (lon, lat) = world_to_lonlat(w[0], w[1]);
                let i = gy * self.gw + gx;
                world[i] = w;
                self.heights[i] = t.sample(lon, lat);
            }
        }

        let (_, clat) = vp.center_lonlat();
        let m_per_world = meters_per_world_unit(clat);
        let exag = if vp.is_flat() { 0.0 } else { EXAG };
        let mut plotted = 0usize;

        // Column-major, marching far to near. That ordering is the whole trick:
        // a bare grid of dots has no surface to hide anything behind, so each
        // sample is drawn as a vertical ribbon down to its nearer neighbour,
        // and nearer ribbons paint over farther ones. Occlusion by a ridge then
        // falls out of the draw order without a visibility test.
        for gx in 1..self.gw - 1 {
            for gy in 1..self.gh - 2 {
                let i = gy * self.gw + gx;
                let h = self.heights[i];
                // Sea level is not terrain; the water layer already owns it.
                if h < 1.0 {
                    continue;
                }
                // Outside the slab there is no ground, so nothing is drawn and
                // the plate keeps a clean edge.
                let mut fade = 1.0f32;
                if bounded {
                    let m = vp.plane_of(world[i]);
                    if m[0].abs() > plate[0] || m[1] < plate[1] || m[1] > plate[2] {
                        continue;
                    }
                    fade = crate::scene::plate_fade(m, plate);
                    if fade <= 0.02 {
                        continue;
                    }
                }

                // Finite differences off the sampled grid rather than extra
                // heightmap lookups -- the neighbours are already in hand.
                let dzdx = self.heights[i + 1] - self.heights[i - 1];
                let dzdy = self.heights[i + self.gw] - self.heights[i - self.gw];

                // Lambert shading against a fixed north-west light, the
                // convention every printed relief map uses.
                let nx = -dzdx;
                let ny = -dzdy;
                let nz = 60.0;
                let len = (nx * nx + ny * ny + nz * nz).sqrt().max(1e-6);
                let lambert = ((nx * -0.5 + ny * -0.5 + nz * 0.7) / len).clamp(0.0, 1.0);

                // Weighted towards slope rather than height. Terrain drawn by
                // elevation alone fills a whole city with texture and buries
                // the map; drawn by slope, flat ground stays empty and only
                // real relief shows, which is what the eye wants from it.
                let slope = ((dzdx * dzdx + dzdy * dzdy).sqrt() / 260.0).clamp(0.0, 1.0);
                let band = (h / MAX_ELEV).clamp(0.0, 1.0).powf(0.6);
                let relief = 0.72 * slope + 0.28 * (1.0 - lambert);
                if relief < 0.06 && band < 0.10 {
                    continue;
                }
                let alpha = (0.10 + 0.80 * relief) * (0.55 + 0.45 * band) * fade;

                let hw = (h - datum) as f64 * exag / m_per_world;
                let (sp, depth) = vp.project3(world[i], hw);
                if !depth.is_finite() {
                    continue;
                }

                let brush = Brush {
                    depth,
                    tint: TINT_GREEN,
                    mat: MAT_DOT,
                    pick: u32::MAX,
                    occlude: true,
                };

                // The next row nearer: the ribbon spans the gap to it, so the
                // surface is continuous instead of a field of specks.
                let j = i + self.gw;
                let hn = self.heights[j];
                let hnw = (hn - datum) as f64 * exag / m_per_world;
                let (sp_near, dn) = vp.project3(world[j], hnw);
                if !dn.is_finite() {
                    continue;
                }

                let y0 = sp[1];
                let y1 = sp_near[1].max(y0);
                let span = (y1 - y0).max(1.0);
                // Paint only a short hatch towards the nearer sample. Filling
                // the entire occlusion ribbon produced screen-high columns on
                // steep ground: correct geometry, but a curtain rather than a
                // hill. A capped stroke keeps slope direction without the
                // alias; the z-buffer still receives the complete ribbon below.
                let paint_to = y0 + (y1 - y0).min(2.5);
                let mut py = y0;
                while py <= paint_to {
                    let t = (py - y0) / span;
                    let x = sp[0] + (sp_near[0] - sp[0]) * t;
                    canvas.splat(x, py, alpha, &brush);
                    py += 1.0;
                }
                let mut y = y0;
                while y <= y1 {
                    // Join the projected samples, not merely their y values.
                    // With the stagger above, fixing x here would recreate the
                    // vertical ribbons the sampling pattern exists to remove.
                    let t = (y - y0) / span;
                    let x = sp[0] + (sp_near[0] - sp[0]) * t;
                    // The ribbon is opaque ground whether or not the stipple
                    // happened to paint here, so claim both subpixel columns
                    // it spans. Otherwise roads behind a ridge leak through the
                    // gaps between dots.
                    let xi = x as isize;
                    canvas.occlude_at(xi, y as isize, depth);
                    canvas.occlude_at(xi + 1, y as isize, depth);
                    y += 1.0;
                }
                plotted += 1;
            }
        }
        plotted
    }
}
