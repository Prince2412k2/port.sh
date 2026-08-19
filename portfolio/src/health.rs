//! Which agent tier is actually answering right now.
//!
//! The ask section used to find out the hard way: start the first model in the
//! list, and if it failed mid-question drop to the next. That works, but the
//! visitor pays for it — they ask something, watch nothing happen, and get an
//! answer late from a model that had to be started after theirs failed. The
//! first person to arrive after a quota resets always paid that.
//!
//! So the tiers are checked on a timer instead, in the background, and a
//! session starts on a tier that was answering minutes ago. The lazy fallback
//! in `acp.rs` stays as the second line: a tier can go down between checks,
//! and something has to catch that.
//!
//! **What a check costs.** One real question, of one word, to one model — the
//! cheapest thing that distinguishes "configured" from "answering", which
//! nothing else does. A listed model can be out of quota, unauthenticated, or
//! withdrawn, and every one of those looks fine until you ask. Tiers are tried
//! in order and the walk stops at the first that answers, so the usual hour
//! costs a single one-word prompt.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, RecvTimeoutError};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::json;

/// How long between checks.
pub fn interval() -> Duration {
    Duration::from_secs(
        std::env::var("PORTFOLIO_PROBE_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(3600),
    )
}

/// How long one model gets to answer one word before it is counted as down.
///
/// Generous. A cold model behind a queue is slow rather than broken, and
/// calling it broken would drop a working tier for the next hour.
const PATIENCE: Duration = Duration::from_secs(45);

#[derive(Debug, Clone, Default)]
pub struct Tier {
    pub name: String,
    pub models: Vec<String>,
}

/// The tiers, in the order they should be tried.
pub fn tiers() -> Vec<Tier> {
    let disk = std::fs::read_to_string("portfolio/data/models.txt")
        .or_else(|_| std::fs::read_to_string("data/models.txt"))
        .ok();
    parse(disk.as_deref().unwrap_or(include_str!("../data/models.txt")))
}

pub fn parse(src: &str) -> Vec<Tier> {
    let mut out: Vec<Tier> = Vec::new();
    for line in src.lines() {
        let bare = line.trim();
        if bare.is_empty() || bare.starts_with('#') {
            continue;
        }
        let (word, rest) = bare.split_once(char::is_whitespace).unwrap_or((bare, ""));
        match word {
            "tier" => out.push(Tier { name: rest.trim().to_string(), models: Vec::new() }),
            "model" => {
                let m = rest.trim().to_string();
                if m.is_empty() {
                    continue;
                }
                // A model before any tier still belongs somewhere. Naming the
                // tier after nothing is better than dropping the model.
                if out.is_empty() {
                    out.push(Tier { name: "default".into(), models: Vec::new() });
                }
                out.last_mut().expect("just pushed").models.push(m);
            }
            _ => {}
        }
    }
    out.retain(|t| !t.models.is_empty());
    out
}

/// What the last check concluded.
#[derive(Debug, Clone, Default)]
pub struct State {
    /// The tier that answered, and the models in it.
    pub tier: Option<Tier>,
    pub checked: bool,
}

fn state() -> &'static Mutex<State> {
    static S: OnceLock<Mutex<State>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(State::default()))
}

/// The models a new session should try, best first.
///
/// Before the first check has finished this is every model in the file, in
/// order — which is exactly the old behaviour, and the right answer for a
/// session that arrives in the first few seconds after a restart.
pub fn models() -> Vec<String> {
    let s = state().lock().unwrap_or_else(|e| e.into_inner());
    match (&s.tier, s.checked) {
        (Some(t), _) => t.models.clone(),
        // Checked, and nothing answered. Hand back everything anyway rather
        // than nothing: the check is a minute old at worst and a tier that
        // just came back should not have to wait an hour to be used.
        (None, _) => tiers().into_iter().flat_map(|t| t.models).collect(),
    }
}

/// A line for the ask section to show while it waits.
pub fn note() -> Option<String> {
    let s = state().lock().unwrap_or_else(|e| e.into_inner());
    s.tier.as_ref().map(|t| t.name.clone())
}

/// Start checking, now and then every `interval`.
///
/// Called once by whichever transport is serving. Sessions never call this;
/// they read `models()`, which is fine before the first check finishes.
pub fn watch() {
    if tiers().is_empty() {
        eprintln!("portfolio: no models configured, the ask section is off");
        return;
    }
    std::thread::spawn(|| loop {
        check();
        std::thread::sleep(interval());
    });
}

/// One pass: find the first tier with a model that answers.
pub fn check() {
    let mut tried = Vec::new();
    for t in tiers() {
        let live: Vec<String> = t.models.iter().filter(|m| ask_one_word(m)).cloned().collect();
        if !live.is_empty() {
            eprintln!(
                "portfolio: agent tier `{}` answering ({}/{} models)",
                t.name,
                live.len(),
                t.models.len()
            );
            let mut s = state().lock().unwrap_or_else(|e| e.into_inner());
            *s = State { tier: Some(Tier { name: t.name, models: live }), checked: true };
            return;
        }
        tried.push(t.name);
    }
    eprintln!("portfolio: no agent tier answered ({})", tried.join(", "));
    let mut s = state().lock().unwrap_or_else(|e| e.into_inner());
    *s = State { tier: None, checked: true };
}

