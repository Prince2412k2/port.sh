//! Our own tools, offered to whatever agent is answering.
//!
//! The agent decides when a map belongs on screen. It cannot be told to by this
//! side -- ACP has no way for a client to hand an agent a tool -- so the one
//! route the protocol offers is an MCP server named in `session/new`, and this
//! is that server.
//!
//! **It runs in this process.** ACP v1 has three MCP transports and one of them
//! is `http` with a URL, which an agent advertises support for at `initialize`
//! as `mcpCapabilities.http`. So instead of spawning a second copy of this
//! binary -- which would mean a second gazetteer, a second archive handle, and
//! the coordinates coming back to the session by observation rather than by
//! return value -- we bind a loopback listener and answer the call on the spot.
//! The session that owns the panel is the one that handles the tool call.
//!
//! Two tools, and they are deliberately two:
//!
//! - `locate_place` turns a name into a point. The agent is good at knowing
//!   that a question is about Jaipur and bad at knowing where Jaipur is to four
//!   decimal places, and a hallucinated coordinate lands the camera in the sea
//!   with no way to tell that is what happened.
//! - `show_map` puts a point on screen. Separate from the lookup because
//!   knowing where something is and deciding to draw it are different
//!   decisions, and most answers want the first without the second.
//!
//! Nothing here is a general-purpose endpoint. The listener is bound to
//! loopback, the path carries a per-session token, and a call with an unknown
//! token is answered with an error and dropped -- a tool call is an instruction
//! to draw on somebody's screen, and which screen is not negotiable.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::mpsc::Sender;
use std::sync::{Mutex, OnceLock};

use crate::json::{self, Value};

/// What this server calls itself.
///
/// Shared with `acp.rs`, which puts it in `session/new`, and with `gates.rs`,
/// which has to predict what an agent will rename our tools to: Copilot
/// namespaces MCP tools as `<server>-<tool>`, so this string is a prefix on the
/// allow-list as well as a label in the handshake. One constant, so the two
/// cannot disagree.
pub const SERVER_NAME: &str = "portfolio";

/// What a tool call asks the page to do.
///
/// The whole vocabulary between the agent and the screen. Kept as a type rather
/// than as a panel so that `ask.rs` decides what a directive *looks like* and
/// this file only carries what was asked for.
#[derive(Debug, Clone, PartialEq)]
pub enum Directive {
    /// Put one or more points on the map. More than one is a route the visitor
    /// can walk with ctrl-n and ctrl-b; the camera flies between them.
    Map { stops: Vec<Stop> },
    /// Take whatever is showing away.
    Clear,
    /// A tool was called, and this is what it was asked for.
    ///
    /// The page cannot learn this from the ACP stream: ACP's `ToolCall` has no
    /// name, Copilot sends an empty title, and the transcript showed a row
    /// reading `\u{2713} tool` with nothing on it. But *this* file knows exactly
    /// which tool was called and with what -- so it says so, and the row can
    /// read `locate_place  Taj Mahal` instead.
    Called { tool: String, detail: String },
}

/// One session, from the tool server's side: somewhere to draw, and where the
/// visitor turned out to be.
struct Board {
    to: Sender<Directive>,
    /// Shared with the lookup thread, not a copy of it -- the geolocation
    /// finishes after the visitor arrives, and a tool called ten seconds in
    /// should see the answer.
    place: Option<std::sync::Arc<Mutex<Option<termap::home::Where>>>>,
}

/// Somewhere on the map, as the agent described it.
#[derive(Debug, Clone, PartialEq)]
pub struct Stop {
    pub lat: f64,
    pub lon: f64,
    pub zoom: f64,
    /// Where the camera should set out from, as (lat, lon, zoom).
    ///
    /// Without it a stop is somewhere to be and the camera gets there from
    /// wherever it was. With it the stop is the *end of a journey*, and the
    /// flight is the answer: how far Kapadwanj is from Ahmedabad, which way
    /// somebody moved, the first leg of a route. The path between two points is
    /// a thing you can watch, and a still of the destination is not.
    pub from: Option<(f64, f64, f64)>,
    pub label: String,
    /// A sentence about the place, shown under its name. This is what makes the
    /// map worth having: a pin says where, and the line under it says why the
    /// answer mentioned it at all.
    pub note: String,
}

/// Sessions that can be drawn on, by token.
fn boards() -> &'static Mutex<HashMap<String, Board>> {
    static B: OnceLock<Mutex<HashMap<String, Board>>> = OnceLock::new();
    B.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Let a session be drawn on, and hand back the token that addresses it.
///
/// The token is the session's whole authority: it is in the URL the agent is
/// given, and it is the only thing that says which of a hundred concurrent
/// pages a `show_map` refers to.
pub fn register(
    tx: Sender<Directive>,
    place: Option<std::sync::Arc<Mutex<Option<termap::home::Where>>>>,
) -> String {
    let token = token();
    boards()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(token.clone(), Board { to: tx, place });
    token
}

/// An unguessable token.
///
/// Deliberately not `visits::next_id`, which is a counter and a timestamp -- it
/// is a key in a log file and being unique is all it has to be. This is a
/// capability: whoever holds it can draw on a stranger's screen. The listener is
/// on loopback, so guessing one means already being on this box, but "already on
/// the box" is not the same as "allowed to interrupt somebody's session", and
/// the fix costs sixteen bytes from the kernel.
///
/// There is no `rand` in the offline registry. `/dev/urandom` needs no crate and
/// is the thing `rand` would read anyway. If it cannot be read the token falls
/// back to being merely unique, and says so in the log rather than pretending.
fn token() -> String {
    use std::io::Read;
    let mut buf = [0u8; 16];
    match std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut buf)) {
        Ok(()) => buf.iter().map(|b| format!("{b:02x}")).collect(),
        Err(e) => {
            eprintln!("portfolio: no /dev/urandom ({e}), tool tokens are only unique");
            static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let t = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            format!("{t:x}-{n:x}")
        }
    }
}

