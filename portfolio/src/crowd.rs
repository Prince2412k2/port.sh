//! How many of a visitor there are, and how fast they are arriving.
//!
//! Both transports need the same two answers and used to have one of them, in
//! `net.rs`, written against SSH. The web side had a global ceiling and nothing
//! else -- so one address could hold all hundred and twenty-eight browser
//! sessions and the box would be full while being read by nobody.
//!
//! Neither of these is a substitute for the other. A concurrency limit is
//! "how much is this visitor holding right now", and it is released when they
//! leave; a rate limit is "how often are they turning up", and it is not.
//! Something that connects and disconnects in a loop never holds two of
//! anything and is still a load.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How an address is counted.
///
/// A v4 address is the address. A v6 address is its **/64**, because a visitor
/// is not given one v6 address, they are given a whole /64 -- counting single
/// addresses there would mean a limit anybody can step around by picking the
/// next number in their own subnet, which is a rule that inconveniences only
/// the people not trying to get around it.
///
/// Loopback is exempt, and returns `None`: it is the operator, the health
/// check and the smoke test, and a rule that locks you out of your own box
/// halfway through a deploy is a rule that gets switched off at the worst
/// possible moment.
pub fn key(ip: IpAddr) -> Option<String> {
    if ip.is_loopback() {
        return None;
    }
    Some(match ip {
        IpAddr::V4(v4) => v4.to_string(),
        IpAddr::V6(v6) => {
            let s = v6.segments();
            format!("{:x}:{:x}:{:x}:{:x}::/64", s[0], s[1], s[2], s[3])
        }
    })
}

/// Who is connected, by address.
///
/// Separate from any global counter because they answer different questions:
/// that one is "is this box full", this one is "is this visitor already here".
/// A seat holds both, so neither can be released without the other.
#[derive(Default)]
pub struct Crowd {
    pub(crate) held: Mutex<HashMap<String, usize>>,
}

impl Crowd {
    /// Take a seat for this address, or say no.
    pub fn take(&self, key: &str, limit: usize) -> bool {
        let mut held = self.held.lock().unwrap_or_else(|e| e.into_inner());
        let n = held.entry(key.to_string()).or_insert(0);
        if *n >= limit {
            return false;
        }
        *n += 1;
        true
    }

    pub fn give_back(&self, key: &str) {
        let mut held = self.held.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(n) = held.get_mut(key) {
            *n = n.saturating_sub(1);
            // Removed at zero rather than left behind. The map is keyed by
            // something a stranger chooses, so entries that are never cleaned
            // up are a slow leak somebody else decides the size of.
            if *n == 0 {
                held.remove(key);
            }
        }
    }
}

/// A held seat, given back whichever way the holder leaves.
///
/// A guard rather than a decrement at the end of the handler. A panic inside a
/// session would skip the decrement and leak the slot for the life of the
/// process -- invisible in a global count, and for one address it means that
/// visitor can never come back.
pub struct Seat {
    crowd: std::sync::Arc<Crowd>,
    key: Option<String>,
}

impl Seat {
    /// Take one, or `None` if this address is already holding its share.
    ///
    /// A `None` key is loopback, which is exempt and has nothing to release --
    /// see `key`.
    pub fn take(crowd: &std::sync::Arc<Crowd>, key: Option<String>, limit: usize) -> Option<Seat> {
        if let Some(key) = &key {
            if !crowd.take(key, limit) {
                return None;
            }
        }
        Some(Seat { crowd: std::sync::Arc::clone(crowd), key })
    }
}

impl Drop for Seat {
    fn drop(&mut self) {
        if let Some(key) = &self.key {
            self.crowd.give_back(key);
        }
    }
}

/// How often an address is turning up.
///
/// A sliding window of arrival times per address, rather than a counter that
/// resets on the minute: a fixed window lets twice the limit through across the
/// boundary, which for a limit small enough to be worth having is most of the
/// point of it.
///
/// The timestamps are the storage, which bounds itself -- a key holds at most
/// `limit` of them, and one that stops arriving is dropped on the next sweep
/// rather than kept forever. The map is keyed by something a stranger chooses,
/// so that last part is not tidiness.
pub struct Meter {
    window: Duration,
    limit: usize,
    seen: Mutex<HashMap<String, Vec<Instant>>>,
}

impl Meter {
    pub fn new(limit: usize, window: Duration) -> Meter {
        Meter { window, limit, seen: Mutex::new(HashMap::new()) }
    }

