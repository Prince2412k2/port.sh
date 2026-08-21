//! Who came, from where, and what they asked.
//!
//! This is a portfolio: the point of putting it on a public address is to find
//! out whether anybody is interested, and "somebody in Berlin asked about the
//! map three times this week" is the answer to that question. So visits are
//! recorded, and a returning visitor is recognised as one.
//!
//! **Append-only JSONL**, the same shape `reach.rs` writes and for the same
//! reasons: no dependency, crash-safe (a torn write costs the last line and not
//! the file), and readable with `jq` or `grep` at three in the morning. There is
//! no schema migration to get wrong because there is no schema.
//!
//! **What identifies somebody.** Over SSH, two things arrive for free and
//! neither is a fingerprint taken behind anyone's back: the username they typed
//! -- `ssh alice@host` makes them alice -- and the public key they authenticated
//! with. The key is the useful one, because it is stable across visits and
//! across addresses, so it is what "remember me" hangs on. The browser has no
//! equivalent, so the web client is issued a random id it keeps in
//! `localStorage`; clearing it makes somebody a new visitor, which is the
//! correct behaviour for a thing a visitor controls.
//!
//! **Where** comes from the same geolocation the map already uses, so there is
//! one HTTP call in this codebase and not two. It runs on its own thread: a
//! session must never wait on a third-party lookup to draw its first frame.

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::json;

/// Where the log goes.
pub fn path() -> PathBuf {
    if let Ok(p) = std::env::var("PORTFOLIO_VISITS") {
        return PathBuf::from(p);
    }
    PathBuf::from("data/visits.jsonl")
}

/// Serialises the tests that point `PORTFOLIO_VISITS` somewhere of their own.
///
/// Environment variables are per-process and the test runner is threaded; two
/// tests writing different files through one variable trip over each other, and
/// only sometimes. `reach.rs` learned this the same way.
#[cfg_attr(not(test), allow(dead_code))]
pub static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// When this process started answering, for `/uptime`.
///
/// Initialised on the first call, so `boot()` is called early from `main` to
/// make that the actual start rather than the first time somebody asked.
pub fn boot() -> std::time::Instant {
    static START: OnceLock<std::time::Instant> = OnceLock::new();
    *START.get_or_init(std::time::Instant::now)
}

pub fn uptime() -> u64 {
    boot().elapsed().as_secs()
}

thread_local! {
    /// Where this session's visitor turned out to be.
    ///
    /// A thread-local because a session owns a thread for its whole life, and
    /// this is the one thing the page wants back from a lookup that happens off
    /// it. The slot is shared with the lookup thread rather than the value, so
    /// `/whoami` reads whatever has arrived by the time it is asked and nothing
    /// ever waits.
    static PLACE: std::cell::RefCell<Option<std::sync::Arc<Mutex<Option<termap::home::Where>>>>> =
        const { std::cell::RefCell::new(None) };
}

/// Where this session's visitor appears to be, if the lookup has come back.
///
/// The whole answer, not the words: `/whoami` wants the label and the chat's
/// map panel wants the point, and one lookup should serve both. Two calls to a
/// public geolocation service to draw one screen would be a poor trade.
pub fn here() -> Option<termap::home::Where> {
    PLACE.with(|p| {
        let slot = p.borrow().clone()?;
        let found = slot.lock().unwrap_or_else(|e| e.into_inner()).clone();
        found
    })
}

/// The slot itself, so something off this thread can read it later.
///
/// `here` is a thread-local because a session owns a thread for its whole life,
/// and that is the right shape for the page. The tool server is a different
/// thread entirely and still has to be able to answer "where is this visitor",
/// so it is handed the shared slot rather than a copy of whatever had arrived
/// at the moment it asked.
pub fn here_slot() -> Option<std::sync::Arc<Mutex<Option<termap::home::Where>>>> {
    PLACE.with(|p| p.borrow().clone())
}

