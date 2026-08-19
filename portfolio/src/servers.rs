//! Which ACP server to run, and how a model name reaches it.
//!
//! ACP is a protocol, not a program, and this file is what stops the client
//! being welded to the one implementation it grew up against. `opencode acp`
//! was hard-coded in two places -- the session in `acp.rs` and the hourly check
//! in `health.rs` -- which meant the section could only ever talk to opencode,
//! and that the check and the session could disagree about what they were
//! starting.
//!
//! **What varies between servers is small.** They all speak the same JSON-RPC
//! over the same stdio. What differs is the command to run and how the model
//! gets pinned: opencode takes a whole config document in the environment,
//! Zed's claude-code-acp takes a flag, others read a variable. So that is
//! exactly what is configurable here -- a command, and a `Pin` -- rather than a
//! template language in a text file, which is the version of this that ends up
//! quoting JSON inside YAML inside a comment.
//!
//! **Where the policy actually lives.** For opencode the gates travel with the
//! model as a tool policy the server itself enforces. No other server has any
//! reason to understand that document, so for those the only thing standing
//! between a visitor and a shell is our own gate in `acp.rs` -- which was
//! always the load-bearing one, and is why it exists. Adding a server here does
//! not widen what an agent may do; `gates.rs` decides that, once, for all of
//! them.

use std::process::{Command, Stdio};

/// How the chosen model reaches the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pin {
    /// The model and the tool policy travel together, as an opencode config
    /// document in `OPENCODE_CONFIG_CONTENT`. `opencode acp` has no `--model`
    /// flag, and the container's filesystem is read-only, so the environment is
    /// the only way in.
    OpencodeConfig,
    /// A command-line flag: `--model <name>`, or whatever the flag is called.
    Flag(String),
    /// A named environment variable carrying just the model name.
    Env(String),
    /// The server decides for itself and we say nothing.
    None,
}

impl Default for Pin {
    /// opencode, because that is what the shipped `models.txt` describes and a
    /// tier that names no command is a tier written before this file existed.
    fn default() -> Self {
        Pin::OpencodeConfig
    }
}

/// One ACP server: what to run, and how to tell it which model to use.
#[derive(Debug, Clone)]
pub struct Server {
    pub command: String,
    pub args: Vec<String>,
    pub pin: Pin,
}

impl Default for Server {
    fn default() -> Self {
        Server {
            command: "opencode".into(),
            args: vec!["acp".into()],
            pin: Pin::OpencodeConfig,
        }
    }
}

impl Server {
    /// Parse a `command` line: the first word is the program, the rest are args.
    ///
    /// Whitespace-split and nothing cleverer. A server whose path has a space in
    /// it is a problem for the day somebody has one; inventing shell quoting
    /// here would mean inventing shell escaping too, and this string is never
    /// handed to a shell.
    pub fn command_line(&mut self, line: &str) {
        let mut words = line.split_whitespace().map(str::to_string);
        if let Some(program) = words.next() {
            self.command = program;
            self.args = words.collect();
            // A command was named, so the opencode default no longer applies:
            // handing `OPENCODE_CONFIG_CONTENT` to something else is noise at
            // best. An explicit `pin` line after this still wins.
            if self.pin == Pin::OpencodeConfig && self.command != "opencode" {
                self.pin = Pin::None;
            }
        }
    }