/// Stop accepting directives for a session. Called when it ends; without it the
/// table grows by one entry for every visitor for the life of the process.
pub fn forget(token: &str) {
    boards().lock().unwrap_or_else(|e| e.into_inner()).remove(token);
}

/// The place index, built once and shared.
///
/// On its own thread because the sweep reads several hundred tiles off a 1.7 GB
/// archive -- about half a second -- and no visitor should wait for it. A
/// lookup that arrives before it is ready answers "not found", which is the
/// same answer it gives for a name the archive does not have and needs no
/// special case on the page.
static INDEX: OnceLock<termap::gazetteer::Gazetteer> = OnceLock::new();

/// What the basemap actually covers, in world coordinates.
///
/// Needed because the geocoder is worldwide and the archive is not. Asked for
/// the Eiffel Tower it now returns the right coordinates -- and there are no
/// tiles within a thousand miles of them, so drawing that point is a blank
/// rectangle presented as a map of Paris. The tool says which it is and lets
/// the agent tell the visitor.
static COVERS: OnceLock<[f64; 4]> = OnceLock::new();

/// Whether there is a map to draw at this point.
fn on_this_map(lat: f64, lon: f64) -> bool {
    let Some([x0, y0, x1, y1]) = COVERS.get() else { return false };
    let [x, y] = termap::geo::lonlat_to_world(lon, lat);
    x >= *x0 && x <= *x1 && y >= *y0 && y <= *y1
}

pub fn warm_index() {
    if INDEX.get().is_some() {
        return;
    }
    std::thread::spawn(|| {
        let mut src = termap::tiles::Source::open(None);
        if !src.has_basemap() {
            eprintln!("portfolio: no basemap, place lookup is off");
            let _ = INDEX.set(termap::gazetteer::Gazetteer::default());
            return;
        }
        let _ = COVERS.set(src.bounds());
        let started = std::time::Instant::now();
        let g = termap::gazetteer::Gazetteer::build(&mut src);
        eprintln!(
            "portfolio: place index ready -- {} names from z{:?} in {:?}",
            g.len(),
            g.swept,
            started.elapsed()
        );
        let _ = INDEX.set(g);
    });
}

fn index() -> Option<&'static termap::gazetteer::Gazetteer> {
    INDEX.get().filter(|g| !g.is_empty())
}

thread_local! {
    /// A map source belonging to the tool server's own thread, and the deep
    /// indexes built from it.
    ///
    /// The country-wide index cannot hold cafes: the landmark layer only starts
    /// around z13, and z13 over India is hundreds of thousands of tiles. A box
    /// around one city at z13 and z14 is a few dozen, so it is built when
    /// somebody asks about that city and kept for the next question about it.
    ///
    /// Thread-local because `Source` holds `Rc`s and is not `Send`, and the tool
    /// server runs every request on one thread -- a current-thread runtime, on
    /// purpose. The cost is one extra copy of the terrain grid, 28 MB, once for
    /// the process rather than once per session.
    static NEARBY: std::cell::RefCell<Nearby> = const { std::cell::RefCell::new(Nearby::new()) };
}

#[derive(Default)]
struct Nearby {
    src: Option<termap::tiles::Source>,
    /// Keyed by the city's coordinates to a hundredth of a degree, which is
    /// about a kilometre -- close enough that two questions about the same city
    /// share one sweep.
    seen: Vec<((i32, i32), termap::gazetteer::Gazetteer)>,
}

impl Nearby {
    const fn new() -> Nearby {
        Nearby { src: None, seen: Vec::new() }
    }
}

/// Look `what` up inside the city at `centre`.
fn look_nearby(centre: (f64, f64), what: &str) -> Option<termap::gazetteer::Entry> {
    NEARBY.with(|n| {
        let mut n = n.borrow_mut();
        let key = ((centre.0 * 100.0) as i32, (centre.1 * 100.0) as i32);
        if !n.seen.iter().any(|(k, _)| *k == key) {
            if n.src.is_none() {
                let src = termap::tiles::Source::open(None);
                if !src.has_basemap() {
                    return None;
                }
                n.src = Some(src);
            }
            let src = n.src.as_mut()?;
            let started = std::time::Instant::now();
            // A tenth of a degree is about eleven kilometres, which covers a
            // city and not its neighbours.
            let g = termap::gazetteer::Gazetteer::around(src, centre, 0.10);
            eprintln!(
                "portfolio: indexed {} landmarks around {:.3},{:.3} in {:?}",
                g.len(),
                centre.0,
                centre.1,
                started.elapsed()
            );
            // Two cities is plenty to keep; a conversation does not wander far.
            if n.seen.len() >= 3 {
                n.seen.remove(0);
            }
            n.seen.push((key, g));
        }
        n.seen
            .iter()
            .find(|(k, _)| *k == key)
            .and_then(|(_, g)| g.find(what))
            .cloned()
    })
}

/// Who we say we are to OpenStreetMap's geocoder.
///
/// Nominatim's usage policy requires a User-Agent that identifies the
/// application and gives a way to reach whoever runs it. That is not a
/// formality: it is how they tell a portfolio from a scraper, and the penalty
/// for anonymous traffic is a block on the address. Overridable so a fork does
/// not pretend to be this deployment.
fn agent_line() -> String {
    std::env::var("PORTFOLIO_GEOCODE_UA").unwrap_or_else(|_| {
        "terminal-portfolio/0.1 (+https://github.com/Prince2412k2; prince240102@gmail.com)"
            .to_string()
    })
}