/// The same thing as a line of text: "Kapadwanj, Gujarat, India".
pub fn last_seen() -> Option<String> {
    here().map(|w| w.label())
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Session ids, unique within one run of the process.
///
/// A counter and the start time rather than a random id: there is no `rand` in
/// this crate's dependencies, and the only thing this has to be is unique in the
/// file it is written to.
fn next_id() -> String {
    static N: AtomicU64 = AtomicU64::new(0);
    format!("{}-{}", now(), N.fetch_add(1, Ordering::Relaxed))
}

/// Everything known about a visitor at the moment they connect.
#[derive(Debug, Clone, Default)]
pub struct Who {
    /// `ssh` or `web`.
    pub via: &'static str,
    /// What they typed in front of the `@`. Empty on the web, where there is
    /// nowhere to type one.
    pub user: String,
    /// The stable identity: an SSH key fingerprint, or the browser's own id.
    /// Empty when there is nothing to go on, and then they are always new.
    pub id: String,
    pub ip: String,
    /// SSH client version string, or the browser's user agent.
    pub client: String,
}

/// One visit, open until it is closed.
pub struct Visit {
    pub session: String,
    pub who: Who,
    started: u64,
    turns: u64,
}

/// How many times this id has been seen before, and when it first was.
///
/// A linear scan of the file. That is the right amount of machinery for a
/// portfolio: the log grows by a line per question, so it is a megabyte after a
/// very good year, and the alternative is a database process to answer a
/// question asked once per connection.
fn history(id: &str) -> (u64, u64) {
    if id.is_empty() {
        return (0, 0);
    }
    let Ok(text) = std::fs::read_to_string(path()) else {
        return (0, 0);
    };
    let mut seen = 0;
    let mut first = 0;
    for line in text.lines() {
        let Some(v) = json::parse(line) else { continue };
        if v.get("event").and_then(|e| e.as_str()) != Some("arrive") {
            continue;
        }
        if v.get("id").and_then(|i| i.as_str()) != Some(id) {
            continue;
        }
        seen += 1;
        let at = v.get("at").and_then(|a| a.as_f64()).unwrap_or(0.0) as u64;
        if first == 0 || at < first {
            first = at;
        }
    }
    (seen, first)
}

fn append(line: &str) {
    let p = path();
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&p) {
        let _ = writeln!(f, "{line}");
    }
}

impl Visit {
    /// Record an arrival and start counting.
    ///
    /// The geolocation is looked up on its own thread and appended when it comes
    /// back, as a second line keyed by session. Blocking a visitor's first frame
    /// on somebody else's HTTP service would be a poor trade for a city name.
    pub fn open(who: Who) -> Visit {
        let (seen, first) = history(&who.id);
        let session = next_id();
        append(&format!(
            r#"{{"at":{},"event":"arrive","session":{},"via":{},"user":{},"id":{},"ip":{},"client":{},"returning":{},"seen_before":{},"first_seen":{}}}"#,
            now(),
            json::quote(&session),
            json::quote(who.via),
            json::quote(&who.user),
            json::quote(&who.id),
            json::quote(&who.ip),
            json::quote(&who.client),
            seen > 0,
            seen,
            first,
        ));

        if !who.ip.is_empty() && !is_private(&who.ip) {
            let (ip, session) = (who.ip.clone(), session.clone());
            // Shared with the lookup thread so `/whoami` can read the answer if
            // it has arrived, and get nothing if it has not. `open` runs on the
            // session's own thread, which is why this reaches the page at all.
            let slot = std::sync::Arc::new(Mutex::new(None));
            PLACE.with(|p| *p.borrow_mut() = Some(slot.clone()));
            std::thread::spawn(move || {
                if let Some(w) = termap::home::locate(&ip) {
                    *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(w.clone());
                    append(&format!(
                        r#"{{"at":{},"event":"where","session":{},"city":{},"region":{},"country":{},"lat":{:.4},"lon":{:.4}}}"#,
                        now(),
                        json::quote(&session),
                        json::quote(&w.city),
                        json::quote(&w.region),
                        json::quote(&w.country),
                        w.lat,
                        w.lon,
                    ));
                }
            });
        }

        Visit { session, who, started: now(), turns: 0 }
    }

