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
use std::process::Child;
use std::sync::mpsc::{channel, RecvTimeoutError};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::json;
use crate::servers::Server;

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
    /// Which ACP server this tier's models are reached through. Per tier rather
    /// than global, because "the same question, somewhere else" is exactly what
    /// a tier is -- and a fallback tier that has to run a different server is
    /// the case this whole indirection exists for.
    pub server: Server,
}

/// One thing to try: a model, the server that runs it, and the tier it came
/// from. Flattened for the caller, which wants a list to walk rather than a
/// tree to recurse.
#[derive(Debug, Clone)]
pub struct Shot {
    pub tier: String,
    pub server: Server,
    pub model: String,
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
        // A directive before any tier still belongs somewhere. Naming the tier
        // after nothing is better than dropping the line.
        let current = |out: &mut Vec<Tier>| {
            if out.is_empty() {
                out.push(Tier { name: "default".into(), ..Tier::default() });
            }
        };
        match word {
            "tier" => out.push(Tier { name: rest.trim().to_string(), ..Tier::default() }),
            "command" if !rest.trim().is_empty() => {
                current(&mut out);
                out.last_mut().expect("just pushed").server.command_line(rest.trim());
            }
            "pin" if !rest.trim().is_empty() => {
                current(&mut out);
                out.last_mut().expect("just pushed").server.pin_line(rest.trim());
            }
            "tools" if !rest.trim().is_empty() => {
                current(&mut out);
                out.last_mut().expect("just pushed").server.tools_line(rest.trim());
            }
            "secrets" if !rest.trim().is_empty() => {
                current(&mut out);
                out.last_mut().expect("just pushed").server.secrets_line(rest.trim());
            }
            "option" if !rest.trim().is_empty() => {
                current(&mut out);
                out.last_mut().expect("just pushed").server.option_line(rest.trim());
            }
            "model" => {
                let m = rest.trim().to_string();
                if m.is_empty() {
                    continue;
                }
                current(&mut out);
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
    /// The tier that answered, and the models in it. `None` until a check has
    /// found one, and again if one stops answering.
    pub tier: Option<Tier>,
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
pub fn plan() -> Vec<Shot> {
    let chosen = {
        let s = state().lock().unwrap_or_else(|e| e.into_inner());
        s.tier.clone()
    };
    // The chosen tier first, and then the others behind it.
    //
    // This used to hand back the chosen tier alone, which quietly defeated the
    // lazy fallback this module's header promises: a tier that answered the
    // hourly check and then stopped answering left `acp.rs` with nothing to try
    // next, and the section reported that every model had refused when two
    // untried tiers were sitting in the file. Copilot revalidating its login
    // against a network it cannot always reach is exactly that case.
    //
    // Checked and nothing answered still hands back everything, for the same
    // reason as before: the check is an hour old at worst.
    let all = tiers();
    let list = match chosen {
        Some(t) => {
            let mut order = vec![t.clone()];
            order.extend(all.into_iter().filter(|o| o.name != t.name));
            order
        }
        None => all,
    };
    list.into_iter()
        .flat_map(|t| {
            let (name, server) = (t.name, t.server);
            t.models.into_iter().map(move |model| Shot {
                tier: name.clone(),
                server: server.clone(),
                model,
            })
        })
        .collect()
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
        let live: Vec<String> =
            t.models.iter().filter(|m| ask_one_word(&t.server, m)).cloned().collect();
        if !live.is_empty() {
            eprintln!(
                "portfolio: agent tier `{}` answering via `{}` ({}/{} models)",
                t.name,
                t.server.label(),
                live.len(),
                t.models.len()
            );
            let mut s = state().lock().unwrap_or_else(|e| e.into_inner());
            *s = State {
                tier: Some(Tier { name: t.name, models: live, server: t.server }),
            };
            return;
        }
        tried.push(t.name);
    }
    eprintln!("portfolio: no agent tier answered ({})", tried.join(", "));
    let mut s = state().lock().unwrap_or_else(|e| e.into_inner());
    *s = State { tier: None };
}

/// Kill a child and reap it, so a failed probe does not leave a process behind.
///
/// Every hour, for ever: a probe that leaks one process an hour fills the
/// container's pid limit in about ten days and takes the whole site with it.
fn reap(mut c: Child) {
    let _ = c.kill();
    let _ = c.wait();
}

/// Ask one model one word, through the server its tier names. True if it
/// answered at all.
fn ask_one_word(server: &Server, model: &str) -> bool {
    let Ok(mut child) = server.spawn_command(model).spawn() else {
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
        // The same handshake acp.rs performs, built by the same functions --
        // a probe that negotiates differently is not a probe of the thing being
        // asked about. That used to be a comment asking the reader to keep two
        // copies in step; now there is one copy.
        if !send(&mut w, &crate::acp::initialize_request(1)) {
            return false;
        }
        let Some(hello) = wait(1, deadline) else {
            return false;
        };
        // What this agent says it needs before it will work, kept for the one
        // failure that is worth explaining.
        let methods = crate::acp::auth_methods(&hello);
        // The check offers the same tool server a real session does, and this is
        // the whole reason it does.
        //
        // It used to send `mcpServers: []` on the grounds that a probe has no
        // screen to draw on. That was true and it was the wrong call: when the
        // payload turned out to be malformed -- the http variant requires
        // `headers` and it was missing -- every real session died on `Invalid
        // params` while this check, alone in sending no server, went on
        // reporting the tier as healthy. A probe that negotiates differently
        // from the thing it is probing is not a probe of it.
        //
        // The token names no registered page on purpose. Nothing here calls a
        // tool; what is being tested is whether the agent will *accept* being
        // handed one.
        let tools = crate::acp::takes_http_tools(&hello)
            .then(|| crate::mcp::url_for("health-check-no-page"))
            .flatten();
        if !send(&mut w, &crate::acp::session_new_request(2, tools.as_deref())) {
            return false;
        }
        let mut opened = wait(2, deadline);
        // Refused with tools attached: say so loudly and carry on without them,
        // which is what `acp.rs` does for a real session. A tier that works only
        // without our tools is a working tier and a broken feature, and those
        // are worth telling apart in the log.
        if opened.is_none() && tools.is_some() {
            eprintln!(
                "portfolio: `{}` will not take our tool server -- the agent works, \
                 the map does not. Check the mcpServers payload against ACP's schema.",
                server.label()
            );
            if !send(&mut w, &crate::acp::session_new_request(6, None)) {
                return false;
            }
            opened = wait(6, deadline);
        }
        let Some(res) = opened else {
            // Copilot refuses to open a session at all until it is logged in,
            // and a probe that answers "no tier answered" to a machine that has
            // simply never been authenticated has told the operator nothing.
            // The agent already said what would fix it, so say that.
            if let Some(m) = methods.first() {
                let hint = if m.description.is_empty() { &m.name } else { &m.description };
                eprintln!("portfolio: `{}` needs authenticating -- {hint}", server.label());
            }
            return false;
        };
        let Some(sid) = res.get("sessionId").and_then(|v| v.as_str()).map(str::to_string) else {
            return false;
        };
        // Restrained here too, and this is also where a model that offers a
        // mode and then refuses to enter it gets caught.
        if let Some(r) = crate::acp::restraint(&res) {
            if !send(&mut w, &r.request(3, &sid)) || wait(3, deadline).is_none() {
                return false;
            }
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
        // Which tier leads is the operator's choice and lives in the file; what
        // is checked here is that parsing preserves the order it is written in.
        let src = std::fs::read_to_string("portfolio/data/models.txt")
            .or_else(|_| std::fs::read_to_string("data/models.txt"))
            .unwrap_or_else(|_| include_str!("../data/models.txt").to_string());
        let written: Vec<String> = src
            .lines()
            .filter_map(|l| l.trim().strip_prefix("tier ").map(|n| n.trim().to_string()))
            .collect();
        let parsed: Vec<String> = t.iter().map(|x| x.name.clone()).collect();
        // A tier with no models is dropped, so the parsed list is a subsequence
        // of what is written rather than equal to it.
        assert!(
            parsed.iter().all(|n| written.contains(n)),
            "parsed a tier that is not in the file: {parsed:?} vs {written:?}"
        );
        let order: Vec<usize> =
            parsed.iter().filter_map(|n| written.iter().position(|w| w == n)).collect();
        assert!(order.windows(2).all(|w| w[0] < w[1]), "the file's order was not kept");
        for tier in &t {
            assert!(!tier.models.is_empty(), "{} is empty", tier.name);
            for m in &tier.models {
                assert!(!m.is_empty(), "{} has a blank model", tier.name);
                // `provider/model` is opencode's naming, not a rule. Copilot
                // takes plain names -- `auto`, `claude-sonnet-4.5` -- so the
                // check applies where the convention does and nowhere else.
                if tier.server.pin == crate::servers::Pin::OpencodeConfig {
                    assert!(m.contains('/'), "{m} is not provider/model");
                }
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
    fn the_plan_before_the_first_check_is_everything_in_order() {
        let _held = PLAN_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let all: Vec<String> = tiers().into_iter().flat_map(|t| t.models).collect();
        let got: Vec<String> = plan().into_iter().map(|s| s.model).collect();
        assert_eq!(got, all);
        assert!(!got.is_empty());
    }

    /// Every shot carries the server its tier named, so nothing downstream has
    /// to fall back to a hard-coded command.
    #[test]
    fn every_shot_knows_which_server_runs_it() {
        for s in plan() {
            assert!(!s.server.label().is_empty(), "{} has no server", s.model);
            assert!(!s.tier.is_empty(), "{} has no tier", s.model);
        }
    }

    #[test]
    fn a_tier_can_name_its_own_acp_server() {
        let t = parse(
            "tier local\n  command  claude-code-acp --stdio\n  pin  flag --model\n  model a/b\n",
        );
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].server.label(), "claude-code-acp");
        assert_eq!(t[0].server.pin, crate::servers::Pin::Flag("--model".into()));
    }

    /// Serialises the two tests that read or write the chosen tier.
    ///
    /// It is one global for the whole process and the runner is threaded, so a
    /// test that sets it and a test that expects it unset are a coin toss --
    /// which is how this suite spent a while failing only sometimes, and only
    /// when the whole of it ran. Same shape as `visits::ENV_LOCK`.
    static PLAN_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// The chosen tier leads, and the rest queue up behind it. Returning only
    /// the chosen one silently removed the fallback this module promises, and a
    /// tier that stops answering between checks took the whole section with it.
    #[test]
    fn a_chosen_tier_is_tried_first_and_the_others_are_still_there() {
        let _held = PLAN_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let all = tiers();
        assert!(all.len() >= 2, "this test needs more than one tier configured");
        let second = all[1].clone();
        {
            let mut s = state().lock().unwrap_or_else(|e| e.into_inner());
            *s = State { tier: Some(second.clone()) };
        }
        let got = plan();
        {
            let mut s = state().lock().unwrap_or_else(|e| e.into_inner());
            *s = State::default();
        }
        assert_eq!(got[0].tier, second.name, "the chosen tier did not lead");
        let names: Vec<&str> = got.iter().map(|s| s.tier.as_str()).collect();
        for t in &all {
            assert!(names.contains(&t.name.as_str()), "{} has nothing to fall back to", t.name);
        }
        // And it appears once, not twice.
        assert_eq!(names.iter().filter(|n| **n == second.name).count(), second.models.len());
    }

    /// A tier that names no server is the shipped file's shape, and must keep
    /// working exactly as it did.
    #[test]
    fn a_tier_naming_no_server_still_gets_opencode() {
        let t = parse("tier default\n  model a/b\n");
        assert_eq!(t[0].server.label(), "opencode");
        assert_eq!(t[0].server.pin, crate::servers::Pin::OpencodeConfig);
    }
}
