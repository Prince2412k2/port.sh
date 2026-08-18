//! Heightmap sampling.
//!
//! Reads the flat grid baked by `scripts/dem2hgt.py`. Deliberately dumb: one
//! header, one block of i16 metres. All of India at 30 arcsec is under 30 MB,
//! so it loads once and stays resident rather than being tiled like the vector
//! data -- terrain is smooth, and a pyramid would buy nothing at this fidelity.

use std::io::Read;
use std::path::Path;

const NODATA: i16 = -32768;

pub struct Terrain {
    west: f64,
    south: f64,
    east: f64,
    north: f64,
    width: usize,
    height: usize,
    /// Row 0 is the north edge.
    data: Vec<i16>,
}

impl Terrain {
    pub fn open(path: &Path) -> std::io::Result<Self> {
        let mut f = std::fs::File::open(path)?;
        let mut head = [0u8; 8 + 32 + 8];
        f.read_exact(&mut head)?;
        if &head[0..4] != b"TMHG" || head[4] != 1 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "not a v1 .tmhg heightmap",
            ));
        }
        let d = |o: usize| f64::from_le_bytes(head[o..o + 8].try_into().unwrap());
        let u = |o: usize| u32::from_le_bytes(head[o..o + 4].try_into().unwrap()) as usize;
        let (west, south, east, north) = (d(8), d(16), d(24), d(32));
        let (width, height) = (u(40), u(44));

        let mut raw = Vec::with_capacity(width * height * 2);
        f.read_to_end(&mut raw)?;
        if raw.len() < width * height * 2 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "heightmap truncated",
            ));
        }
        let data = raw
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]))
            .collect();

        Ok(Terrain { west, south, east, north, width, height, data })
    }

    #[inline]
    fn at(&self, x: isize, y: isize) -> f32 {
        if x < 0 || y < 0 || x >= self.width as isize || y >= self.height as isize {
            return 0.0;
        }
        let v = self.data[y as usize * self.width + x as usize];
        if v == NODATA {
            0.0
        } else {
            v as f32
        }
    }

    /// Metres above sea level, bilinearly interpolated. Outside the grid, and
    /// over ocean, this is zero -- which is the right answer for both.
    pub fn sample(&self, lon: f64, lat: f64) -> f32 {
        if lon < self.west || lon >= self.east || lat <= self.south || lat > self.north {
            return 0.0;
        }
        let fx = (lon - self.west) / (self.east - self.west) * self.width as f64 - 0.5;
        let fy = (self.north - lat) / (self.north - self.south) * self.height as f64 - 0.5;
        let (x0, y0) = (fx.floor(), fy.floor());
        let (tx, ty) = ((fx - x0) as f32, (fy - y0) as f32);
        let (x0, y0) = (x0 as isize, y0 as isize);

        let a = self.at(x0, y0);
        let b = self.at(x0 + 1, y0);
        let c = self.at(x0, y0 + 1);
        let d = self.at(x0 + 1, y0 + 1);
        let top = a + (b - a) * tx;
        let bot = c + (d - c) * tx;
        top + (bot - top) * ty
    }
}