    /// Record one exchange. Both halves, because the question alone says what
    /// people want and the answer says whether they got it.
    pub fn asked(&mut self, question: &str, answer: &str, spent: Option<crate::acp::Spend>) {
        self.turns += 1;
        // What it cost, where the agent reported it. Here rather than on screen:
        // a token count is a billing detail belonging to whoever pays for it,
        // and this file is the answer to "what did strangers cost me". A `/cert`
        // or a `/coffee` never reached a model, so the fields are absent rather
        // than zero -- absent and free are different claims.
        let cost = match spent {
            None => String::new(),
            Some(s) => format!(
                r#","used":{},"window":{}{}"#,
                s.used,
                s.window,
                match s.cost {
                    // Six places, because a cheap answer is a fraction of a
                    // cent and rounding it to two makes every one of them free.
                    Some(c) => format!(r#","cost":{c:.6}"#),
                    None => String::new(),
                }
            ),
        };
        append(&format!(
            r#"{{"at":{},"event":"ask","session":{},"n":{},"q":{},"a":{}{cost}}}"#,
            now(),
            json::quote(&self.session),
            self.turns,
            json::quote(question.trim()),
            json::quote(answer.trim()),
        ));
    }

    /// Record the departure and how long they stayed.
    pub fn close(&self) {
        // Also to the log the operator is already watching. A portfolio's whole
        // question is whether anybody came, and `docker compose logs` answering
        // it live is worth one line per visit.
        eprintln!(
            "portfolio: visit over -- {}{} from {}, {} question{}",
            if self.who.user.is_empty() { "someone" } else { &self.who.user },
            match self.who.via {
                "web" => " (web)",
                _ => " (ssh)",
            },
            if self.who.ip.is_empty() { "somewhere" } else { &self.who.ip },
            self.turns,
            if self.turns == 1 { "" } else { "s" },
        );
        append(&format!(
            r#"{{"at":{},"event":"leave","session":{},"secs":{},"turns":{}}}"#,
            now(),
            json::quote(&self.session),
            now().saturating_sub(self.started),
            self.turns,
        ));
    }
}

/// Addresses that geolocate to nothing and should not be sent to a public API.
///
/// Lifted in spirit from termap's own check: asking a third party about `10.x`
/// tells them more about the query than it tells us.
pub fn is_private(ip: &str) -> bool {
    let ip = ip.split(':').next().unwrap_or(ip);
    ip.starts_with("10.")
        || ip.starts_with("127.")
        || ip.starts_with("192.168.")
        || ip.starts_with("169.254.")
        || ip == "::1"
        || ip.starts_with("fc")
        || ip.starts_with("fd")
        || ip.is_empty()
        || (ip.starts_with("172.")
            && ip
                .split('.')
                .nth(1)
                .and_then(|o| o.parse::<u8>().ok())
                .is_some_and(|o| (16..32).contains(&o)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("portfolio-visits-{name}.jsonl"));
        let _ = std::fs::remove_file(&p);
        p
    }

    fn read(p: &PathBuf) -> Vec<json::Value> {
        std::fs::read_to_string(p)
            .unwrap_or_default()
            .lines()
            .map(|l| json::parse(l).unwrap_or_else(|| panic!("not JSON: {l}")))
            .collect()
    }

    fn ssh_visitor(user: &str, id: &str) -> Who {
        Who {
            via: "ssh",
            user: user.into(),
            id: id.into(),
            // Private on purpose: no test should reach a geolocation service.
            ip: "10.0.0.4".into(),
            client: "SSH-2.0-OpenSSH_9.6".into(),
        }
    }

    #[test]
    fn a_visit_records_who_arrived_what_they_asked_and_when_they_left() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let p = scratch("basic");
        std::env::set_var("PORTFOLIO_VISITS", &p);

        let mut v = Visit::open(ssh_visitor("alice", "SHA256:AAA"));
        v.asked("why braille?", "Because dots.", None);
        v.close();
        std::env::remove_var("PORTFOLIO_VISITS");

        let rows = read(&p);
        assert_eq!(rows.len(), 3, "{rows:?}");
        assert_eq!(rows[0].get("event").and_then(|e| e.as_str()), Some("arrive"));
        assert_eq!(rows[0].get("user").and_then(|u| u.as_str()), Some("alice"));
        assert_eq!(rows[0].get("via").and_then(|u| u.as_str()), Some("ssh"));
        assert_eq!(rows[0].get("returning").and_then(|r| r.as_bool()), Some(false));
        assert_eq!(rows[1].get("q").and_then(|q| q.as_str()), Some("why braille?"));
        assert_eq!(rows[1].get("a").and_then(|a| a.as_str()), Some("Because dots."));
        assert_eq!(rows[2].get("turns").and_then(|t| t.as_f64()), Some(1.0));
    }

    /// The whole point of keeping an identity: the second visit knows it is one.
    #[test]
    fn somebody_who_comes_back_is_recognised() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let p = scratch("returning");
        std::env::set_var("PORTFOLIO_VISITS", &p);

        Visit::open(ssh_visitor("alice", "SHA256:AAA")).close();
        Visit::open(ssh_visitor("alice", "SHA256:AAA")).close();
        let third = Visit::open(ssh_visitor("alice", "SHA256:AAA"));
        // A different key is a different person, even under the same name.
        let other = Visit::open(ssh_visitor("alice", "SHA256:BBB"));
        std::env::remove_var("PORTFOLIO_VISITS");

