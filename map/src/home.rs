//! Where you are.
//!
//! Resolved in order of how much it can be trusted:
//!
//! 1. `TERMAP_HOME=lat,lon` -- explicit, and the only one worth relying on if
//!    the position is going to be shown to anyone.
//! 2. `~/.config/termap/home` -- the same thing, persisted.
//! 3. The SSH client's own address, when there is one. Served over SSH, the
//!    interesting position is not the server's -- that is a datacentre -- but
//!    the visitor's, and `SSH_CONNECTION` names it.
//! 4. IP geolocation of whatever address the server has -- city-level at best,
//!    and wrong entirely behind a VPN.
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
    if let Some((lat, lon)) = std::env::var("TERMAP_HOME")
        .ok()
        .as_deref()
        .and_then(parse_pair)
    {
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

/// The address at the other end of the SSH connection.
///
/// `SSH_CONNECTION` is "client-ip client-port server-ip server-port". Private
/// and loopback ranges are dropped: they geolocate to nothing useful, and a
/// lookup of 10.x tells a public API more about the query than it tells us.
fn ssh_client_ip() -> Option<String> {
    if std::env::var_os("TERMAP_NO_CLIENT_LOOKUP").is_some() {
        return None;
    }
    let conn = std::env::var("SSH_CONNECTION").ok()?;
    let ip = conn.split_whitespace().next()?.to_string();
    let private = ip.starts_with("10.")
        || ip.starts_with("127.")
        || ip.starts_with("192.168.")
        || ip.starts_with("169.254.")
        || ip.starts_with("::1")
        || ip.starts_with("fc")
        || ip.starts_with("fd")
        || (ip.starts_with("172.")
            && ip
                .split('.')
                .nth(1)
                .and_then(|o| o.parse::<u8>().ok())
                .is_some_and(|o| (16..32).contains(&o)));
    (!private).then_some(ip)
}

/// Where an address is, as the geolocation service describes it.
///
/// Public because two callers want it now and neither should own a second copy
/// of the HTTP call: the map puts a marker on it, and the portfolio's visit log
/// records where somebody connected from. The map wants a point; the log wants
/// the words.
#[derive(Debug, Clone, Default)]
pub struct Where {
    pub city: String,
    pub region: String,
    pub country: String,
    pub lat: f64,
    pub lon: f64,
}

impl Where {
    /// "Kapadwanj, Gujarat, India", skipping whatever came back empty.
    pub fn label(&self) -> String {
        [
            self.city.as_str(),
            self.region.as_str(),
            self.country.as_str(),
        ]
        .iter()
        .filter(|s| !s.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join(", ")
    }
}

/// Locate one address. An empty string asks the service about this machine.
///
/// Plain HTTP over a raw socket, deliberately, for the reason in the module
/// header: the alternative is a TLS stack for a lookup of an address the far
/// end already knows.
pub fn locate(ip: &str) -> Option<Where> {
    const HOST: &str = "ip-api.com";
    let req = format!(
        "GET /json/{ip}?fields=status,city,regionName,country,lat,lon HTTP/1.1\r\n\
         Host: {HOST}\r\nUser-Agent: termap/0.1\r\nConnection: close\r\n\r\n"
    );

    let mut stream = TcpStream::connect((HOST, 80)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(6))).ok()?;
    stream
        .set_write_timeout(Some(Duration::from_secs(6)))
        .ok()?;
    stream.write_all(req.as_bytes()).ok()?;

    let mut buf = String::new();
    stream.take(64 * 1024).read_to_string(&mut buf).ok()?;
    let body = buf.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or(&buf);

    if field(body, "status")? != "success" {
        return None;
    }
    Some(Where {
        city: field(body, "city").unwrap_or_default().to_string(),
        region: field(body, "regionName").unwrap_or_default().to_string(),
        country: field(body, "country").unwrap_or_default().to_string(),
        lat: field(body, "lat")?.parse().ok()?,
        lon: field(body, "lon")?.parse().ok()?,
    })
}

fn lookup_by_ip() -> Option<Fix> {
    // Ask about the visitor if there is one, otherwise about ourselves.
    let who = ssh_client_ip().unwrap_or_default();
    let found = locate(&who)?;
    let (lat, lon) = (found.lat, found.lon);
    // The marker names the place, not the country: at street zoom "India" under
    // a dot is noise, and the country is in the log where it is useful.
    let label = [found.city.as_str(), found.region.as_str()]
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
    let source = if ssh_client_ip().is_some() {
        "your address"
    } else {
        "IP"
    };
    Some(fix_from(lat, lon, label, 10.0, source))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// One test, not two, because `ssh_client_ip` reads process-global
    /// environment variables and Rust runs tests on parallel threads by
    /// default -- two tests each setting `SSH_CONNECTION` race each other and
    /// fail intermittently depending on scheduling. Keeping every case that
    /// touches these variables in one function is what makes it deterministic.
    #[test]
    fn client_lookup_reads_the_environment_it_is_given() {
        // The private ranges have to be excluded, and 172.16/12 is the one
        // that is easy to get wrong: 172.15 and 172.32 are public, everything
        // between is not.
        let cases = [
            ("203.0.113.7 51234 10.0.0.2 22", true),
            ("10.1.2.3 51234 10.0.0.2 22", false),
            ("192.168.1.9 5 1.2.3.4 22", false),
            ("127.0.0.1 5 1.2.3.4 22", false),
            ("172.16.0.4 5 1.2.3.4 22", false),
            ("172.31.255.1 5 1.2.3.4 22", false),
            ("172.15.0.1 5 1.2.3.4 22", true),
            ("172.32.0.1 5 1.2.3.4 22", true),
            ("::1 5 1.2.3.4 22", false),
        ];
        for (conn, want) in cases {
            std::env::set_var("SSH_CONNECTION", conn);
            assert_eq!(ssh_client_ip().is_some(), want, "{conn}");
        }

        // And the off switch, on a connection that would otherwise pass.
        std::env::set_var("SSH_CONNECTION", "203.0.113.7 1 2 22");
        std::env::set_var("TERMAP_NO_CLIENT_LOOKUP", "1");
        assert!(ssh_client_ip().is_none());

        std::env::remove_var("TERMAP_NO_CLIENT_LOOKUP");
        std::env::remove_var("SSH_CONNECTION");
    }
}