/// One geocoded answer.
struct Placed {
    lat: f64,
    lon: f64,
    name: String,
    zoom: f64,
}

/// A zoom that frames what came back, from its bounding box.
///
/// Nominatim gives a box rather than a zoom, and the box is the useful part: a
/// country and a cafe both arrive as one point and only their extent says which
/// is which. Banded rather than a formula because the bands are legible and a
/// log of a ratio is not.
fn zoom_for(span: f64) -> f64 {
    match span {
        s if s > 6.0 => 5.0,
        s if s > 2.0 => 7.0,
        s if s > 0.5 => 9.0,
        s if s > 0.1 => 11.0,
        s if s > 0.02 => 13.0,
        _ => 14.0,
    }
}

thread_local! {
    /// Answers already asked for, and when the last request went out.
    ///
    /// Both halves matter. The cache is because a conversation asks about the
    /// same handful of places repeatedly, and a miss is worth remembering too --
    /// a name the geocoder does not know will not learn it while somebody is
    /// still typing. The clock is Nominatim's usage policy: at most one request a
    /// second, and exceeding it gets the address blocked rather than
    /// rate-limited.
    static GEOCODED: std::cell::RefCell<Asked> = const { std::cell::RefCell::new(Asked::new()) };
}

/// What has been asked for, and when the last request went out.
struct Asked {
    seen: Vec<(String, Option<Placed>)>,
    last: Option<std::time::Instant>,
}

impl Asked {
    const fn new() -> Asked {
        Asked { seen: Vec::new(), last: None }
    }
}

/// Ask OpenStreetMap where something is.
///
/// The last resort behind `locate_place`: the basemap knows India's settlements
/// and, swept deeply, a city's own landmarks -- this knows the rest of the world
/// and everything with a name on it. It is also the only part of this box that
/// makes an outbound TLS connection, and the reason `ureq` is a dependency.
///
/// Off entirely with `PORTFOLIO_NO_GEOCODE`, and silent on any failure: a
/// portfolio whose chat stops working because somebody else's service is down
/// is a worse thing than one that cannot place a cafe.
fn geocode(query: &str) -> Option<Placed> {
    let query = query.trim();
    if query.len() < 3 || std::env::var_os("PORTFOLIO_NO_GEOCODE").is_some() {
        return None;
    }

    let cached = GEOCODED.with(|g| {
        let g = g.borrow();
        g.seen.iter().find(|(q, _)| q == query).map(|(_, found)| {
            found.as_ref().map(|p| Placed { lat: p.lat, lon: p.lon, name: p.name.clone(), zoom: p.zoom })
        })
    });
    if let Some(hit) = cached {
        return hit;
    }

    // One request a second, as their policy asks. This blocks the tool server's
    // thread, which is acceptable for something that runs when a place is asked
    // about for the first time and never in a loop.
    GEOCODED.with(|g| {
        let last = g.borrow().last;
        if let Some(last) = last {
            let since = last.elapsed();
            if since < std::time::Duration::from_secs(1) {
                std::thread::sleep(std::time::Duration::from_secs(1) - since);
            }
        }
        g.borrow_mut().last = Some(std::time::Instant::now());
    });

    let found = ask_nominatim(query);
    match &found {
        Some(p) => eprintln!("portfolio: geocoded `{query}` to {:.4},{:.4}", p.lat, p.lon),
        None => eprintln!("portfolio: no geocode for `{query}`"),
    }
    GEOCODED.with(|g| {
        let mut g = g.borrow_mut();
        // Bounded: a long conversation should not turn this into a leak.
        if g.seen.len() >= 64 {
            g.seen.remove(0);
        }
        let keep = found.as_ref().map(|p| Placed {
            lat: p.lat,
            lon: p.lon,
            name: p.name.clone(),
            zoom: p.zoom,
        });
        g.seen.push((query.to_string(), keep));
    });
    found
}

fn ask_nominatim(query: &str) -> Option<Placed> {
    let url = format!(
        "https://nominatim.openstreetmap.org/search?format=jsonv2&limit=1&q={}",
        urlencode(query)
    );
    let body = ureq::get(&url)
        .header("User-Agent", &agent_line())
        .config()
        .timeout_global(Some(std::time::Duration::from_secs(8)))
        .build()
        .call()
        .ok()?
        .body_mut()
        .read_to_string()
        .ok()?;
    parse_nominatim(&body)
}

/// The first result, or nothing. Split out so it can be tested against a
/// recorded response -- this machine cannot reach the service.
fn parse_nominatim(body: &str) -> Option<Placed> {
    let first = json::parse(body)?.as_array()?.first()?.clone();
    let num = |k: &str| {
        first.get(k).and_then(|v| match v {
            // Nominatim sends lat and lon as strings, and the box as strings too.
            Value::Str(s) => s.parse::<f64>().ok(),
            other => other.as_f64(),
        })
    };
    let (lat, lon) = (num("lat")?, num("lon")?);
    if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
        return None;
    }
    // `boundingbox` is [south, north, west, east], as strings.
    let span = first
        .get("boundingbox")
        .and_then(|b| b.as_array())
        .and_then(|b| {
            let f = |i: usize| b.get(i).and_then(|v| v.as_str()).and_then(|s| s.parse::<f64>().ok());
            Some(((f(1)? - f(0)?).abs()).max((f(3)? - f(2)?).abs()))
        })
        .unwrap_or(0.0);
    Some(Placed {
        lat,
        lon,
        name: first
            .get("display_name")
            .or_else(|| first.get("name"))
            .and_then(|n| n.as_str())
            .unwrap_or("")
            .to_string(),
        zoom: zoom_for(span),
    })
}