        let arrivals: Vec<_> = read(&p)
            .into_iter()
            .filter(|r| r.get("event").and_then(|e| e.as_str()) == Some("arrive"))
            .collect();
        assert_eq!(arrivals.len(), 4);
        assert_eq!(arrivals[0].get("seen_before").and_then(|s| s.as_f64()), Some(0.0));
        assert_eq!(arrivals[2].get("seen_before").and_then(|s| s.as_f64()), Some(2.0));
        assert_eq!(arrivals[2].get("returning").and_then(|r| r.as_bool()), Some(true));
        assert_eq!(
            arrivals[3].get("returning").and_then(|r| r.as_bool()),
            Some(false),
            "a different key was taken for the same visitor"
        );
        let _ = (third.session, other.session);
    }

    /// Nothing identifying is offered, so nothing is claimed. Somebody who
    /// arrives without a key is a stranger every time rather than being merged
    /// with every other anonymous visit.
    #[test]
    fn an_unidentified_visitor_is_never_treated_as_a_returning_one() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let p = scratch("anon");
        std::env::set_var("PORTFOLIO_VISITS", &p);

        Visit::open(ssh_visitor("", "")).close();
        Visit::open(ssh_visitor("", "")).close();
        std::env::remove_var("PORTFOLIO_VISITS");

        for r in read(&p).iter().filter(|r| r.get("event").and_then(|e| e.as_str()) == Some("arrive")) {
            assert_eq!(r.get("returning").and_then(|x| x.as_bool()), Some(false));
        }
    }

    /// A question is a stranger's text going into a file read with `jq`. A
    /// newline in it must not become a second record.
    /// What a turn cost lands with the question, and only when something
    /// reported it. `/cert` never reached a model, and "free" is a different
    /// claim from "nobody told us".
    #[test]
    fn what_an_answer_cost_is_recorded_beside_it_when_it_is_known() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let p = scratch("spend");
        std::env::set_var("PORTFOLIO_VISITS", &p);

        let mut v = Visit::open(ssh_visitor("alice", "SHA256:AAA"));
        v.asked(
            "where is jaipur",
            "Rajasthan.",
            Some(crate::acp::Spend { used: 1256, window: 128_000, cost: Some(0.000_74) }),
        );
        // A priced-at-nothing model: the window is known, the money is not.
        v.asked("and kochi", "Kerala.", Some(crate::acp::Spend { used: 1391, window: 128_000, cost: None }));
        v.asked("/cert", "A badge.", None);
        v.close();
        std::env::remove_var("PORTFOLIO_VISITS");

        let rows = read(&p);
        let asks: Vec<&crate::json::Value> = rows
            .iter()
            .filter(|r| r.get("event").and_then(|e| e.as_str()) == Some("ask"))
            .collect();
        assert_eq!(asks.len(), 3);

        assert_eq!(asks[0].get("used").and_then(|u| u.as_f64()), Some(1256.0));
        assert_eq!(asks[0].get("window").and_then(|u| u.as_f64()), Some(128_000.0));
        // Six places, because two would round this one to free.
        let cost = asks[0].get("cost").and_then(|c| c.as_f64()).expect("no cost");
        assert!((cost - 0.000_74).abs() < 1e-9, "{cost}");

        assert_eq!(asks[1].get("used").and_then(|u| u.as_f64()), Some(1391.0));
        assert!(asks[1].get("cost").is_none(), "an unpriced model was given a price");

        assert!(asks[2].get("used").is_none(), "a local command reported tokens");
        assert!(asks[2].get("cost").is_none(), "a local command reported a cost");

        // And every row is still one parseable line.
        assert!(rows.iter().all(|r| r.get("at").is_some()));
    }

    #[test]
    fn a_hostile_question_cannot_forge_a_record() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let p = scratch("hostile");
        std::env::set_var("PORTFOLIO_VISITS", &p);

        let mut v = Visit::open(ssh_visitor("x", "SHA256:X"));
        v.asked(
            "first\n{\"event\":\"arrive\",\"id\":\"forged\"}",
            "and \"quoted\" \\ too",
            None,
        );
        v.close();
        std::env::remove_var("PORTFOLIO_VISITS");

        let rows = read(&p);
        assert_eq!(rows.len(), 3, "a newline became a record: {rows:?}");
        assert!(rows
            .iter()
            .all(|r| r.get("id").and_then(|i| i.as_str()) != Some("forged")));
    }

    #[test]
    fn private_addresses_are_never_sent_to_a_lookup() {
        for p in ["10.0.0.1", "127.0.0.1", "192.168.1.9", "172.16.4.4", "::1", ""] {
            assert!(is_private(p), "{p} would have been looked up");
        }
        for public in ["8.8.8.8", "1.1.1.1", "172.32.0.1"] {
            assert!(!is_private(public), "{public} was treated as private");
        }
    }
}
