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
//! Two more, in `browse.rs`, are offered only when this box has the keys for
//! them: `search_web` and `fetch_page`. They are the ones that leave the
//! machine and cost money, so they are counted -- `GATES.web_calls` per session,
//! spent before the request goes out and reported back to the agent so it can
//! budget rather than discover the ceiling by hitting it.
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
    /// Put a project on screen: its mark, its diagram, or both.
    ///
    /// Which parts, rather than a whole card, because the three are different
    /// answers -- and an agent that merely mentions a project in passing asks
    /// for none of them and gets the facts alone.
    Work { id: String, mark: bool, diagram: bool },
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
    /// Searches and page reads this session has spent.
    ///
    /// Per board rather than per process: one visitor asking a lot of questions
    /// should not be able to leave the next one with nothing, and a counter that
    /// resets when the session ends is the same thing as one that lives on the
    /// board -- `forget` takes it with the rest.
    spent: usize,
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
        .insert(token.clone(), Board { to: tx, place, spent: 0 });
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

/// The projects, parsed once from the same file the projects section reads.
///
/// Compiled in, like every other reader of this file -- the projects section,
/// the card renderer and the first prompt all `include_str!` it. Unlike
/// `models.txt` it is not mounted, so a tool call is a lookup in memory rather
/// than a file read, and the tool cannot disagree with the section about which
/// projects exist.
fn projects() -> &'static [skysheet::data::Project] {
    static P: OnceLock<Vec<skysheet::data::Project>> = OnceLock::new();
    P.get_or_init(|| {
        skysheet::data::parse(include_str!("../../skills/data/projects.txt")).unwrap_or_default()
    })
}

/// Every project's id, in the order the file lists them.
pub fn project_ids() -> Vec<&'static str> {
    projects().iter().map(|p| p.id.as_str()).collect()
}

/// One project by id, for the page. Public because `ask.rs` draws its caption
/// and would otherwise need its own copy of the file.
pub fn project(id: &str) -> Option<&'static skysheet::data::Project> {
    projects().iter().find(|p| p.id == id)
}

/// Find a project by what the agent called it.
///
/// By id first, then by name, then by a loose contains -- a model asked about
/// "the netjail project" or "watch party" should not be told there is no such
/// thing over a word or a hyphen. Stingy enough not to match everything: the
/// needle has to be at least four characters before containment is tried.
fn project_named(want: &str) -> Option<&'static skysheet::data::Project> {
    let want = want.trim().to_lowercase();
    let flat = |s: &str| s.to_lowercase().replace(['-', '_', ' '], "");
    let needle = flat(&want);
    if needle.is_empty() {
        return None;
    }
    let all = projects();
    all.iter()
        .find(|p| flat(&p.id) == needle || flat(&p.name) == needle)
        .or_else(|| {
            (needle.len() >= 4)
                .then(|| all.iter().find(|p| flat(&p.id).contains(&needle)))
                .flatten()
        })
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

/// Which tools this box can serve right now, by name.
///
/// Kept beside `tool_list` and checked against it by a test, because the two
/// answer the same question and a disagreement between them is the shape of the
/// bug this exists to prevent: an agent quietly missing a capability, with the
/// handshake succeeding and nothing anywhere saying which tools it got.
fn on_offer() -> Vec<&'static str> {
    let mut names = vec!["locate_place", "show_map", "locate_visitor", "hide_map", "show_project"];
    if crate::browse::can_search() {
        names.push("search_web");
    }
    if crate::browse::can_read() {
        names.push("fetch_page");
    }
    names
}