/// Percent-encode a query. Only what a place name can contain, which is why
/// this is nine lines rather than a dependency.
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The tools, as MCP describes them.
///
/// The descriptions are the only instructions the agent gets about *when* to
/// reach for these, so they say when not to as well. An agent that shows a map
/// for every answer is worse than one that never does: the picture stops
/// meaning anything and the page starts flashing.
fn tool_list() -> String {
    let map_when = "Put a place on the map that sits beside your answer on the \
        visitor's screen.\n\n\
        Call it whenever your answer names a real geographic place -- not when \
        you are asked to, when you name one. The visitor can see the screen; \
        this is how you point at something. Where somewhere is, where he \
        studied or worked, the towns in a region, where the visitor is.\n\n\
        Give every place a `note`: one sentence on why it is in the answer, in \
        your own words, the thing you would have said aloud. \"Where he learned \
        Linux and the shell\". \"The ghats, and the reason people come\". A pin \
        says where a place is; the note says why you brought it up, and a stop \
        without one is a dot on a map and worth much less.\n\n\
        Several places go in one call as a `places` list and become a route the \
        visitor steps through with ctrl-n and ctrl-b while the camera flies. One \
        call with the route in it, never several that arrive as unrelated pins.\n\n\
        Think of it as the map you would point at while telling somebody about a \
        place, not as a control panel. A place worth naming is usually worth \
        pinning -- and still not everything: code, a project, a skill, an \
        opinion, a greeting, or a place mentioned in passing gets none. A \
        picture that appears for everything stops meaning anything. It clears \
        itself when an answer does not ask for one, so there is nothing to tidy \
        up.";
    format!(
        r#"{{"tools":[
        {{"name":"locate_place",
          "description":"Look up where anything is by name: a country, a state, a city, a town, a village, a monument, a lake, a mall, a cafe. Returns latitude, longitude, a zoom that frames it, `source` -- the map data on this box, or OpenStreetMap -- and `on_this_map`. That last one matters: the lookup covers the world and the map on screen only covers India, so `on_this_map:false` means you know where the place is and cannot show it. Say where it is in words and do not call show_map, because an empty frame presented as a map of Paris is worse than no picture. Pass the zoom back to show_map unless the answer is about something smaller than what you looked up.\n\nAlways look a place up rather than recalling its coordinates. A wrong one puts the camera in the sea and nothing on screen says so.\n\nIt tries three things in order and you need not care which answers: an index of India built into this box, then that city's own streets, then OpenStreetMap. `found:false` means all three came up empty, and then say you cannot place it rather than guessing a point.",
          "inputSchema":{{"type":"object","properties":{{
            "name":{{"type":"string","description":"The place name, e.g. Jaipur, Kerala, Ahmedabad."}},
            "near":{{"type":"string","description":"The town or city to look inside, when `name` is something within one -- a cafe, a mall, a park, a hospital, a temple. Give it whenever you have it: it is what tells a Zen Cafe in Ahmedabad from the several elsewhere, and it lets this box search that city's own streets before going out to the internet."}}
          }},"required":["name"]}}}},
        {{"name":"show_map",
          "description":{},
          "inputSchema":{{"type":"object","properties":{{
            "lat":{{"type":"number","description":"Latitude, from locate_place. Use this and lon for a single place."}},
            "lon":{{"type":"number","description":"Longitude, from locate_place."}},
            "zoom":{{"type":"number","description":"How close to look, 3 to 16.5. Yours to choose, and worth choosing: 5 a region, 7 a state, 10 a city and its surroundings, 12 a neighbourhood, 14 a few streets, 16 a single corner. `locate_place` suggests one that frames what it found -- pass it back for a place somebody named, and pick your own when the answer is about something smaller than the thing you looked up. A cafe shown at city zoom is a dot in a smudge."}},
            "label":{{"type":"string","description":"The place's name, written under the map."}},
            "note":{{"type":"string","description":"One sentence on why this place matters to the answer. Shown under the name. This is what makes the map worth looking at -- a pin says where, this says why you mentioned it."}},
            "from":{{"type":"object","description":"Where to fly *from*, as {{lat, lon}} and optionally zoom. Use it when the journey is the answer rather than the destination: how far one place is from another, which way somebody moved, the first leg of a route. The camera travels the path between the two and the visitor watches it go; leave it out and the map simply arrives at the place. Do not use it to show a single place -- an unnecessary journey is a long wait for a pin.","properties":{{
              "lat":{{"type":"number"}},"lon":{{"type":"number"}},"zoom":{{"type":"number"}}
            }},"required":["lat","lon"]}},
            "places":{{"type":"array","description":"Several places at once, each {{lat, lon, label, note, zoom}}. Use this instead of lat/lon when the answer walks through more than one -- the visitor can step between them, and they stay together as one route rather than arriving as unrelated calls.","items":{{"type":"object","properties":{{
              "lat":{{"type":"number"}},"lon":{{"type":"number"}},
              "zoom":{{"type":"number"}},
              "label":{{"type":"string"}},"note":{{"type":"string"}}
            }},"required":["lat","lon"]}}}}
          }}}}}},
        {{"name":"locate_visitor",
          "description":"Where the person you are talking to appears to be connecting from, as an address lookup sees it. Use it when they ask where they are, or where you think they are. It is a guess with a city's worth of precision -- say so. Returns found:false when the lookup has not come back or the address is private, and then you should say you cannot tell rather than guessing.",
          "inputSchema":{{"type":"object","properties":{{}}}}}},
        {{"name":"hide_map",
          "description":"Take the map off the screen. Only needed when a map is showing and the answer has moved on to something that is not a place; otherwise it goes by itself.",
          "inputSchema":{{"type":"object","properties":{{}}}}}}
    ]}}"#,
        json::quote(map_when)
    )
}