    /// Parse a `pin` line.
    pub fn pin_line(&mut self, line: &str) {
        let (word, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        let rest = rest.trim();
        self.pin = match word {
            "opencode-config" => Pin::OpencodeConfig,
            "flag" if !rest.is_empty() => Pin::Flag(rest.to_string()),
            "env" if !rest.is_empty() => Pin::Env(rest.to_string()),
            "none" => Pin::None,
            // An unreadable pin is left as it was rather than guessed at. The
            // model then goes out the way the tier already said, which is the
            // conservative reading of a typo.
            _ => return,
        };
    }

    /// The whole configuration for one attempt, as an opencode config document.
    ///
    /// The gates travel with it. `tools` is what the agent is told it has and
    /// `permission` is what happens if it asks anyway -- both generated from
    /// `gates.rs` so this document cannot claim a policy the client does not
    /// then enforce.
    pub fn opencode_config(model: &str) -> String {
        format!(
            r#"{{"model":{},{}}}"#,
            crate::json::quote(model),
            crate::gates::tool_policy()
        )
    }

    /// A command ready to spawn, with the model pinned however this server
    /// wants it and stdio wired for JSON-RPC.
    pub fn spawn_command(&self, model: &str) -> Command {
        let mut c = Command::new(&self.command);
        c.args(&self.args);
        match &self.pin {
            Pin::OpencodeConfig => {
                c.env("OPENCODE_CONFIG_CONTENT", Self::opencode_config(model));
            }
            Pin::Flag(flag) => {
                c.arg(flag).arg(model);
            }
            Pin::Env(name) => {
                c.env(name, model);
            }
            Pin::None => {}
        }
        c.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // The server's own diagnostics are not the visitor's business and
            // would land in the middle of the rendered frame if they were.
            .stderr(Stdio::null());
        c
    }

    /// How to describe this server in one word, for the screen and the log.
    pub fn label(&self) -> &str {
        &self.command
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_is_the_opencode_the_shipped_file_describes() {
        let s = Server::default();
        assert_eq!(s.command, "opencode");
        assert_eq!(s.args, ["acp"]);
        assert_eq!(s.pin, Pin::OpencodeConfig);
    }

    #[test]
    fn a_command_line_splits_into_program_and_args() {
        let mut s = Server::default();
        s.command_line("npx -y @zed-industries/claude-code-acp");
        assert_eq!(s.command, "npx");
        assert_eq!(s.args, ["-y", "@zed-industries/claude-code-acp"]);
    }

    #[test]
    fn naming_another_server_drops_the_opencode_config() {
        // Handing OPENCODE_CONFIG_CONTENT to something that is not opencode is
        // at best ignored, and it would be silently relied upon.
        let mut s = Server::default();
        s.command_line("claude-code-acp");
        assert_eq!(s.pin, Pin::None);
    }

    #[test]
    fn an_explicit_pin_survives_the_command_that_precedes_it() {
        let mut s = Server::default();
        s.command_line("claude-code-acp");
        s.pin_line("flag --model");
        assert_eq!(s.pin, Pin::Flag("--model".into()));
    }

    #[test]
    fn a_command_naming_opencode_keeps_its_config_pin() {
        let mut s = Server::default();
        s.command_line("opencode acp");
        assert_eq!(s.pin, Pin::OpencodeConfig);
    }

    #[test]
    fn an_unreadable_pin_leaves_the_previous_one_alone() {
        let mut s = Server::default();
        s.pin_line("telepathy");
        assert_eq!(s.pin, Pin::OpencodeConfig);
        // A flag with no flag name is not a flag.
        s.pin_line("flag");
        assert_eq!(s.pin, Pin::OpencodeConfig);
    }

    #[test]
    fn the_opencode_config_pins_the_model_and_carries_the_gates() {
        let doc = Server::opencode_config("github-copilot/gpt-4.1");
        let v = crate::json::parse(&doc).expect("valid JSON");
        assert_eq!(v.get("model").and_then(|m| m.as_str()), Some("github-copilot/gpt-4.1"));
        // The gates came along, and the shell is shut in both blocks.
        assert_eq!(
            v.get("tools").and_then(|t| t.get("bash")).and_then(|b| b.as_bool()),
            Some(false)
        );
        assert_eq!(
            v.get("permission").and_then(|t| t.get("bash")).and_then(|b| b.as_str()),
            Some("deny")
        );
    }

    #[test]
    fn a_hostile_model_name_cannot_rewrite_the_config() {
        // The name is quoted, not interpolated. A model called `","bash":true`
        // must stay a model name.
        let doc = Server::opencode_config(r#"x","tools":{"bash":true},"x":"#);
        let v = crate::json::parse(&doc).expect("valid JSON");
        assert_eq!(
            v.get("tools").and_then(|t| t.get("bash")).and_then(|b| b.as_bool()),
            Some(false),
            "the config was rewritten by a model name"
        );
    }

    #[test]
    fn a_flag_pin_puts_the_model_after_the_args() {
        let mut s = Server::default();
        s.command_line("claude-code-acp --stdio");
        s.pin_line("flag --model");
        let c = s.spawn_command("sonnet");
        let args: Vec<_> = c.get_args().map(|a| a.to_string_lossy().to_string()).collect();
        assert_eq!(args, ["--stdio", "--model", "sonnet"]);
    }

    #[test]
    fn an_env_pin_carries_only_the_model_name() {
        let mut s = Server::default();
        s.command_line("some-acp-server");
        s.pin_line("env ACP_MODEL");
        let c = s.spawn_command("a/b");
        let env: Vec<_> = c
            .get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().to_string(),
                    v.map(|v| v.to_string_lossy().to_string()),
                )
            })
            .collect();
        assert!(env.contains(&("ACP_MODEL".to_string(), Some("a/b".to_string()))), "{env:?}");
    }
}