/// Kill a child and reap it, so a failed probe does not leave a process behind.
///
/// Every hour, for ever: a probe that leaks one process an hour fills the
/// container's pid limit in about ten days and takes the whole site with it.
fn reap(mut c: Child) {
    let _ = c.kill();
    let _ = c.wait();
}

/// Ask one model one word. True if it answered at all.
fn ask_one_word(model: &str) -> bool {
    let Ok(mut child) = Command::new("opencode")
        .arg("acp")
        .env("OPENCODE_CONFIG_CONTENT", crate::acp::config(model))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };

    let Some(stdout) = child.stdout.take() else {
        reap(child);
        return false;
    };
    let (tx, rx) = channel::<json::Value>();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if let Some(v) = json::parse(line.trim()) {
                if tx.send(v).is_err() {
                    return;
                }
            }
        }
    });

    let Some(mut w) = child.stdin.take() else {
        reap(child);
        return false;
    };
    let deadline = Instant::now() + PATIENCE;

    // The same handshake acp.rs performs, minus everything the answer is for.
    let send = |w: &mut std::process::ChildStdin, s: &str| {
        w.write_all(s.as_bytes()).is_ok() && w.write_all(b"\n").is_ok() && w.flush().is_ok()
    };
    let wait = |id: i64, deadline: Instant| -> Option<json::Value> {
        loop {
            let left = deadline.checked_duration_since(Instant::now())?;
            match rx.recv_timeout(left) {
                Ok(v) => {
                    if v.get("id").and_then(|i| i.as_f64()) == Some(id as f64) {
                        if v.get("error").is_some() {
                            return None;
                        }
                        return v.get("result").cloned().or(Some(json::Value::Null));
                    }
                }
                Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => return None,
            }
        }
    };

    let ok = (|| {
        if !send(&mut w, concat!(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1,"#,
            r#""clientCapabilities":{"fs":{"readTextFile":false,"writeTextFile":false}}}}"#
        )) {
            return false;
        }
        if wait(1, deadline).is_none() {
            return false;
        }
        let cwd = std::env::current_dir().unwrap_or_default();
        if !send(&mut w, &format!(
            r#"{{"jsonrpc":"2.0","id":2,"method":"session/new","params":{{"cwd":{},"mcpServers":[]}}}}"#,
            json::quote(&cwd.to_string_lossy())
        )) {
            return false;
        }
        let Some(res) = wait(2, deadline) else { return false };
        let Some(sid) = res.get("sessionId").and_then(|v| v.as_str()).map(str::to_string) else {
            return false;
        };
        // Plan mode here too. A probe that runs in a mode the real sessions
        // never use is not a probe of the thing being asked about, and this is
        // also where a model that refuses plan mode gets caught.
        if !send(&mut w, &format!(
            r#"{{"jsonrpc":"2.0","id":3,"method":"session/set_mode","params":{{"sessionId":{},"modeId":"plan"}}}}"#,
            json::quote(&sid)
        )) || wait(3, deadline).is_none()
        {
            return false;
        }
        // The actual question. One word, and the answer is thrown away: what
        // is being tested is that a token came back at all.
        if !send(&mut w, &format!(
            r#"{{"jsonrpc":"2.0","id":4,"method":"session/prompt","params":{{"sessionId":{},"prompt":[{{"type":"text","text":"ping"}}]}}}}"#,
            json::quote(&sid)
        )) {
            return false;
        }
        wait(4, deadline).is_some()
    })();

    reap(child);
    ok
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shipped_file_parses_into_ordered_tiers() {
        let t = tiers();
        assert!(t.len() >= 3, "{t:?}");
        assert_eq!(t[0].name, "github copilot");
        for tier in &t {
            assert!(!tier.models.is_empty(), "{} is empty", tier.name);
            for m in &tier.models {
                assert!(m.contains('/'), "{m} is not provider/model");
            }
        }
    }

    #[test]
    fn comments_blanks_and_a_stray_model_are_all_handled() {
        let t = parse(
            "# a comment\n\n  model orphan/one\ntier  second\n  model a/b\n  model c/d\n\n",
        );
        assert_eq!(t.len(), 2);
        // A model before any tier is kept rather than dropped on the floor.
        assert_eq!(t[0].name, "default");
        assert_eq!(t[0].models, vec!["orphan/one"]);
        assert_eq!(t[1].name, "second");
        assert_eq!(t[1].models, vec!["a/b", "c/d"]);
    }

    /// A tier with a name and no models is a heading somebody left behind, and
    /// selecting it would leave the section with nothing to call.
    #[test]
    fn an_empty_tier_is_dropped() {
        let t = parse("tier empty\ntier real\n  model a/b\n");
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].name, "real");
    }

    /// Before any check has run, every model is on the table — a session that
    /// arrives in the first seconds after a restart must not find an empty
    /// list and report the agent as down.
    #[test]
    fn models_before_the_first_check_are_everything_in_order() {
        let all: Vec<String> = tiers().into_iter().flat_map(|t| t.models).collect();
        let m = models();
        assert_eq!(m, all);
        assert!(!m.is_empty());
    }
}