/// Answer one tool call. `token` says whose screen it is.
///
/// Logged, every one. It is the only positive evidence that the agent can see
/// these at all: the handshake succeeding proves the *server* is reachable, and
/// that is not the same claim -- Copilot connected here perfectly while the
/// model could not see a single tool, because the allow-list used the wrong
/// spelling of their names. An empty log next to a working handshake is the
/// shape of that bug.
fn call(token: &str, name: &str, args: Option<&Value>) -> String {
    eprintln!("portfolio: agent called `{name}`");
    send(token, Directive::Called { tool: name.to_string(), detail: detail_of(name, args) });
    let arg_str = |k: &str| args.and_then(|a| a.get(k)).and_then(|v| v.as_str()).unwrap_or("");

    match name {
        "locate_place" => {
            let want = arg_str("name");
            // Somewhere inside a city: the country-wide index does not carry
            // cafes, so find the city first and then sweep it deeply.
            //
            // Nearby *before* the country-wide index, not after. Asked for
            // "Kankaria" near Ahmedabad, the global index has a Kankaria and it
            // is four hundred kilometres away in Madhya Pradesh -- a correct
            // answer to a question nobody asked. A caller that names a city has
            // told us which of the identically named places it means.
            let near = arg_str("near");
            // Three tiers, cheapest first, and the order is the whole design.
            // The country-wide index is in memory. The deep sweep of one city is
            // fifty milliseconds of local disk. OpenStreetMap is somebody else's
            // server across the internet, so it goes last and only when the two
            // that cost nothing have both come up empty.
            let local = match near.trim().is_empty() {
                false => index()
                    .and_then(|g| g.find(near))
                    .and_then(|city| look_nearby(city.lonlat, want))
                    .or_else(|| index().and_then(|g| g.find(want)).cloned()),
                true => index().and_then(|g| g.find(want)).cloned(),
            };
            match local {
                Some(e) => text(&format!(
                    r#"{{"found":true,"name":{},"kind":{},"lat":{:.5},"lon":{:.5},"zoom":{:.2},"source":"map data"}}"#,
                    json::quote(&e.name),
                    json::quote(e.what),
                    e.lonlat.1,
                    e.lonlat.0,
                    e.zoom
                )),
                None => match geocode(&match near.trim().is_empty() {
                    // The city goes into the query rather than being a separate
                    // field: "Zen Cafe" alone finds a Zen Cafe, and there are a
                    // great many of them.
                    true => want.to_string(),
                    false => format!("{want}, {near}"),
                }) {
                    Some(p) => text(&format!(
                        r#"{{"found":true,"name":{},"kind":"geocoded","lat":{:.5},"lon":{:.5},"zoom":{:.2},"source":"OpenStreetMap","on_this_map":{}}}"#,
                        json::quote(&p.name),
                        p.lat,
                        p.lon,
                        p.zoom,
                        on_this_map(p.lat, p.lon)
                    )),
                    // Deliberately not the nearest thing. Answering a miss with
                    // a town forty kilometres away would look exactly like an
                    // answer.
                    None => text(&format!(
                        r#"{{"found":false,"name":{},"why":"not in the map data and not in OpenStreetMap either"}}"#,
                        json::quote(want)
                    )),
                },
            }
        }
        "show_map" => {
            // Either one point at the top level, or a list of them. Both,
            // because an agent that has a single place to show should not have
            // to build an array to say so, and one that is walking somebody
            // through five should not have to call five times and lose the
            // fact that they belong together.
            let listed = args.and_then(|a| a.get("places")).and_then(|p| p.as_array());
            let raw: Vec<&Value> = match listed {
                Some(list) => list.iter().collect(),
                None => args.into_iter().collect(),
            };
            let mut stops = Vec::new();
            for one in raw {
                let num = |k: &str| one.get(k).and_then(|v| v.as_f64());
                let text_of =
                    |k: &str| one.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
                let (Some(lat), Some(lon)) = (num("lat"), num("lon")) else { continue };
                if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
                    continue;
                }
                // `from` is optional and its zoom more so: the flight's own
                // derivation decides how high to climb between two points, so a
                // caller that has not thought about it gets the right arc by
                // saying nothing.
                let start = one.get("from").and_then(|f| {
                    let n = |k: &str| f.get(k).and_then(|v| v.as_f64());
                    let (lat, lon) = (n("lat")?, n("lon")?);
                    ((-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon)).then(|| {
                        (lat, lon, n("zoom").unwrap_or(num("zoom").unwrap_or(11.5)).clamp(3.0, 16.5))
                    })
                });
                stops.push(Stop {
                    lat,
                    lon,
                    from: start,
                    // The agent's to choose, within what the archive can draw.
                    // It was clamped to a narrow band because the panel was 46
                    // columns and street zoom in that space was four roads and
                    // a bus stop labelled twice -- the map is the whole page
                    // now, and "a cafe in Ahmedabad" is a question about a
                    // street corner that a city view cannot answer.
                    zoom: num("zoom").unwrap_or(11.5).clamp(3.0, 16.5),
                    label: text_of("label"),
                    note: text_of("note"),
                });
            }
            if stops.is_empty() {
                return err("show_map needs lat and lon, or a places list of them");
            }
            match send(token, Directive::Map { stops }) {
                true => text(r#"{"shown":true}"#),
                false => err("that screen is gone"),
            }
        }
        "locate_visitor" => {
            let found = boards()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(token)
                .and_then(|b| b.place.clone())
                .and_then(|slot| slot.lock().unwrap_or_else(|e| e.into_inner()).clone());
            match found {
                Some(w) => text(&format!(
                    r#"{{"found":true,"where":{},"lat":{:.4},"lon":{:.4},"accuracy":"city"}}"#,
                    json::quote(&w.label()),
                    w.lat,
                    w.lon
                )),
                None => text(
                    r#"{"found":false,"why":"the lookup has not come back, or the address is private"}"#,
                ),
            }
        }
        "hide_map" => match send(token, Directive::Clear) {
            true => text(r#"{"hidden":true}"#),
            false => err("that screen is gone"),
        },
        _ => err("no such tool"),
    }
}

/// The interesting half of a call, for the row on screen.
///
/// Not the whole argument object: a row is one line and `{"lat":23.0386,...}` is
/// not what anybody is trying to read. The name being looked up, or the point
/// being flown to.
fn detail_of(name: &str, args: Option<&Value>) -> String {
    let str_of = |k: &str| args.and_then(|a| a.get(k)).and_then(|v| v.as_str()).unwrap_or("");
    let num_of = |k: &str| args.and_then(|a| a.get(k)).and_then(|v| v.as_f64());
    match name {
        "locate_place" => str_of("name").to_string(),
        "show_map" if args.and_then(|a| a.get("from")).is_some() => {
            let label = str_of("label");
            match label.is_empty() {
                true => "a journey".to_string(),
                false => format!("to {label}"),
            }
        }
        "show_map" => {
            if let Some(list) = args.and_then(|a| a.get("places")).and_then(|p| p.as_array()) {
                let names: Vec<&str> = list
                    .iter()
                    .filter_map(|p| p.get("label").and_then(|l| l.as_str()))
                    .collect();
                return match names.is_empty() {
                    true => format!("{} places", list.len()),
                    false => names.join(", "),
                };
            }
            let label = str_of("label");
            match (num_of("lat"), num_of("lon")) {
                (Some(lat), Some(lon)) if label.is_empty() => format!("{lat:.3}, {lon:.3}"),
                (Some(lat), Some(lon)) => format!("{label}  {lat:.3}, {lon:.3}"),
                _ => label.to_string(),
            }
        }
        _ => String::new(),
    }
}

fn send(token: &str, d: Directive) -> bool {
    let table = boards().lock().unwrap_or_else(|e| e.into_inner());
    table.get(token).is_some_and(|b| b.to.send(d).is_ok())
}

/// An MCP tool result: content blocks, and a flag for the error case.
fn text(body: &str) -> String {
    format!(r#"{{"content":[{{"type":"text","text":{}}}]}}"#, json::quote(body))
}

fn err(why: &str) -> String {
    format!(
        r#"{{"isError":true,"content":[{{"type":"text","text":{}}}]}}"#,
        json::quote(why)
    )
}

/// One JSON-RPC frame in, one out. `None` for a notification, which gets no
/// reply at all -- answering one is a protocol error, not a harmless extra.
pub fn handle(token: &str, body: &str) -> Option<String> {
    let v = json::parse(body)?;
    let method = v.get("method").and_then(|m| m.as_str()).unwrap_or("");
    // No id is a notification -- `notifications/initialized` and friends -- and
    // a notification gets no reply at all. Answering one is a protocol error,
    // not a harmless extra.
    let id = match v.get("id")? {
        Value::Num(n) => format!("{n}"),
        Value::Str(s) => json::quote(s),
        _ => "null".to_string(),
    };
    let params = v.get("params");

    let result = match method {
        // The version is echoed rather than chosen: an agent that speaks a
        // later revision is told what it asked for, and the two tools here have
        // been the same shape in every revision that has one.
        "initialize" => {
            let want = params
                .and_then(|p| p.get("protocolVersion"))
                .and_then(|p| p.as_str())
                .unwrap_or("2025-06-18");
            format!(
                r#"{{"protocolVersion":{},"capabilities":{{"tools":{{}}}},"serverInfo":{{"name":"portfolio","version":"0.1.0"}}}}"#,
                json::quote(want)
            )
        }
        "tools/list" => tool_list(),
        "tools/call" => {
            let name = params.and_then(|p| p.get("name")).and_then(|n| n.as_str()).unwrap_or("");
            call(token, name, params.and_then(|p| p.get("arguments")))
        }
        "ping" => "{}".to_string(),
        _ => {
            return Some(format!(
                r#"{{"jsonrpc":"2.0","id":{id},"error":{{"code":-32601,"message":"unknown method"}}}}"#
            ))
        }
    };
    Some(format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{result}}}"#))
}

/// Where the tool server is listening, once it is.
static WHERE: OnceLock<SocketAddr> = OnceLock::new();

/// Start the loopback listener, once per process.
///
/// Loopback only, and not on the web terminal's port. The web port may be
/// published to the internet and this is not something to publish: it draws on
/// visitors' screens. An SSH-only deployment has no web server at all, so this
/// cannot ride on that one anyway.
pub fn serve() {
    if WHERE.get().is_some() {
        return;
    }
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
            Ok(rt) => rt,
            Err(e) => {
                eprintln!("portfolio: no runtime for the tool server: {e}");
                let _ = tx.send(None);
                return;
            }
        };
        rt.block_on(async move {
            let listener = match tokio::net::TcpListener::bind("127.0.0.1:0").await {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("portfolio: tool server could not bind: {e}");
                    let _ = tx.send(None);
                    return;
                }
            };
            let addr = listener.local_addr().ok();
            let _ = tx.send(addr);
            let app = axum::Router::new().route(
                "/mcp/{token}",
                axum::routing::post(
                    |axum::extract::Path(token): axum::extract::Path<String>, body: String| async move {
                        match handle(&token, &body) {
                            Some(out) => (
                                [(axum::http::header::CONTENT_TYPE, "application/json")],
                                out,
                            )
                                .into_response(),
                            // A notification is answered with 202 and no body,
                            // which is what Streamable HTTP asks for.
                            None => axum::http::StatusCode::ACCEPTED.into_response(),
                        }
                    },
                ),
            );
            if let Err(e) = axum::serve(listener, app).await {
                eprintln!("portfolio: tool server stopped: {e}");
            }
        });
    });
    if let Ok(Some(addr)) = rx.recv_timeout(std::time::Duration::from_secs(5)) {
        let _ = WHERE.set(addr);
        eprintln!("portfolio: tool server on http://{addr}");
    }
}