/// Say what the agent is getting, and what it is not getting and why.
///
/// Printed once at start. The alternative is how this was found: the section
/// answers a question that wanted the web, says it cannot search, and there is
/// nothing in the log to distinguish "no key on this box" from a bug -- because
/// a tool that is not offered leaves no trace at all. One line at boot is the
/// whole diagnosis.
fn say_what_is_offered() {
    let names = on_offer();
    eprintln!("portfolio: serving {} tools: {}", names.len(), names.join(", "));
    for (what, keyed, var) in [
        ("search_web", crate::browse::can_search(), "EXA_API_KEY"),
        ("fetch_page", crate::browse::can_read(), "JINA_API_KEY"),
    ] {
        if !keyed {
            eprintln!(
                "portfolio: no {what} -- ${var} is unset or empty, so the agent has no {}",
                match what {
                    "search_web" => "way to search the web",
                    _ => "way to read a page",
                }
            );
        }
    }
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
    let core = format!(
        r#"{{"name":"locate_place",
          "description":"Look up where anything is by name: a country, a state, a city, a town, a village, a monument, a lake, a mall, a cafe. Returns latitude, longitude, a zoom that frames it, `source` -- the map data on this box, or OpenStreetMap -- and `on_this_map`. That last one matters: the lookup covers the world and the map on screen only covers India, so `on_this_map:false` means you know where the place is and cannot show it. Say where it is in words and do not call show_map, because an empty frame presented as a map of Paris is worse than no picture. Pass the zoom back to show_map unless the answer is about something smaller than what you looked up.\n\nAlways look a place up rather than recalling its coordinates. A wrong one puts the camera in the sea and nothing on screen says so.\n\nIt tries three things in order and you need not care which answers: an index of India built into this box, then that city's own streets, then OpenStreetMap. `found:false` means all three came up empty, and then say you cannot place it rather than guessing a point.",
          "inputSchema":{{"type":"object","properties":{{
            "name":{{"type":"string","description":"The place name, e.g. Jaipur, Kerala, Ahmedabad."}},
            "near":{{"type":"string","description":"The town or city to look inside, when `name` is something within one -- a cafe, a mall, a park, a hospital, a temple. Give it whenever you have it: it is what tells a Zen Cafe in Ahmedabad from the several elsewhere, and it lets this box search that city's own streets before going out to the internet. A town or a city only: a country or a state here is a search for a street inside a country, which finds nothing and costs a round trip. Leave it out when `name` is itself a city, a state or a country."}}
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
          "inputSchema":{{"type":"object","properties":{{}}}}}}"#,
        json::quote(map_when)
    );

    // The web tools are listed only when this box can actually use them. A tool
    // an agent can see and cannot use is worse than one it cannot see: it
    // reaches for it, gets a failure, and reports having looked.
    let mut tools = vec![core, one_of("show_project", PROJECT_WHEN, PROJECT_ARGS)];
    if crate::browse::can_search() {
        tools.push(one("search_web", SEARCH_WHEN, "query", QUERY_ARG));
    }
    if crate::browse::can_read() {
        tools.push(one("fetch_page", FETCH_WHEN, "url", URL_ARG));
    }
    format!(r#"{{"tools":[{}]}}"#, tools.join(","))
}

/// When to search, in the agent's words rather than ours. See `tool_list` for
/// why this is a description and not a line in the system prompt.
const SEARCH_WHEN: &str = "Search the web and get back the pages that answer a \
    question, each with its address and a paragraph of its own text.\n\n\
    Reach for it whenever an answer needs something this box cannot know: \
    anything current, anything after your training, documentation, a fact about \
    the world, or a claim you are about to make and are not certain of. Looking \
    something up and saying what you found is worth far more than a confident \
    guess, and the visitor can tell the difference.\n\n\
    Not for questions about Prince, his work, or this site. All of that is \
    already in front of you; searching the web for it finds somebody else with \
    the same name.\n\n\
    It costs money and this conversation has a small allowance, so ask one \
    well-formed question rather than three vague ones. Every reply says how many \
    lookups are left. Read the gists before deciding you need the whole page -- \
    often they are already the answer.";

const QUERY_ARG: &str = "What to search for, as a question or a phrase. Write it \
    the way you would type it into a search box, not as a sentence addressed to \
    the visitor.";

const FETCH_WHEN: &str = "Read one web page and get it back as text you can \
    quote.\n\n\
    Use it when a search result's gist is not enough, or when the visitor gives \
    you a link and asks what is in it. Give the whole address, including \
    https://.\n\n\
    A long page comes back cut off and the reply says so. If the part you were \
    given does not contain the answer, say that rather than filling in the rest. \
    It comes out of the same allowance as search_web.\n\n\
    A page that is not there usually comes back as the site's own \"not found\" \
    page rather than as an error, because that is genuinely what is at that \
    address -- the reader read what was there. If that is what you are looking \
    at, say the link is dead instead of describing it.";

const URL_ARG: &str = "The full address of the page, including https://. One \
    page per call.";

const PROJECT_WHEN: &str = "Everything this box knows about one of Prince's \
    projects, and optionally its picture beside your answer.\n\n\
    It always returns the facts -- what it is, when, the repository, what it is \
    built with, and the two to four paragraphs of engineering that are the \
    point of it. Use those to answer; they are more specific and more accurate \
    than anything you can recall about a repository you have not read.\n\n\
    `show` is what appears on screen, and the three are different answers:\n\
    - `mark` -- the project's emblem, large. For naming one in passing, or for \
    listing several: call it once per project and the last one stays up.\n\
    - `diagram` -- an animated drawing of how the thing actually works, made \
    for that project. This is the one worth reaching for when somebody asks \
    what a project *is* or how it works: it explains in a way a paragraph \
    cannot, and it is sitting right there.\n\
    - both, for a full answer about one project.\n\n\
    Leave `show` out entirely when the project is a passing mention, or when \
    the question is about something else. A picture that arrives for every \
    answer stops meaning anything, and the page clears itself when the next \
    answer does not ask for one.\n\n\
    Ask by name -- `netjail`, `watch-party`, `termap`. `found:false` lists what \
    there is, so an unfamiliar name costs one call rather than a wrong answer.";

const PROJECT_ARGS: &str = r#""name":{"type":"string","description":"Which project, by name. Hyphens and spaces are both fine."},
            "show":{"type":"array","description":"What to draw beside your answer: `mark`, `diagram`, both, or leave it out for the facts alone.","items":{"type":"string","enum":["mark","diagram"]}}"#;

/// A tool with a hand-written argument block, for the ones whose arguments are
/// not all one string.
fn one_of(name: &str, when: &str, args: &str) -> String {
    format!(
        r#"{{"name":{},"description":{},"inputSchema":{{"type":"object","properties":{{{args}}},"required":["name"]}}}}"#,
        json::quote(name),
        json::quote(when)
    )
}

/// One tool with one string argument, which is the shape both web tools have.
fn one(name: &str, when: &str, arg: &str, about: &str) -> String {
    format!(
        r#"{{"name":{},"description":{},"inputSchema":{{"type":"object","properties":{{{}:{{"type":"string","description":{}}}}},"required":[{}]}}}}"#,
        json::quote(name),
        json::quote(when),
        json::quote(arg),
        json::quote(about),
        json::quote(arg)
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
            // An *empty* `places` counts as absent. A real model filled every
            // field the schema offered -- `places: []` alongside a perfectly
            // good lat and lon -- and the list won, so the call was refused
            // with the point sitting right there in it. It then recovered by
            // duplicating the point into the list, which is two wasted turns
            // and a visitor watching a tool fail twice.
            let listed = args
                .and_then(|a| a.get("places"))
                .and_then(|p| p.as_array())
                .filter(|list| !list.is_empty());
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
                    let (from_lat, from_lon) = (n("lat")?, n("lon")?);
                    if !(-90.0..=90.0).contains(&from_lat) || !(-180.0..=180.0).contains(&from_lon)
                    {
                        return None;
                    }
                    // A journey to where the camera already is, is not a
                    // journey. Models fill the field because it is there: one
                    // sent a `from` identical to the destination, which is a
                    // second and a half of flight that lands where it took off
                    // and reads on screen as a stall. The description asks them
                    // not to; this makes it not matter.
                    const SAME: f64 = 0.002;
                    if (from_lat - lat).abs() < SAME && (from_lon - lon).abs() < SAME {
                        return None;
                    }
                    Some((
                        from_lat,
                        from_lon,
                        n("zoom").unwrap_or(num("zoom").unwrap_or(11.5)).clamp(3.0, 16.5),
                    ))
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
        "search_web" => {
            // Checked before anything is spent: a malformed call is our mistake
            // to report, not the visitor's allowance to pay for.
            let query = arg_str("query").trim();
            if query.len() < 2 {
                return err("search_web needs a `query`");
            }
            match spend(token) {
                Err(why) => err(why),
                Ok(left) => match crate::browse::search(query) {
                    Err(why) => err(&why),
                    Ok(hits) => {
                        let rows: Vec<String> = hits
                            .iter()
                            .map(|h| {
                                format!(
                                    r#"{{"title":{},"url":{},"gist":{}}}"#,
                                    json::quote(&h.title),
                                    json::quote(&h.url),
                                    json::quote(&h.gist)
                                )
                            })
                            .collect();
                        text(&format!(
                            r#"{{"found":{},"lookups_left":{left},"results":[{}]}}"#,
                            rows.len(),
                            rows.join(",")
                        ))
                    }
                },
            }
        }
        "fetch_page" => {
            let url = arg_str("url").trim();
            if url.is_empty() {
                return err("fetch_page needs a `url`");
            }
            match spend(token) {
                Err(why) => err(why),
                Ok(left) => match crate::browse::read(url) {
                    Err(why) => err(&why),
                    Ok(page) => text(&format!(
                        r#"{{"url":{},"title":{},"clipped":{},"lookups_left":{left},"text":{}}}"#,
                        json::quote(&page.url),
                        json::quote(&page.title),
                        page.clipped,
                        json::quote(&page.text)
                    )),
                },
            }
        }
        "show_project" => {
            let want = arg_str("name");
            let Some(p) = project_named(want) else {
                let known: Vec<String> =
                    projects().iter().map(|p| json::quote(&p.id)).collect();
                return text(&format!(
                    r#"{{"found":false,"asked":{},"projects":[{}]}}"#,
                    json::quote(want),
                    known.join(",")
                ));
            };
            // What to draw. An unknown word in the list is ignored rather than
            // refused: the facts are the answer and a typo in the picture should
            // not cost them.
            let asked = |what: &str| {
                args.and_then(|a| a.get("show"))
                    .and_then(|s| s.as_array())
                    .is_some_and(|list| {
                        list.iter().any(|v| v.as_str().is_some_and(|s| s.trim() == what))
                    })
            };
            let (mark, diagram) = (asked("mark"), asked("diagram"));
            send(token, Directive::Work { id: p.id.clone(), mark, diagram });

            let beats: Vec<String> = p
                .beats
                .iter()
                .map(|b| {
                    format!(
                        r#"{{"heading":{},"text":{}}}"#,
                        json::quote(&b.head),
                        json::quote(&b.body)
                    )
                })
                .collect();
            let tools: Vec<String> = p.tools.iter().map(|t| json::quote(t)).collect();
            text(&format!(
                r#"{{"found":true,"id":{},"name":{},"year":{},"repo":{},"what":{},"built_with":[{}],"scale":{},"engineering":[{}],"drawn":{{"mark":{mark},"diagram":{diagram}}}{}}}"#,
                json::quote(&p.id),
                json::quote(&p.name),
                json::quote(&p.year),
                json::quote(&p.repo),
                json::quote(&p.tag),
                tools.join(","),
                json::quote(&p.stats),
                beats.join(","),
                // Said plainly, because a card written from a summary rather
                // than from the source is a thing the visitor is entitled to
                // know before they trust a detail of it.
                match p.draft {
                    true => r#","note":"this description was written from a summary rather than from the repository, and may be wrong in its details -- say so if you lean on it"#.to_string() + "\"",
                    false => String::new(),
                }
            ))
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
        // The question, and the address. Both are the whole point of the row:
        // a visitor watching `search_web` with nothing beside it cannot tell
        // whether it went looking for what they asked about.
        "search_web" => str_of("query").to_string(),
        "show_project" => str_of("name").to_string(),
        "fetch_page" => str_of("url").to_string(),
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

/// Spend one of this session's web lookups, and say how many are left.
///
/// Spent *before* the request goes out, not after it succeeds: the abuse case is
/// a tool called in a loop, and a call that fails costs the service the same
/// work as one that does not. The cost of that choice is that a flaky minute can
/// eat a session's allowance, which is the cheaper mistake.
///
/// The error is the sentence the agent is given, because the two ways this can
/// fail are different things to be told: a session that has gone is not
/// something to try again, and an exhausted allowance is not something to
/// apologise for -- it is a reason to answer from what is already known.
fn spend(token: &str) -> Result<usize, &'static str> {
    let mut table = boards().lock().unwrap_or_else(|e| e.into_inner());
    let Some(board) = table.get_mut(token) else {
        return Err("that screen is gone");
    };
    let ceiling = crate::gates::GATES.web_calls;
    if board.spent >= ceiling {
        return Err("this conversation has used all of its web lookups -- \
                    answer from what you already have, and say that is what you are doing");
    }
    board.spent += 1;
    Ok(ceiling - board.spent)
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
        say_what_is_offered();
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

    /// A tool's answer, parsed. The result carries JSON *inside* a JSON string,
    /// so matching escaped substrings against it is a test that fails on its own
    /// escaping rather than on the answer -- which is exactly what happened to
    /// the first version of the tests below.
    fn answer(out: &str) -> Value {
        let text = result_of(out)
            .get("content")
            .and_then(|c| c.as_array())
            .and_then(|c| c.first())
            .and_then(|c| c.get("text"))
            .and_then(|t| t.as_str())
            .unwrap_or_default()
            .to_string();
        json::parse(&text).unwrap_or_else(|| panic!("not json: {text}"))
    }

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

    /// What the log says it serves is what `tools/list` serves.
    ///
    /// Two lists that answer the same question, and the whole point of the log
    /// line is to be trustworthy about a thing that is otherwise invisible. A
    /// log that says `search_web` while the agent was handed four tools would
    /// be worse than no log at all.
    #[test]
    fn the_log_line_lists_exactly_what_the_agent_is_handed() {
        let _lock = crate::visits::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        for (exa, jina) in [(false, false), (true, false), (false, true), (true, true)] {
            match exa {
                true => std::env::set_var("EXA_API_KEY", "k"),
                false => std::env::remove_var("EXA_API_KEY"),
            }
            match jina {
                true => std::env::set_var("JINA_API_KEY", "k"),
                false => std::env::remove_var("JINA_API_KEY"),
            }
            let out = handle("t", r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#).unwrap();
            let served: Vec<String> = result_of(&out)
                .get("tools")
                .and_then(|t| t.as_array())
                .expect("no tools")
                .iter()
                .filter_map(|t| t.get("name").and_then(|n| n.as_str()).map(str::to_string))
                .collect();
            let said: Vec<String> = on_offer().iter().map(|s| s.to_string()).collect();
            assert_eq!(said, served, "the log and the server disagree (exa {exa}, jina {jina})");
        }
        std::env::remove_var("EXA_API_KEY");
        std::env::remove_var("JINA_API_KEY");
    }

    /// The two that cost money are offered only when this box can pay: a tool
    /// an agent can see and cannot use gets reached for, fails, and is then
    /// reported as a search that happened.
    #[test]
    fn the_web_tools_are_offered_only_when_this_box_has_the_keys() {
        let _lock = crate::visits::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let names = || -> Vec<String> {
            let out = handle("t", r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#).unwrap();
            result_of(&out)
                .get("tools")
                .and_then(|t| t.as_array())
                .expect("no tools")
                .iter()
                .filter_map(|t| t.get("name").and_then(|n| n.as_str()).map(str::to_string))
                .collect()
        };

        std::env::remove_var("EXA_API_KEY");
        std::env::remove_var("JINA_API_KEY");
        let without = names();
        assert!(!without.contains(&"search_web".to_string()), "{without:?}");
        assert!(!without.contains(&"fetch_page".to_string()), "{without:?}");
        // The map tools do not depend on a credential and never disappear.
        assert!(without.contains(&"show_map".to_string()), "{without:?}");

        // Empty is the same as absent -- compose passes `${EXA_API_KEY:-}`, so
        // the variable exists and is blank on any host that has not set one.
        std::env::set_var("EXA_API_KEY", "");
        assert!(!names().contains(&"search_web".to_string()));

        std::env::set_var("EXA_API_KEY", "k");
        std::env::set_var("JINA_API_KEY", "k");
        let with = names();
        assert!(with.contains(&"search_web".to_string()), "{with:?}");
        assert!(with.contains(&"fetch_page".to_string()), "{with:?}");
        std::env::remove_var("EXA_API_KEY");
        std::env::remove_var("JINA_API_KEY");

        // Whatever the environment, the list is still JSON an agent can read.
        assert!(crate::json::parse(&handle("t", r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#).unwrap()).is_some());
    }

    /// The allowance is per session and it runs out.
    ///
    /// Also proves the *order*: the ceiling is checked before the request goes
    /// out. If it were checked after, this test would either reach the network
    /// or report a missing key, and both read differently from what it asserts.
    #[test]
    fn the_web_lookups_run_out_and_say_so() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let board = register(tx, None);

        for i in 0..crate::gates::GATES.web_calls {
            assert!(spend(&board).is_ok(), "lookup {i} was refused early");
        }
        assert!(spend(&board).is_err(), "the ceiling did not hold");

        let out = handle(
            &board,
            r#"{"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":"search_web",
               "arguments":{"query":"where is the nearest cafe"}}}"#,
        )
        .unwrap();
        let r = result_of(&out);
        assert_eq!(r.get("isError").and_then(|e| e.as_bool()), Some(true), "{out}");
        let said = r
            .get("content")
            .and_then(|c| c.as_array())
            .and_then(|c| c.first())
            .and_then(|c| c.get("text"))
            .and_then(|t| t.as_str())
            .unwrap_or("");
        assert!(said.contains("web lookups"), "{said}");

        // A different session still has its own.
        let (tx2, _rx2) = std::sync::mpsc::channel();
        let other = register(tx2, None);
        assert!(spend(&other).is_ok(), "one visitor spent another's allowance");
        forget(&board);
        forget(&other);
    }

    /// A malformed call is not charged for. The reply says what is missing.
    #[test]
    fn a_call_with_nothing_to_look_up_costs_nothing() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let board = register(tx, None);
        for bad in [
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"search_web","arguments":{}}}"#,
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"search_web","arguments":{"query":" "}}}"#,
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"fetch_page","arguments":{}}}"#,
        ] {
            let out = handle(&board, bad).unwrap();
            assert_eq!(
                result_of(&out).get("isError").and_then(|e| e.as_bool()),
                Some(true),
                "{out}"
            );
        }
        let left = boards().lock().unwrap().get(&board).map(|b| b.spent);
        assert_eq!(left, Some(0), "a malformed call spent a lookup");
        forget(&board);
    }

    /// The row the visitor watches. A search with nothing beside it does not
    /// say whether the agent looked for what was asked about.
    #[test]
    fn a_web_call_says_what_it_went_looking_for() {
        let args = crate::json::parse(r#"{"query":"acp schema","url":"https://x.test/a"}"#).unwrap();
        assert_eq!(detail_of("search_web", Some(&args)), "acp schema");
        assert_eq!(detail_of("fetch_page", Some(&args)), "https://x.test/a");
    }

    /// The facts always come back; the picture is optional and separate.
    ///
    /// This is the shape the tool exists for: "tell me about netjail" wants the
    /// diagram, "which projects are there" wants marks and no diagrams, and an
    /// answer that merely mentions one wants neither -- and all three are one
    /// call with the same return.
    #[test]
    fn a_project_returns_its_facts_and_draws_only_what_was_asked_for() {
        let (tx, rx) = std::sync::mpsc::channel();
        let token = register(tx, None);
        let ask = |show: &str| {
            format!(
                r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"show_project",
                   "arguments":{{"name":"netjail"{show}}}}}}}"#
            )
        };
        // Facts alone: no `show`, so nothing is drawn -- but the panel is still
        // told, because an answer about something else should not leave the
        // last picture sitting there.
        let a = answer(&handle(&token, &ask("")).unwrap());
        assert_eq!(a.get("found").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(a.get("id").and_then(|v| v.as_str()), Some("netjail"));
        assert!(a.get("repo").is_some() && a.get("built_with").is_some(), "{a:?}");
        let beats = a.get("engineering").and_then(|b| b.as_array()).expect("no beats");
        assert!(beats.len() >= 2, "a project with no engineering on it");
        assert!(beats[0].get("heading").and_then(|h| h.as_str()).is_some());
        match rx.try_iter().find_map(|d| match d {
            Directive::Work { id, mark, diagram } => Some((id, mark, diagram)),
            _ => None,
        }) {
            Some((id, mark, diagram)) => {
                assert_eq!(id, "netjail");
                assert!(!mark && !diagram, "drew a picture nobody asked for");
            }
            None => panic!("the page was never told"),
        }

        // A diagram, and only a diagram.
        let a = answer(&handle(&token, &ask(r#","show":["diagram"]"#)).unwrap());
        let drawn = a.get("drawn").expect("no `drawn`");
        assert_eq!(drawn.get("mark").and_then(|v| v.as_bool()), Some(false));
        assert_eq!(drawn.get("diagram").and_then(|v| v.as_bool()), Some(true));
        let drew = rx.try_iter().find_map(|d| match d {
            Directive::Work { mark, diagram, .. } => Some((mark, diagram)),
            _ => None,
        });
        assert_eq!(drew, Some((false, true)));

        // Both.
        handle(&token, &ask(r#","show":["mark","diagram"]"#)).unwrap();
        let drew = rx.try_iter().find_map(|d| match d {
            Directive::Work { mark, diagram, .. } => Some((mark, diagram)),
            _ => None,
        });
        assert_eq!(drew, Some((true, true)));
        forget(&token);
    }

    /// A name that is not a project lists the ones that are, so the model spends
    /// one call rather than answering from memory about a repository it has
    /// never read.
    #[test]
    fn an_unknown_project_answers_with_the_ones_there_are() {
        let out = handle(
            "t",
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"show_project",
               "arguments":{"name":"kubernetes"}}}"#,
        )
        .unwrap();
        let a = answer(&out);
        assert_eq!(a.get("found").and_then(|v| v.as_bool()), Some(false));
        let known: Vec<&str> = a
            .get("projects")
            .and_then(|p| p.as_array())
            .expect("no list of what there is")
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(known.contains(&"netjail") && known.contains(&"termap"), "{known:?}");
    }

    /// Asked the way a model actually asks: with a space, with the display name,
    /// with a hyphen. All three are the same project.
    #[test]
    fn a_project_answers_to_what_a_model_would_call_it() {
        for name in ["watch-party", "watch party", "Watch Party", "watchparty"] {
            let found = project_named(name).map(|p| p.id.as_str());
            assert_eq!(found, Some("watch-party"), "`{name}` did not resolve");
        }
        // And stinginess: a short word must not match half the list.
        assert!(project_named("a").is_none());
        assert!(project_named("").is_none());
        assert!(project_named("   ").is_none());
    }

    /// A card written from a summary says so in the tool's own answer, so the
    /// agent can hedge instead of stating a guess as a fact.
    #[test]
    fn a_draft_card_admits_it_to_the_agent() {
        let drafts: Vec<&str> = projects()
            .iter()
            .filter(|p| p.draft)
            .map(|p| p.id.as_str())
            .collect();
        if drafts.is_empty() {
            return; // every card is written from source now, which is the goal
        }
        let out = handle(
            "t",
            &format!(
                r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"show_project",
                   "arguments":{{"name":"{}"}}}}}}"#,
                drafts[0]
            ),
        )
        .unwrap();
        let note = answer(&out).get("note").and_then(|n| n.as_str()).unwrap_or("").to_string();
        assert!(note.contains("written from a summary"), "a draft did not admit it: {note:?}");
    }

    /// An unknown scene name is ignored rather than refused: the facts are the
    /// answer, and a typo in the picture should not cost them.
    #[test]
    fn a_nonsense_part_costs_the_picture_and_not_the_answer() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let token = register(tx, None);
        let out = handle(
            &token,
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"show_project",
               "arguments":{"name":"termap","show":["hologram"]}}}"#,
        )
        .unwrap();
        let a = answer(&out);
        assert_eq!(a.get("found").and_then(|v| v.as_bool()), Some(true));
        let drawn = a.get("drawn").expect("no `drawn`");
        assert_eq!(drawn.get("mark").and_then(|v| v.as_bool()), Some(false));
        assert_eq!(drawn.get("diagram").and_then(|v| v.as_bool()), Some(false));
        forget(&token);
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

    /// The payload a real model actually sent, which this used to refuse.
    ///
    /// `places: []` beside a good lat and lon: the model filled every field the
    /// schema offered. The empty list won, the call failed, and it recovered by
    /// copying the point into the list -- two turns and two failed tool rows on
    /// somebody's screen. Also in here: a `from` identical to the destination,
    /// which is a flight that lands where it took off.
    #[test]
    fn the_payload_a_model_really_sent_is_drawn_rather_than_refused() {
        let (tx, rx) = std::sync::mpsc::channel();
        let token = register(tx, None);
        let out = handle(
            &token,
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"show_map",
               "arguments":{"lat":26.91546,"lon":75.81898,"zoom":11,"label":"Jaipur, Rajasthan",
                 "note":"Known as the Pink City.",
                 "from":{"lat":26.91546,"lon":75.81898,"zoom":11},
                 "places":[]}}}"#,
        )
        .unwrap();
        assert!(!out.contains("isError"), "the point beside an empty list was refused: {out}");
        let map = rx.try_iter().find_map(|d| match d {
            Directive::Map { stops } => Some(stops),
            _ => None,
        });
        let stops = map.expect("nothing reached the screen");
        assert_eq!(stops.len(), 1);
        assert_eq!(stops[0].label, "Jaipur, Rajasthan");
        assert!((stops[0].lat - 26.91546).abs() < 1e-5);
        assert!(
            stops[0].from.is_none(),
            "a journey to where the camera already is: {:?}",
            stops[0].from
        );
        forget(&token);
    }

    /// A `from` that is somewhere else is still a journey. The point of the
    /// check above is to drop the useless ones, not the feature.
    #[test]
    fn a_from_somewhere_else_is_still_a_journey() {
        let (tx, rx) = std::sync::mpsc::channel();
        let token = register(tx, None);
        handle(
            &token,
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"show_map",
               "arguments":{"lat":23.02,"lon":73.07,"zoom":12,"label":"Kapadwanj",
                 "from":{"lat":23.03,"lon":72.58,"zoom":10}}}}"#,
        )
        .unwrap();
        let stops = rx
            .try_iter()
            .find_map(|d| match d {
                Directive::Map { stops } => Some(stops),
                _ => None,
            })
            .expect("nothing reached the screen");
        let (lat, lon, zoom) = stops[0].from.expect("the journey was dropped");
        assert!((lat - 23.03).abs() < 1e-6 && (lon - 72.58).abs() < 1e-6);
        assert_eq!(zoom, 10.0);
        forget(&token);
    }

    /// A `places` list with things in it still wins over a stray top-level
    /// point, which is what it is for.
    #[test]
    fn a_list_with_places_in_it_is_the_route() {
        let (tx, rx) = std::sync::mpsc::channel();
        let token = register(tx, None);
        handle(
            &token,
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"show_map",
               "arguments":{"lat":0.0,"lon":0.0,
                 "places":[{"lat":23.02,"lon":73.07,"label":"one"},
                           {"lat":26.91,"lon":75.82,"label":"two"}]}}}"#,
        )
        .unwrap();
        let stops = rx
            .try_iter()
            .find_map(|d| match d {
                Directive::Map { stops } => Some(stops),
                _ => None,
            })
            .expect("nothing reached the screen");
        assert_eq!(stops.len(), 2, "the route was not the route");
        assert_eq!(stops[0].label, "one");
        forget(&token);
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
