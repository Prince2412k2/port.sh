//! Minimal PMTiles v3 reader.
//!
//! Hand-rolled rather than pulled in: the parts of the spec that matter here are
//! a fixed 127-byte header and one varint-encoded directory format, and the
//! published crate is async-first, which would drag a runtime into an otherwise
//! synchronous program for no benefit.
//!
//! Directories are cached, so steady-state tile lookup is one seek and one read.

#[cfg(feature = "native")]
use std::collections::HashMap;
#[cfg(feature = "native")]
use std::fs::File;
#[cfg(feature = "native")]
use std::io::{Read, Seek, SeekFrom};
#[cfg(feature = "native")]
use std::path::Path;

const COMPRESSION_GZIP: u8 = 2;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct TileId {
    pub z: u8,
    pub x: u32,
    pub y: u32,
}

/// PMTiles orders tiles along a Hilbert curve within each zoom level, so that
/// neighbouring tiles are usually neighbouring bytes on disk.
pub fn hilbert_id(z: u8, x: u32, y: u32) -> u64 {
    let mut acc: u64 = 0;
    for t in 0..z {
        acc += 1u64 << (2 * t as u64);
    }
    let (mut x, mut y) = (x as u64, y as u64);
    let n = 1u64 << z as u64;
    let mut d: u64 = 0;
    let mut s: u64 = n >> 1;
    while s > 0 {
        let rx = u64::from(x & s > 0);
        let ry = u64::from(y & s > 0);
        d += s * s * ((3 * rx) ^ ry);
        if ry == 0 {
            if rx == 1 {
                x = n - 1 - x;
                y = n - 1 - y;
            }
            std::mem::swap(&mut x, &mut y);
        }
        s >>= 1;
    }
    acc + d
}

#[derive(Clone, Copy)]
struct Entry {
    id: u64,
    run: u32,
    len: u32,
    off: u64,
}

#[cfg(feature = "native")]
pub struct Archive {
    file: File,
    root: (u64, u64),
    leaf_off: u64,
    data_off: u64,
    internal_gzip: bool,
    tile_gzip: bool,
    pub min_zoom: u8,
    pub max_zoom: u8,
    /// lon/lat degrees: minx, miny, maxx, maxy.
    pub bounds: [f64; 4],
    dirs: HashMap<(u64, u64), Vec<Entry>>,
}

#[cfg(feature = "native")]
impl Archive {
    pub fn open(path: &Path) -> std::io::Result<Self> {
        let mut file = File::open(path)?;
        let mut h = [0u8; 127];
        file.read_exact(&mut h)?;
        if &h[0..7] != b"PMTiles" || h[7] != 3 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "not a PMTiles v3 archive",
            ));
        }
        let u64at = |o: usize| u64::from_le_bytes(h[o..o + 8].try_into().unwrap());
        let i32at = |o: usize| i32::from_le_bytes(h[o..o + 4].try_into().unwrap());

        Ok(Archive {
            file,
            root: (u64at(8), u64at(16)),
            leaf_off: u64at(40),
            data_off: u64at(56),
            internal_gzip: h[97] == COMPRESSION_GZIP,
            tile_gzip: h[98] == COMPRESSION_GZIP,
            min_zoom: h[100],
            max_zoom: h[101],
            bounds: [
                i32at(102) as f64 / 1e7,
                i32at(106) as f64 / 1e7,
                i32at(110) as f64 / 1e7,
                i32at(114) as f64 / 1e7,
            ],
            dirs: HashMap::new(),
        })
    }

    fn read_at(&mut self, off: u64, len: usize, gzip: bool) -> std::io::Result<Vec<u8>> {
        self.file.seek(SeekFrom::Start(off))?;
        let mut buf = vec![0u8; len];
        self.file.read_exact(&mut buf)?;
        if gzip {
            let mut out = Vec::with_capacity(len * 4);
            flate2::read::GzDecoder::new(&buf[..]).read_to_end(&mut out)?;
            return Ok(out);
        }
        Ok(buf)
    }

    fn directory(&mut self, off: u64, len: u64) -> std::io::Result<&Vec<Entry>> {
        if !self.dirs.contains_key(&(off, len)) {
            let raw = self.read_at(off, len as usize, self.internal_gzip)?;
            self.dirs.insert((off, len), deserialize_dir(&raw));
        }
        Ok(&self.dirs[&(off, len)])
    }

    /// Decompressed MVT bytes for a tile, or None if the archive has no such tile.
    pub fn tile(&mut self, t: TileId) -> std::io::Result<Option<Vec<u8>>> {
        let want = hilbert_id(t.z, t.x, t.y);
        let (mut off, mut len) = self.root;

        // Root directory, then at most a few levels of leaf directories.
        for _ in 0..4 {
            let entries = self.directory(off, len)?;
            let Some(e) = find(entries, want) else {
                return Ok(None);
            };
            let (run, elen, eoff) = (e.run, e.len, e.off);
            if run == 0 {
                // A run length of zero means the entry points at a leaf directory.
                off = self.leaf_off + eoff;
                len = elen as u64;
                continue;
            }
            let data_off = self.data_off;
            let gz = self.tile_gzip;
            return Ok(Some(self.read_at(data_off + eoff, elen as usize, gz)?));
        }
        Ok(None)
    }
}

/// Largest entry whose id is <= want, respecting run length.
#[cfg(feature = "native")]
fn find(entries: &[Entry], want: u64) -> Option<&Entry> {
    let idx = match entries.binary_search_by(|e| e.id.cmp(&want)) {
        Ok(i) => i,
        Err(0) => return None,
        Err(i) => i - 1,
    };
    let e = &entries[idx];
    if e.run == 0 || want < e.id + e.run as u64 {
        Some(e)
    } else {
        None
    }
}

fn varint(b: &[u8], p: &mut usize) -> u64 {
    let mut r = 0u64;
    let mut s = 0u32;
    while *p < b.len() {
        let x = b[*p];
        *p += 1;
        r |= ((x & 0x7F) as u64) << s;
        if x & 0x80 == 0 {
            break;
        }
        s += 7;
    }
    r
}

/// Directories store each column separately, all varint-encoded: tile ids as
/// deltas, then run lengths, then byte lengths, then offsets (where 0 means
/// "immediately after the previous entry").
fn deserialize_dir(b: &[u8]) -> Vec<Entry> {
    let mut p = 0usize;
    let n = varint(b, &mut p) as usize;
    let mut out = vec![
        Entry {
            id: 0,
            run: 0,
            len: 0,
            off: 0
        };
        n
    ];

    let mut last = 0u64;
    for e in out.iter_mut() {
        last += varint(b, &mut p);
        e.id = last;
    }
    for e in out.iter_mut() {
        e.run = varint(b, &mut p) as u32;
    }
    for e in out.iter_mut() {
        e.len = varint(b, &mut p) as u32;
    }
    for i in 0..n {
        let v = varint(b, &mut p);
        out[i].off = if v == 0 && i > 0 {
            out[i - 1].off + out[i - 1].len as u64
        } else {
            v - 1
        };
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hilbert_order_matches_pmtiles_at_zoom_one() {
        assert_eq!(hilbert_id(1, 0, 0), 1);
        assert_eq!(hilbert_id(1, 0, 1), 2);
        assert_eq!(hilbert_id(1, 1, 1), 3);
        assert_eq!(hilbert_id(1, 1, 0), 4);
    }
}