use axum::response::IntoResponse;

/// The URL to hand an agent for one session, if the server is up.
pub fn url_for(token: &str) -> Option<String> {
    WHERE.get().map(|addr| format!("http://{addr}/mcp/{token}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result_of(out: &str) -> Value {
        json::parse(out).expect("not json").get("result").cloned().expect("no result")
    }

    /// A notification gets no answer. Replying to one is a protocol error.
    #[test]
    fn a_notification_is_not_answered() {
        assert!(handle("t", r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).is_none());
        assert!(handle("t", r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#).is_some());
    }

    /// The handshake echoes the version the agent asked for.
    #[test]
    fn initialize_answers_with_tools_and_the_asked_version() {
        let out =
            handle("t", r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}"#)
                .unwrap();
        let r = result_of(&out);
        assert_eq!(r.get("protocolVersion").and_then(|v| v.as_str()), Some("2025-03-26"));
        assert!(r.get("capabilities").and_then(|c| c.get("tools")).is_some());
    }

    /// Both tools are listed, with the schemas an agent needs to call them.
    #[test]
    fn the_tools_are_listed_with_their_arguments() {
        let out = handle("t", r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#).unwrap();
        let names: Vec<String> = result_of(&out)
            .get("tools")
            .and_then(|t| t.as_array())
            .expect("no tools")
            .iter()
            .filter_map(|t| t.get("name").and_then(|n| n.as_str()).map(str::to_string))
            .collect();
        assert!(names.contains(&"locate_place".to_string()), "{names:?}");
        assert!(names.contains(&"show_map".to_string()), "{names:?}");
        assert!(names.contains(&"hide_map".to_string()), "{names:?}");
        assert!(names.contains(&"locate_visitor".to_string()), "{names:?}");
    }

    /// An unknown method is an error, not an empty success. Same rule as the
    /// ACP gates: `{}` tells the caller its request worked.
    #[test]
    fn an_unknown_method_is_an_error() {
        let out = handle("t", r#"{"jsonrpc":"2.0","id":3,"method":"tools/wat"}"#).unwrap();
        let v = json::parse(&out).unwrap();
        assert!(v.get("error").is_some(), "{out}");
        assert!(v.get("result").is_none(), "{out}");
    }

    /// A directive reaches the session that owns the token, and nobody else.
    #[test]
    fn show_map_draws_on_the_session_that_asked_for_it() {
        let (tx, rx) = std::sync::mpsc::channel();
        let token = register(tx, None);
        let out = handle(
            &token,
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"show_map",
               "arguments":{"lat":23.03,"lon":72.51,"zoom":12.5,"label":"Ahmedabad"}}}"#,
        )
        .unwrap();
        assert!(!out.contains("isError"), "{out}");
        // The row first -- what was called and with what -- then the panel.
        match rx.try_recv() {
            Ok(Directive::Called { tool, detail }) => {
                assert_eq!(tool, "show_map");
                assert!(detail.contains("Ahmedabad"), "{detail}");
            }
            other => panic!("expected a row: {other:?}"),
        }
        match rx.try_recv() {
            Ok(Directive::Map { stops }) => {
                assert_eq!(stops.len(), 1);
                let s = &stops[0];
                assert_eq!((s.lat, s.lon, s.zoom), (23.03, 72.51, 12.5));
                assert_eq!(s.label, "Ahmedabad");
            }
            other => panic!("wrong directive: {other:?}"),
        }

        // A stranger's token draws on nothing.
        let out = handle(
            "not-a-session",
            r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"show_map",
               "arguments":{"lat":1.0,"lon":1.0}}}"#,
        )
        .unwrap();
        assert!(out.contains("isError"), "an unknown token was served: {out}");
        assert!(rx.try_recv().is_err(), "it drew on somebody else's screen");
        let _ = &rx;

        forget(&token);
        let out = handle(
            &token,
            r#"{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"show_map",
               "arguments":{"lat":1.0,"lon":1.0}}}"#,
        )
        .unwrap();
        assert!(out.contains("isError"), "a forgotten session still accepts calls");
    }

    /// Coordinates off the globe are refused rather than drawn.
    #[test]
    fn a_coordinate_that_cannot_exist_is_refused() {
        let (tx, rx) = std::sync::mpsc::channel();
        let token = register(tx, None);
        for args in [
            r#"{"lat":91.0,"lon":0.0}"#,
            r#"{"lat":0.0,"lon":-181.0}"#,
            r#"{"lon":72.5}"#,
            r#"{}"#,
        ] {
            let out = handle(
                &token,
                &format!(
                    r#"{{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{{"name":"show_map","arguments":{args}}}}}"#
                ),
            )
            .unwrap();
            assert!(out.contains("isError"), "{args} was accepted: {out}");
        }
        // A row saying it was called is fine and true. What must never arrive is
        // a `Map`: that is the one that moves a camera.
        let moved = rx.try_iter().any(|d| matches!(d, Directive::Map { .. }));
        assert!(!moved, "a bad coordinate reached the screen");
        forget(&token);
    }

    /// The visitor lookup reads the slot, not a copy of it -- the geolocation
    /// finishes after the session starts, and a tool called later must see it.
    #[test]
    fn the_visitor_lookup_sees_an_answer_that_arrives_late() {
        let slot = std::sync::Arc::new(Mutex::new(None));
        let (tx, _rx) = std::sync::mpsc::channel();
        let token = register(tx, Some(slot.clone()));
        let ask = r#"{"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":"locate_visitor","arguments":{}}}"#;

        // Nothing back yet: a miss, not a zero.
        let out = handle(&token, ask).unwrap();
        assert!(out.contains(r#"found\":false"#), "{out}");

        *slot.lock().unwrap() = Some(termap::home::Where {
            city: "Kapadwanj".into(),
            region: "Gujarat".into(),
            country: "India".into(),
            lat: 23.02,
            lon: 73.07,
        });
        let out = handle(&token, ask).unwrap();
        assert!(out.contains("Kapadwanj"), "the late answer never arrived: {out}");
        assert!(out.contains("23.02"), "{out}");
        forget(&token);
    }

    /// The geocoder's answers, parsed from responses it really sent.
    ///
    /// Recorded rather than mocked, and recorded through tmux because the
    /// sandbox blocks the call: these are the exact bodies Nominatim returned
    /// for a monument, a state, a cafe and a nonsense query. The shape has two
    /// traps in it and both are in here -- `lat` and `lon` arrive as *strings*,
    /// and so do the four numbers of the bounding box.
    #[test]
    fn a_recorded_geocode_is_read_correctly() {
        let monument = r#"[{"place_id":248874686,"osm_type":"way","lat":"18.9219661","lon":"72.8345657","category":"building","type":"yes","addresstype":"building","name":"Gateway of India","display_name":"Gateway of India, Apollo Bandar, Mumbai, Maharashtra, 400039, India","boundingbox":["18.9217929","18.9221323","72.8343573","72.8347646"]}]"#;
        let p = parse_nominatim(monument).expect("a monument did not parse");
        assert!((p.lat - 18.9219661).abs() < 1e-7, "{}", p.lat);
        assert!((p.lon - 72.8345657).abs() < 1e-7, "{}", p.lon);
        assert!(p.name.starts_with("Gateway of India"), "{}", p.name);
        // A box a few ten-thousandths of a degree across is a building.
        assert_eq!(p.zoom, 14.0, "a monument was framed at {}", p.zoom);

        let state = r#"[{"place_id":252892919,"osm_type":"relation","lat":"10.3528744","lon":"76.5120396","category":"boundary","type":"administrative","addresstype":"state","name":"Kerala","display_name":"Kerala, India","boundingbox":["8.2935318","12.7960559","74.8640682","77.4123612"]}]"#;
        let p = parse_nominatim(state).expect("a state did not parse");
        assert_eq!(p.name, "Kerala, India");
        // Four and a half degrees tall, so it must not be framed like a cafe.
        assert_eq!(p.zoom, 7.0, "a state was framed at {}", p.zoom);

        // The cross-check worth having: OpenStreetMap and the basemap agree
        // about where this cafe is, to four decimal places.
        let cafe = r#"[{"place_id":248205078,"osm_type":"node","lat":"23.0362229","lon":"72.5494261","category":"amenity","type":"cafe","addresstype":"amenity","name":"Zen Cafe","display_name":"Zen Cafe, 120 Feet Ring Road, Ahmedabad, Gujarat, India","boundingbox":["23.0361729","23.0362729","72.5493761","72.5494761"]}]"#;
        let p = parse_nominatim(cafe).expect("a cafe did not parse");
        assert!((p.lat - 23.03622).abs() < 1e-4 && (p.lon - 72.54942).abs() < 1e-4);
        assert_eq!(p.zoom, 14.0);

        // Nothing found is an empty array, not an error and not a null.
        assert!(parse_nominatim("[]").is_none(), "an empty result invented a place");
        assert!(parse_nominatim("not json at all").is_none());
        // And a point off the globe is refused rather than drawn.
        assert!(parse_nominatim(r#"[{"lat":"91.0","lon":"0.0"}]"#).is_none());
    }

    /// The query is escaped, because place names have spaces and commas in them.
    #[test]
    fn a_place_name_survives_being_put_in_a_url() {
        assert_eq!(urlencode("Gateway of India, Mumbai"), "Gateway+of+India%2C+Mumbai");
        assert_eq!(urlencode("Ward's Lake"), "Ward%27s+Lake");
        // Not ASCII, and not a reason to send a malformed request.
        assert_eq!(urlencode("गेटवे"), "%E0%A4%97%E0%A5%87%E0%A4%9F%E0%A4%B5%E0%A5%87");
    }

    /// The user agent identifies the deployment, which their policy requires.
    #[test]
    fn the_geocoder_says_who_it_is() {
        let ua = agent_line();
        assert!(ua.contains("terminal-portfolio"), "{ua}");
        assert!(ua.contains('@') || ua.contains("http"), "no way to reach anybody: {ua}");
    }

    /// A name the index does not have is a miss, not the nearest thing.
    #[test]
    fn a_place_that_is_not_there_is_not_invented() {
        let out = handle(
            "t",
            r#"{"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"locate_place",
               "arguments":{"name":"Lannisport"}}}"#,
        )
        .unwrap();
        assert!(out.contains("\\\"found\\\":false"), "{out}");
    }
}
