//! Immutable elevation data publication; never constructs a visual scene.
#[path = "../../../../map/src/terrain.rs"]
#[allow(dead_code)]
mod source;
// The source module's tile-composition helper is data-only and needs TileId.
pub type Source = source::Terrain;

pub fn tile(source: Option<&Source>, z: u8, x: u32, y: u32) -> Vec<u8> {
    const SIDE: usize = 49;
    let n = (1u64 << z) as f64;
    let lat = |y: f64| {
        (std::f64::consts::PI * (1.0 - 2.0 * y / n))
            .sinh()
            .atan()
            .to_degrees()
    };
    let west = x as f64 / n * 360.0 - 180.0;
    let east = (x as f64 + 1.0) / n * 360.0 - 180.0;
    let north = lat(y as f64);
    let south = lat(y as f64 + 1.0);
    let dx = (east - west) * 0.25;
    let dy = (north - south) * 0.25;
    let bounds = [
        west - dx,
        (south - dy).max(-89.0),
        east + dx,
        (north + dy).min(89.0),
    ];
    let smooth = (north - south) * 110540.0 / 32.0;
    let mut bytes = b"TMHG\x01\x00\x00\x00".to_vec();
    for value in bounds {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    for _ in 0..2 {
        bytes.extend_from_slice(&(SIDE as u32).to_le_bytes());
    }
    for row in 0..SIDE {
        for column in 0..SIDE {
            let lon = bounds[0] + (bounds[2] - bounds[0]) * column as f64 / (SIDE - 1) as f64;
            let lat = bounds[3] - (bounds[3] - bounds[1]) * row as f64 / (SIDE - 1) as f64;
            let height = source.map_or(0.0, |t| t.sample_smooth(lon, lat, smooth));
            bytes
                .extend_from_slice(&(height.round().clamp(-32767.0, 32767.0) as i16).to_le_bytes());
        }
    }
    bytes
}
