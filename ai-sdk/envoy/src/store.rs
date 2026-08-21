//! Conversations on disk.
//!
//! One file per session, one message per line. The format is chosen for being
//! repairable: a truncated write costs the last message rather than the file,
//! and a line that will not parse can be deleted with an editor. A session is
//! appended to as it grows, so an interrupted process leaves everything up to
//! its last completed turn.
//!
//! This is what makes `loadSession`, `session/resume` and `session/fork`
//! honest. Without a store they are advertised as absent, because a client that
//! is told it can resume and then cannot has been lied to in a way it acts on.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use parley::types::Message;

pub struct Store {
    dir: PathBuf,
}

impl Store {
    /// Open, creating the directory if it is not there.
    pub fn open(dir: impl Into<PathBuf>) -> std::io::Result<Store> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir)?;
        Ok(Store { dir })
    }

    fn path(&self, id: &str) -> PathBuf {
        // Session ids come from us, but a `session/load` carries one from the
        // client. Anything that could climb out of the directory is refused
        // rather than sanitised, because a quietly rewritten id reads the wrong
        // conversation back.
        self.dir.join(format!("{id}.jsonl"))
    }

    pub fn usable(id: &str) -> bool {
        !id.is_empty()
            && id.len() <= 128
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    }

    pub fn append(&self, id: &str, messages: &[Message]) -> std::io::Result<()> {
        if !Store::usable(id) {
            return Err(std::io::Error::other(format!("unusable session id `{id}`")));
        }
        if messages.is_empty() {
            return Ok(());
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.path(id))?;
        for message in messages {
            let line = serde_json::to_string(message).map_err(std::io::Error::other)?;
            writeln!(file, "{line}")?;
        }
        file.flush()
    }

    pub fn read(&self, id: &str) -> std::io::Result<Vec<Message>> {
        if !Store::usable(id) {
            return Err(std::io::Error::other(format!("unusable session id `{id}`")));
        }
        let file = std::fs::File::open(self.path(id))?;
        let mut messages = Vec::new();
        for line in BufReader::new(file).lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str(&line) {
                Ok(message) => messages.push(message),
                // A line we cannot read is the interesting case: a half-written
                // last line from a killed process. Losing it is right; losing
                // the conversation is not.
                Err(e) => eprintln!("envoy: {}: skipping a bad line: {e}", id),
            }
        }
        Ok(messages)
    }

    pub fn exists(&self, id: &str) -> bool {
        Store::usable(id) && self.path(id).is_file()
    }

    pub fn list(&self) -> std::io::Result<Vec<String>> {
        let mut ids = Vec::new();
        for entry in std::fs::read_dir(&self.dir)? {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    ids.push(stem.to_string());
                }
            }
        }
        ids.sort();
        Ok(ids)
    }

    pub fn remove(&self, id: &str) -> std::io::Result<()> {
        if !Store::usable(id) {
            return Ok(());
        }
        match std::fs::remove_file(self.path(id)) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            other => other,
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parley::types::{Assistant, Block, Stop};

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("envoy-store-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn conversation() -> Vec<Message> {
        vec![
            Message::user("where is Jaipur"),
            Message::Assistant(Assistant {
                content: vec![
                    Block::text("checking"),
                    Block::ToolCall {
                        id: "c1".into(),
                        name: "locate_place".into(),
                        args: serde_json::json!({"name": "Jaipur"}),
                    },
                ],
                stop: Stop::ToolUse,
                ..Assistant::pending()
            }),
            Message::ToolResult {
                call_id: "c1".into(),
                name: "locate_place".into(),
                content: vec![Block::text("26.9,75.8")],
                error: false,
            },
        ]
    }

    #[test]
    fn a_conversation_round_trips_with_its_tool_calls_intact() {
        let store = Store::open(scratch("round")).unwrap();
        store.append("abc", &conversation()).unwrap();
        let read = store.read("abc").unwrap();
        assert_eq!(read, conversation());
    }

    #[test]
    fn appending_twice_continues_the_same_conversation() {
        let store = Store::open(scratch("append")).unwrap();
        store.append("abc", &[Message::user("one")]).unwrap();
        store.append("abc", &[Message::user("two")]).unwrap();
        assert_eq!(store.read("abc").unwrap().len(), 2);
    }

    #[test]
    fn a_half_written_last_line_costs_one_message_not_the_file() {
        let dir = scratch("torn");
        let store = Store::open(&dir).unwrap();
        store.append("abc", &conversation()).unwrap();
        // Simulate a process killed mid-write.
        let path = dir.join("abc.jsonl");
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str("{\"role\":\"user\",\"content\":[{\"type\":\"tex");
        std::fs::write(&path, text).unwrap();

        let read = store.read("abc").unwrap();
        assert_eq!(read.len(), 3, "the torn line is dropped, the rest survives");
    }

    #[test]
    fn sessions_can_be_listed_and_removed() {
        let store = Store::open(scratch("list")).unwrap();
        store.append("aaa", &[Message::user("x")]).unwrap();
        store.append("bbb", &[Message::user("y")]).unwrap();
        assert_eq!(store.list().unwrap(), vec!["aaa", "bbb"]);
        store.remove("aaa").unwrap();
        assert_eq!(store.list().unwrap(), vec!["bbb"]);
        // Removing something absent is not a failure.
        store.remove("aaa").unwrap();
    }

    #[test]
    fn an_id_that_could_escape_the_directory_is_refused() {
        // `session/load` carries an id from the client, so this is reachable.
        assert!(!Store::usable("../../etc/passwd"));
        assert!(!Store::usable("a/b"));
        assert!(!Store::usable(""));
        assert!(Store::usable("1a2b-3"));
        let store = Store::open(scratch("escape")).unwrap();
        assert!(store.append("../x", &[Message::user("no")]).is_err());
        assert!(store.read("../x").is_err());
        assert!(!store.exists("../x"));
    }

    #[test]
    fn reading_a_session_that_was_never_written_is_an_error_not_an_empty_one() {
        // Told apart on purpose: an empty conversation and a missing one mean
        // different things to `session/load`.
        let store = Store::open(scratch("missing")).unwrap();
        assert!(!store.exists("nope"));
        assert!(store.read("nope").is_err());
    }
}
