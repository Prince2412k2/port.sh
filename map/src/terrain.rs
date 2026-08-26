//! Heightmap sampling.
//!
//! Reads the flat grid baked by `scripts/dem2hgt.py`. Deliberately dumb: one
//! header, one block of i16 metres. The grid is memory-mapped rather than read
//! into the heap, so resident cost is the working set instead of the whole
//! file, the page cache shares one copy across processes for free, and a world
//! heightmap at this fidelity stops being a memory problem before it exists.

use std::io::Read;
use std::path::Path;

use memmap2::{Mmap, MmapOptions};

const NODATA: i16 = -32768;
/// Magic, version, four f64 bounds, two u32 dimensions.
const HEADER_LEN: u64 = 48;

pub struct Terrain {
    west: f64,
    south: f64,
    east: f64,
    north: f64,
    width: usize,
    height: usize,
    /// Row 0 is the north edge.
    data: Mmap,
}

impl Terrain {
    pub fn open(path: &Path) -> std::io::Result<Self> {
        let mut f = std::fs::File::open(path)?;
        let mut head = [0u8; HEADER_LEN as usize];
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

        // SAFETY: the file is baked once by `scripts/dem2hgt.py` and never
        // rewritten while the map is running, so nothing can shorten or edit
        // the bytes underneath the mapping.
        let data = unsafe { MmapOptions::new().offset(HEADER_LEN).map(&f)? };
        if data.len() < width * height * 2 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "heightmap truncated",
            ));
        }

        Ok(Terrain { west, south, east, north, width, height, data })
    }

    #[inline]
    fn at(&self, x: isize, y: isize) -> f32 {
        if x < 0 || y < 0 || x >= self.width as isize || y >= self.height as isize {
            return 0.0;
        }
        let i = (y as usize * self.width + x as usize) * 2;
        let v = i16::from_le_bytes([self.data[i], self.data[i + 1]]);
        if v == NODATA {
            0.0
        } else {
            v as f32
        }
    }

    /// Ground metres between one sample and the next, at a latitude.
    ///
    /// The renderer needs this to know when it is drawing detail the heightmap
    /// does not have. The shipped grid is 30 arcsec, about 850 m at Indian
    /// latitudes, which is coarser than a screen pixel from about z10 upward.
    pub fn spacing_m(&self, lat: f64) -> f64 {
        let deg = (self.east - self.west) / self.width as f64;
        deg * 111_320.0 * lat.to_radians().cos().max(0.05)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn bake(name: &str, cells: &[i16], w: usize, h: usize) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("termap-terrain-{}-{name}.tmhg", std::process::id()));
        let mut buf = Vec::new();
        buf.extend_from_slice(b"TMHG");
        buf.push(1);
        buf.extend_from_slice(&[0u8; 3]);
        buf.extend_from_slice(&10.0f64.to_le_bytes()); // west
        buf.extend_from_slice(&20.0f64.to_le_bytes()); // south
        buf.extend_from_slice(&12.0f64.to_le_bytes()); // east
        buf.extend_from_slice(&22.0f64.to_le_bytes()); // north
        buf.extend_from_slice(&(w as u32).to_le_bytes());
        buf.extend_from_slice(&(h as u32).to_le_bytes());
        for c in cells {
            buf.extend_from_slice(&c.to_le_bytes());
        }
        std::fs::File::create(&p).unwrap().write_all(&buf).unwrap();
        p
    }

    #[test]
    fn samples_come_off_the_map_little_endian_with_nodata_as_sea() {
        // [100, -50, NODATA / 250,   7,    42]
        let p = bake("grid", &[100, -50, NODATA, 250, 7, 42], 3, 2);
        let t = Terrain::open(&p).unwrap();

        // The centre of cell (0, 0), read back exactly.
        assert_eq!(t.sample(10.0 + 1.0 / 3.0, 21.5), 100.0);
        // A negative height survives the mapping byte for byte.
        assert_eq!(t.sample(11.0, 21.5), -50.0);
        // NODATA is sea, and sea is zero.
        assert_eq!(t.sample(10.0 + 5.0 / 3.0, 21.5), 0.0);
        // Bilinear across four cells: a quarter of the way in from (0, 0).
        let got = t.sample(10.5, 21.25);
        assert!((got - 94.1875).abs() < 1e-4, "got {got}");
        // Outside the grid is zero on every side.
        assert_eq!(t.sample(9.9, 21.0), 0.0);
        assert_eq!(t.sample(12.1, 21.0), 0.0);
        assert_eq!(t.sample(11.0, 22.1), 0.0);
        assert_eq!(t.sample(11.0, 19.9), 0.0);

        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn a_truncated_grid_is_an_error_not_a_panicking_index() {
        let p = bake("short", &[100, -50], 3, 2);
        assert!(matches!(
            Terrain::open(&p),
            Err(ref e) if e.kind() == std::io::ErrorKind::UnexpectedEof
        ));
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn not_a_heightmap_is_rejected() {
        let p = bake("badmagic", &[1; 6], 3, 2);
        let raw = std::fs::read(&p).unwrap();
        let mut bad = raw.clone();
        bad[0] = b'X';
        std::fs::write(&p, &bad).unwrap();
        assert!(matches!(
            Terrain::open(&p),
            Err(ref e) if e.kind() == std::io::ErrorKind::InvalidData
        ));
        let mut old = raw.clone();
        old[4] = 2;
        std::fs::write(&p, &old).unwrap();
        assert!(matches!(
            Terrain::open(&p),
            Err(ref e) if e.kind() == std::io::ErrorKind::InvalidData
        ));
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn the_real_heightmap_opens_without_becoming_resident() {
        let Some(p) = crate::paths::data_file("india.tmhg") else { return };
        let t = Terrain::open(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));

        let (mut sea, mut high) = (false, false);
        for i in 0..2000 {
            let lon = t.west + (i as f64 + 0.5) / 2000.0 * (t.east - t.west);
            let lat = t.south + (((i * 7) % 1999) as f64 + 0.5) / 2000.0 * (t.north - t.south);
            let m = t.sample(lon, lat);
            sea |= m == 0.0;
            high |= m > 500.0;
        }
        assert!(sea, "no ocean anywhere in the grid?");
        assert!(high, "no land above 500 m anywhere in the grid?");
    }
}
