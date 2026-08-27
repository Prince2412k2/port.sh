//! Leaving Prince a message.
//!
//! One append-only JSONL file. No SMTP, no webhook, no third party: a portfolio
//! that posts strangers' text to somebody's chat as a side effect of being
//! visited is a spam relay with a nice font, and the moment it needs a secret
//! to do that, the secret is on a public box.
//!
//! Append-only and O_APPEND, so two sessions writing at once interleave whole
//! lines rather than corrupting each other's. One JSON document per line means
//! a half-written file still reads: `tail -f` it, or `jq .` it, and a truncated
//! last line costs the last message rather than the lot.
//!
//! The container's filesystem is read-only, so the directory this lives in is
//! the one writable mount in the whole deployment. If it is missing -- run
//! locally, or the volume was not mounted -- saying so beats pretending the
//! message went somewhere.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::json;

/// The most a stranger may leave in one message.
///
/// Generous for a note and mean for a payload. The file is something Prince
/// reads by hand, and the failure worth preventing is one visitor filling it.
pub const MAX_LEN: usize = 1200;

/// Where the messages go.
pub fn path() -> PathBuf {
    if let Ok(p) = std::env::var("PORTFOLIO_MESSAGES") {
        return PathBuf::from(p);
    }
    PathBuf::from("data/messages.jsonl")
}

/// Serialises the tests that set PORTFOLIO_MESSAGES.
///
/// Environment variables are per-process and the test runner is threaded, so
/// two tests pointing this at different files will trip over each other -- and
/// only sometimes, which is the worst way to find out. Every test that touches
/// the variable takes this first.
#[cfg(test)]
pub static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Roughly where a visitor came from, for the record.
///
/// Not identity and not meant to be: SSH_CONNECTION is set by the transport
/// rather than by anything the visitor typed, which is the only reason it is
/// worth writing down at all.
pub fn origin() -> String {
    match std::env::var("SSH_CONNECTION") {
        Ok(v) => v.split_whitespace().next().unwrap_or("ssh").to_string(),
        Err(_) => "web".to_string(),
    }
}

/// What happened, in words meant for the person who typed it.
pub enum Sent {
    Ok,
    Empty,
    TooLong(usize),
    /// Could not be saved. Carries nothing: the reason is already in the
    /// container's log, and there is nothing the visitor could do with it.
    Unwritable,
}

/// Append one message. `who` may be empty; `from` is the transport's idea of
/// where the visitor came from, not anything they typed.
pub fn leave(who: &str, body: &str, from: &str) -> Sent {
    let body = body.trim();
    if body.is_empty() {
        return Sent::Empty;
    }
    if body.chars().count() > MAX_LEN {
        return Sent::TooLong(body.chars().count());
    }

    let at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Built through json::quote rather than by pasting text between quotes:
    // this is a stranger's input going into a file Prince will read with jq,
    // and a newline in it must not become a second record.
    let line = format!(
        r#"{{"at":{at},"who":{},"from":{},"message":{}}}"#,
        json::quote(who.trim()),
        json::quote(from),
        json::quote(body)
    );

    let p = path();
    if let Some(dir) = p.parent() {
        if !dir.as_os_str().is_empty() && !dir.exists() {
            crate::note!("portfolio: {} does not exist, message not saved", dir.display());
            return Sent::Unwritable;
        }
    }
    match OpenOptions::new().create(true).append(true).open(&p) {
        Ok(mut f) => match writeln!(f, "{line}") {
            Ok(()) => Sent::Ok,
            Err(e) => unwritable(&p, e),
        },
        Err(e) => unwritable(&p, e),
    }
}

/// Report a message that could not be saved.
///
/// The visitor is told plainly that it did not save and pointed at the email
/// address; the reason goes to stderr, which is the container's log. They
/// cannot act on an errno and the operator cannot act on anything else --
/// and a message box that has quietly stopped accepting messages is exactly
/// the failure worth being loud about.
fn unwritable(p: &std::path::Path, e: std::io::Error) -> Sent {
    crate::note!("portfolio: cannot write {}: {e}", p.display());
    Sent::Unwritable
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One test, not three. They all set PORTFOLIO_MESSAGES, the test runner
    /// is threaded, and environment variables are per-process -- three of
    /// these racing produced a failure that only showed up on a loaded
    /// machine. That has already cost this project an afternoon once.
    #[test]
    fn messages_are_appended_as_one_record_each_and_bad_ones_refused() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("reach-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("m.jsonl");
        std::env::set_var("PORTFOLIO_MESSAGES", &file);

        // The one that matters: a stranger's text must not be able to end its
        // own record and start another, or forge a timestamp.
        let nasty = "hi\"}\n{\"at\":0,\"message\":\"forged\nsecond line";
        assert!(matches!(leave("me\"x", nasty, "ssh"), Sent::Ok));
        assert!(matches!(leave("", "a second note", "web"), Sent::Ok));

        let text = std::fs::read_to_string(&file).unwrap();
        assert_eq!(text.lines().count(), 2, "a message broke out of its record");

        let v = json::parse(text.lines().next().unwrap()).expect("not valid JSON");
        assert_eq!(v.get("message").and_then(|m| m.as_str()), Some(nasty));
        assert_eq!(v.get("who").and_then(|m| m.as_str()), Some("me\"x"));
        assert_eq!(v.get("from").and_then(|m| m.as_str()), Some("ssh"));

        assert!(matches!(leave("", "   ", "web"), Sent::Empty));
        let long = "a".repeat(MAX_LEN + 1);
        assert!(matches!(leave("", &long, "web"), Sent::TooLong(_)));

        // A missing directory is reported rather than swallowed, so the
        // visitor is told their message did not go anywhere.
        std::env::set_var("PORTFOLIO_MESSAGES", "/nope/definitely/not/here/m.jsonl");
        assert!(matches!(leave("", "hello", "ssh"), Sent::Unwritable));

        std::env::remove_var("PORTFOLIO_MESSAGES");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
