//! Revision-scoped decoded cache shared by both platform adapters. I/O stays outside.
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
use termap::data::Tile;

pub type TileKey = (u8, u32, u32);
pub type Generation = Vec<(u8, u32, u32, Arc<Tile>)>;
const LIMIT: usize = 48 * 1024 * 1024;

#[derive(Default)]
pub struct MapStore {
    entries: BTreeMap<TileKey, (Arc<Tile>, usize, u64)>,
    bytes: usize,
    clock: u64,
    protected: BTreeSet<TileKey>,
    terrain: BTreeMap<TileKey, (Arc<termap::terrain::Terrain>, u64)>,
}

impl MapStore {
    pub fn has_terrain(&self, key: &TileKey) -> bool {
        self.terrain.contains_key(key)
    }
    pub fn insert_terrain(&mut self, key: TileKey, bytes: Vec<u8>) -> bool {
        if self.has_terrain(&key) {
            return true;
        }
        if bytes.len() > 64 * 1024 {
            return false;
        }
        let Ok(terrain) = termap::terrain::Terrain::from_bytes(bytes) else {
            return false;
        };
        while self.terrain.len() >= 128 {
            let Some(oldest) = self
                .terrain
                .iter()
                .filter(|(key, _)| !self.protected.contains(key))
                .min_by_key(|(_, (_, age))| age)
                .map(|(key, _)| *key)
            else {
                return false;
            };
            self.terrain.remove(&oldest);
        }
        self.clock += 1;
        self.terrain.insert(key, (Arc::new(terrain), self.clock));
        true
    }
    pub fn terrain_generation(&mut self, wanted: &[TileKey]) -> Option<termap::terrain::Terrain> {
        if !wanted.iter().all(|key| self.has_terrain(key)) {
            return None;
        }
        self.clock += 1;
        Some(termap::terrain::Terrain::from_tiles(
            wanted
                .iter()
                .map(|&(z, x, y)| {
                    let (terrain, age) = self.terrain.get_mut(&(z, x, y)).unwrap();
                    *age = self.clock;
                    (termap::pmtiles::TileId { z, x, y }, terrain.clone())
                })
                .collect(),
        ))
    }
    pub fn protect(&mut self, wanted: &[TileKey]) {
        self.protected = wanted.iter().copied().collect();
    }
    pub fn contains(&self, key: &TileKey) -> bool {
        self.entries.contains_key(key)
    }

    pub fn insert(&mut self, key: TileKey, bytes: &[u8]) -> bool {
        if self.contains(&key) {
            return true;
        }
        if bytes.len() > 8 * 1024 * 1024 {
            return false;
        }
        let features = termap::mvt::decode_checked(
            bytes,
            termap::pmtiles::TileId {
                z: key.0,
                x: key.1,
                y: key.2,
            },
        );
        let Ok(features) = features else {
            return false;
        };
        let tile = Tile::new(features);
        let size = std::mem::size_of::<Tile>()
            + tile.features.capacity() * std::mem::size_of::<termap::data::Feature>()
            + tile
                .features
                .iter()
                .map(|f| f.pts.capacity() * 16 + f.name.as_ref().map_or(0, |n| n.len()))
                .sum::<usize>()
            + tile
                .by_layer
                .iter()
                .map(|v| v.capacity() * 4)
                .sum::<usize>();
        if size > LIMIT {
            return false;
        }
        while self.bytes + size > LIMIT || self.entries.len() >= 512 {
            let Some(oldest) = self
                .entries
                .iter()
                .filter(|(key, _)| !self.protected.contains(key))
                .min_by_key(|(_, (_, _, age))| age)
                .map(|(key, _)| *key)
            else {
                return false;
            };
            if let Some((_, size, _)) = self.entries.remove(&oldest) {
                self.bytes -= size;
            }
        }
        self.clock += 1;
        self.entries.insert(key, (Arc::new(tile), size, self.clock));
        self.bytes += size;
        true
    }

    /// Empty tiles count as complete. Errors do not: keep the prior generation visible.
    pub fn generation(&mut self, wanted: &[TileKey]) -> Option<Generation> {
        if !wanted.iter().all(|key| self.contains(key)) {
            return None;
        }
        self.clock += 1;
        Some(
            wanted
                .iter()
                .map(|&(z, x, y)| {
                    let (tile, _, age) = self.entries.get_mut(&(z, x, y)).unwrap();
                    *age = self.clock;
                    (z, x, y, tile.clone())
                })
                .collect(),
        )
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn elevation_generations_wait_for_every_mandatory_tile() {
        fn tile() -> Vec<u8> {
            let mut b = b"TMHG\x01\0\0\0".to_vec();
            for v in [-180.0f64, -85.0, 180.0, 85.0] {
                b.extend_from_slice(&v.to_le_bytes());
            }
            for _ in 0..2 {
                b.extend_from_slice(&2u32.to_le_bytes());
            }
            for _ in 0..4 {
                b.extend_from_slice(&100i16.to_le_bytes());
            }
            b
        }
        let mut cache = MapStore::default();
        let wanted = [(1, 0, 0), (1, 1, 0)];
        cache.protect(&wanted);
        assert!(cache.insert_terrain(wanted[0], tile()));
        assert!(cache.terrain_generation(&wanted).is_none());
        assert!(cache.insert_terrain(wanted[1], tile()));
        let terrain = cache.terrain_generation(&wanted).unwrap();
        assert_eq!(terrain.sample(-0.0001, 30.0), 100.0);
        assert_eq!(terrain.sample(0.0001, 30.0), 100.0);
        assert!(!cache.insert((5, 1, 1), &[0x1a, 0x7f]));
        assert!(!cache.contains(&(5, 1, 1)));
    }
    #[test]
    fn generations_wait_for_every_tile_and_accept_declared_empty() {
        let mut cache = MapStore::default();
        let wanted = [(5, 1, 1), (5, 1, 2)];
        cache.insert(wanted[0], &[]);
        assert!(cache.generation(&wanted).is_none());
        cache.insert(wanted[1], &[]);
        let generation = cache.generation(&wanted).unwrap();
        assert_eq!(generation.len(), 2);
        assert!(generation
            .iter()
            .all(|(_, _, _, tile)| tile.features.is_empty()));
        let bytes = cache.bytes();
        cache.insert(wanted[0], &[]);
        assert_eq!(cache.bytes(), bytes);
    }
}