    /// Count one arrival from this address, and say whether it is allowed.
    ///
    /// Refusals are not counted. A limit that counts its own refusals is one
    /// that anybody who keeps knocking locks themselves out of for as long as
    /// they keep knocking, which sounds like a feature until it happens to a
    /// browser with a reconnecting tab and a visitor who has gone to lunch.
    pub fn allow(&self, key: &str, now: Instant) -> bool {
        let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        let cutoff = now.checked_sub(self.window);

        // Everything that has aged out, everywhere -- not only for this key.
        // Sweeping one key would leave every address that ever arrived once in
        // the map for the life of the process.
        if let Some(cutoff) = cutoff {
            seen.retain(|_, at| {
                at.retain(|t| *t > cutoff);
                !at.is_empty()
            });
        }

        let at = seen.entry(key.to_string()).or_default();
        if at.len() >= self.limit {
            return false;
        }
        at.push(now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_v6_visitor_is_their_subnet_and_not_their_address() {
        let of = |s: &str| key(s.parse().unwrap()).unwrap();
        assert_eq!(of("2001:db8:1:2::1"), of("2001:db8:1:2:ffff:ffff:ffff:ffff"));
        assert_ne!(of("2001:db8:1:2::1"), of("2001:db8:1:3::1"));
        assert_eq!(of("203.0.113.7"), "203.0.113.7");
        // The operator is not a crowd.
        assert_eq!(key("127.0.0.1".parse().unwrap()), None);
        assert_eq!(key("::1".parse().unwrap()), None);
    }

    #[test]
    fn arrivals_are_counted_over_a_window_that_slides() {
        let m = Meter::new(3, Duration::from_secs(60));
        let t0 = Instant::now();

        for i in 0..3 {
            assert!(m.allow("a", t0 + Duration::from_secs(i)), "refused arrival {i}");
        }
        assert!(!m.allow("a", t0 + Duration::from_secs(3)), "a fourth got through");

        // One address does not spend another's allowance.
        assert!(m.allow("b", t0 + Duration::from_secs(3)), "one address blocked another");

        // The window slides rather than resetting. At t0+61 the cutoff is
        // t0+1, so the arrivals at t0 and t0+1 have aged out and the one at
        // t0+2 has not: two are owed back, and a third is still refused.
        assert!(m.allow("a", t0 + Duration::from_secs(61)), "nothing aged out");
        assert!(m.allow("a", t0 + Duration::from_secs(61)), "only one of the two aged out");
        assert!(!m.allow("a", t0 + Duration::from_secs(61)), "the window reset instead of sliding");
    }

    /// Refusing somebody must not extend how long they are refused for.
    #[test]
    fn knocking_while_refused_does_not_move_the_window() {
        let m = Meter::new(1, Duration::from_secs(10));
        let t0 = Instant::now();
        assert!(m.allow("a", t0));
        for i in 1..9 {
            assert!(!m.allow("a", t0 + Duration::from_secs(i)));
        }
        // The one arrival that counted was at t0, so it ages out at t0 + 10 --
        // no matter how many refusals landed in between.
        assert!(m.allow("a", t0 + Duration::from_secs(11)), "the refusals held the door shut");
    }

    #[test]
    fn a_seat_is_given_back_however_its_holder_leaves() {
        let crowd = std::sync::Arc::new(Crowd::default());
        let one = Seat::take(&crowd, Some("a".into()), 2).expect("refused the first");
        let two = Seat::take(&crowd, Some("a".into()), 2).expect("refused the second");
        assert!(Seat::take(&crowd, Some("a".into()), 2).is_none(), "three fitted in two");
        drop(one);
        assert!(Seat::take(&crowd, Some("a".into()), 2).is_some(), "a seat was not given back");
        drop(two);

        // Loopback holds nothing and is never refused.
        let _ = Seat::take(&crowd, None, 0).expect("the operator was refused");
        let _ = Seat::take(&crowd, None, 0).expect("the operator was refused twice");
    }

    /// A panic in a session must not lock that address out for good.
    #[test]
    fn a_panic_does_not_keep_the_seat() {
        let crowd = std::sync::Arc::new(Crowd::default());
        let c = std::sync::Arc::clone(&crowd);
        let _ = std::thread::spawn(move || {
            let _held = Seat::take(&c, Some("a".into()), 1).expect("refused the first");
            panic!("session died");
        })
        .join();
        assert!(crowd.held.lock().unwrap().is_empty(), "the address slot leaked");
        assert!(Seat::take(&crowd, Some("a".into()), 1).is_some(), "they could not come back");
    }

    /// An address that stops arriving stops being remembered.
    #[test]
    fn the_map_does_not_grow_with_everyone_who_ever_visited() {
        let m = Meter::new(2, Duration::from_secs(30));
        let t0 = Instant::now();
        for i in 0..500 {
            assert!(m.allow(&format!("visitor-{i}"), t0));
        }
        assert_eq!(m.seen.lock().unwrap().len(), 500);
        // One arrival well after all of them, which sweeps the rest out.
        assert!(m.allow("late", t0 + Duration::from_secs(31)));
        assert_eq!(
            m.seen.lock().unwrap().len(),
            1,
            "five hundred addresses stayed in the map after their window passed"
        );
    }
}
