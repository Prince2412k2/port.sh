//! Where you are.
//!
//! Resolved in order of how much it can be trusted:
//!
//! 1. `TERMAP_HOME=lat,lon` -- explicit, and the only one worth relying on if
//!    the position is going to be shown to anyone.
//! 2. `~/.config/termap/home` -- the same thing, persisted.
//! 3. IP geolocation -- city-level at best, and wrong entirely behind a VPN.
//!
//! The lookup runs on its own thread and the result appears when it appears;
//! blocking startup on a network round trip to draw one marker would be a poor
//! trade. Deliberately plain HTTP over a raw socket: the alternative is a TLS
//! stack, and this is not a secret worth protecting -- the server already knows
//! the address it is answering.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone)]
pub struct Fix {
    /// World (Mercator) coordinates.
    pub world: [f64; 2],
    pub lonlat: (f64, f64),
    pub label: String,
    /// Radius the source claims, in km. IP lookups are optimistic about this.
    pub accuracy_km: f64,
    pub source: &'static str,
}

pub type Slot = Arc<Mutex<Option<Fix>>>;

fn config_path(kind: &str) -> Option<PathBuf> {
    let base = std::env::var_os("HOME")?;
    Some(PathBuf::from(base).join(".config/termap").join(kind))
}

fn parse_pair(s: &str) -> Option<(f64, f64)> {
    let (a, b) = s.trim().split_once(',')?;
    Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
}

fn fix_from(lat: f64, lon: f64, label: String, acc: f64, source: &'static str) -> Fix {
    Fix {
        world: crate::geo::lonlat_to_world(lon, lat),
        lonlat: (lon, lat),
        label,
        accuracy_km: acc,
        source,
    }
}

/// Start resolving. Returns immediately; the slot fills in later, if at all.
pub fn spawn() -> Slot {
    let slot: Slot = Arc::new(Mutex::new(None));

    // An explicit position needs no thread and no network.
    if let Some((lat, lon)) = std::env::var("TERMAP_HOME").ok().as_deref().and_then(parse_pair) {
        *slot.lock().unwrap() = Some(fix_from(lat, lon, "home".into(), 0.0, "TERMAP_HOME"));
        return slot;
    }
    if let Some((lat, lon)) = config_path("home")
        .and_then(|p| std::fs::read_to_string(p).ok())
        .as_deref()
        .and_then(parse_pair)
    {
        *slot.lock().unwrap() = Some(fix_from(lat, lon, "home".into(), 0.0, "config"));
        return slot;
    }

    let out = slot.clone();
    std::thread::spawn(move || {
        if let Some(fix) = lookup_by_ip() {
            *out.lock().unwrap() = Some(fix);
        }
    });
    slot
}

fn lookup_by_ip() -> Option<Fix> {
    const HOST: &str = "ip-api.com";
    let req = format!(
        "GET /json/?fields=status,city,regionName,country,lat,lon HTTP/1.1\r\n\
         Host: {HOST}\r\nUser-Agent: termap/0.1\r\nConnection: close\r\n\r\n"
    );

    let mut stream = TcpStream::connect((HOST, 80)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(6))).ok()?;
    stream.set_write_timeout(Some(Duration::from_secs(6))).ok()?;
    stream.write_all(req.as_bytes()).ok()?;

    let mut buf = String::new();
    stream.take(64 * 1024).read_to_string(&mut buf).ok()?;
    let body = buf.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or(&buf);

    if field(body, "status")? != "success" {
        return None;
    }
    let lat: f64 = field(body, "lat")?.parse().ok()?;
    let lon: f64 = field(body, "lon")?.parse().ok()?;
    let label = [
        field(body, "city").unwrap_or_default(),
        field(body, "regionName").unwrap_or_default(),
    ]
    .iter()
    .filter(|s| !s.is_empty())
    .cloned()
    .collect::<Vec<_>>()
    .join(", ");

    if let Some(p) = config_path("last-ip-fix") {
        let _ = std::fs::create_dir_all(p.parent()?);
        let _ = std::fs::write(p, format!("{lat},{lon}\n"));
    }
    // IP databases resolve to a city centroid, sometimes to a whole region.
    // Ten kilometres is generous rather than pessimistic.
    Some(fix_from(lat, lon, label, 10.0, "IP"))
}

/// Pull one value out of a flat JSON object. Enough for six fields; not enough
/// to deserve a dependency.
fn field<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let at = json.find(&format!("\"{key}\":"))? + key.len() + 3;
    let rest = json.get(at..)?.trim_start();
    if let Some(s) = rest.strip_prefix('"') {
        s.split('"').next()
    } else {
        rest.split([',', '}']).next().map(str::trim)
    }
}
