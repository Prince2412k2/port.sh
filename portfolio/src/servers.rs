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

/// Every credential this image passes into the container.
///
/// Listed in one place so that scoping can be done by subtraction: a spawned
/// agent keeps the ones its tier declares and has the rest removed from its
/// environment. Adding a variable to the compose file and forgetting it here
/// means it reaches every agent, which is the failure this list exists to make
/// hard to arrive at by accident.
pub const SECRETS: &[&str] = &[
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "OLLAMA_API_KEY",
    "OPENCODE_API_KEY",
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
    // The web tools' keys, and the reason this list is by subtraction. These
    // two belong to *this* process -- `browse.rs` spends them, behind a
    // per-session ceiling and a tool description that says what they are for.
    // No tier declares them, so every agent starts without them, which is the
    // point: an agent handed a search key can search as much as it likes and
    // nothing here would count it.
    "EXA_API_KEY",
    "JINA_API_KEY",
];

/// One ACP server: what to run, and how to tell it which model to use.
#[derive(Debug, Clone)]
pub struct Server {
    pub command: String,
    pub args: Vec<String>,
    pub pin: Pin,
    /// The credentials this server is allowed to see. Everything else in
    /// `SECRETS` is stripped from its environment before it starts.
    ///
    /// The directories were already separate -- Copilot's under `COPILOT_HOME`,
    /// opencode's under `XDG_DATA_HOME` -- but separate paths are not isolation
    /// while every process inherits every token. A GitHub PAT with Copilot
    /// Requests on it has no business being visible to a process talking to
    /// ollama.com, and an ollama key has none inside Copilot.
    ///
    /// Worth being exact about what this is: the agents run as one user in one
    /// container, so this scopes what each is *handed*, not what it could reach
    /// if it went looking. It shrinks the blast radius of a leaky or talkative
    /// agent; it is not a sandbox.
    pub secrets: Vec<String>,
    /// Per-model settings handed to the server, as key/value pairs.
    ///
    /// Only opencode has anywhere to put these: its config takes
    /// `provider.<id>.models.<id>.options`, and that is where a reasoning
    /// effort or a verbosity goes. Kept as strings and passed through rather
    /// than modelled, because the set of them belongs to whoever wrote the
    /// server and changes faster than this file does -- the keys were read off
    /// the shipped binary, not guessed.
    pub options: Vec<(String, String)>,
    /// The variable a server reads *our* tool server's address from.
    ///
    /// ACP's only route for handing an agent a tool is an MCP server named in
    /// `session/new`, and an agent that does not advertise
    /// `mcpCapabilities.http` cannot be given one that way. Some can be told
    /// where it is by other means -- `envoy` takes `ENVOY_MCP_HTTP` -- and that
    /// is strictly better than the alternative, which is the map tools silently
    /// not existing on that tier.
    ///
    /// The address is per session: it carries the token that says whose screen
    /// this is, so it cannot live in a config file and cannot be shared between
    /// two visitors.
    pub tools_env: Option<String>,
    /// A flag that takes the gates' allow-list of tool names, comma separated --
    /// Copilot's `--available-tools`, for instance.
    ///
    /// Optional because it is the server's own second opinion, not the
    /// enforcement. `acp.rs` refuses a tool by name whatever this said; this
    /// just means the agent is never offered the tool in the first place, which
    /// is a better experience than watching it reach for something and be
    /// refused.
    pub tool_flag: Option<String>,
}

impl Default for Server {
    fn default() -> Self {
        Server {
            command: "opencode".into(),
            args: vec!["acp".into()],
            pin: Pin::OpencodeConfig,
            // opencode's policy travels in its config document instead.
            tool_flag: None,
            // opencode takes an MCP server in `session/new`, which is the
            // protocol's own way and needs nothing here.
            tools_env: None,
            // opencode fronts the provider tiers, so it gets the provider keys
            // and not the GitHub PAT.
            secrets: ["OPENCODE_API_KEY", "OLLAMA_API_KEY", "ANTHROPIC_API_KEY", "OPENAI_API_KEY"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            options: Vec::new(),
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

    /// Parse a `secrets` line: the credentials this server may see, or `none`.
    pub fn secrets_line(&mut self, line: &str) {
        if line.trim() == "none" {
            self.secrets.clear();
            return;
        }
        self.secrets = line.split_whitespace().map(|s| s.to_string()).collect();
    }

    /// Parse a `tools` line: how this server learns what it may call, and where
    /// our own tool server is.
    ///
    /// `flag` and `env` are different things and both are called `tools` because
    /// from `models.txt`'s side they are the same question -- "how do the tools
    /// reach this server". One is a list of names it is allowed to use, the
    /// other is an address to fetch ours from.
    pub fn tools_line(&mut self, line: &str) {
        let (word, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        let rest = rest.trim();
        match word {
            "flag" if !rest.is_empty() => self.tool_flag = Some(rest.to_string()),
            "env" if !rest.is_empty() => self.tools_env = Some(rest.to_string()),
            "none" => {
                self.tool_flag = None;
                self.tools_env = None;
            }
            _ => {}
        }
    }

    /// Whether this server is handed our tool server outside `session/new`.
    ///
    /// Asked by `acp.rs` for two decisions: whether to name the server in
    /// `session/new` at all, and whether the agent has our tools -- which is
    /// what stops the keyword guess from drawing a map the agent never asked
    /// for.
    pub fn tools_by_env(&self) -> bool {
        self.tools_env.is_some()
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
    pub fn opencode_config(&self, model: &str) -> String {
        // `provider/id`. Options are addressed by both halves, so a model named
        // without a provider gets none -- there is nowhere to hang them.
        let block = match (model.split_once('/'), self.options.is_empty()) {
            (Some((provider, id)), false) => {
                let opts: Vec<String> = self
                    .options
                    .iter()
                    .map(|(k, v)| format!("{}:{}", crate::json::quote(k), crate::json::quote(v)))
                    .collect();
                format!(
                    r#""provider":{{{}:{{"models":{{{}:{{"options":{{{}}}}}}}}}}},"#,
                    crate::json::quote(provider),
                    crate::json::quote(id),
                    opts.join(",")
                )
            }
            _ => String::new(),
        };
        format!(
            r#"{{"model":{},{block}{}}}"#,
            crate::json::quote(model),
            crate::gates::tool_policy()
        )
    }

    /// `option reasoningEffort low` from `models.txt`.
    pub fn option_line(&mut self, rest: &str) {
        if let Some((key, value)) = rest.trim().split_once(char::is_whitespace) {
            let (key, value) = (key.trim(), value.trim());
            if !key.is_empty() && !value.is_empty() {
                self.options.push((key.to_string(), value.to_string()));
            }
        }
    }

    /// A command ready to spawn, with the model pinned however this server
    /// wants it and stdio wired for JSON-RPC.
    pub fn spawn_command(&self, model: &str, tools: Option<&str>) -> Command {
        let mut c = Command::new(&self.command);
        c.args(&self.args);
        c.env_clear();
        for var in [
            "PATH",
            "HOME",
            "ENVOY_CONFIG",
            "ENVOY_CONFIG_CONTENT",
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "XDG_CACHE_HOME",
            "COPILOT_HOME",
            "SSL_CERT_FILE",
            "SSL_CERT_DIR",
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "NO_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
            "no_proxy",
        ] {
            if let Some(value) = std::env::var_os(var) {
                c.env(var, value);
            }
        }
        for var in SECRETS {
            let declared = self.secrets.iter().any(|s| s == var);
            // Included only when this tier declares a non-empty value. Compose passes
            // `OPENAI_API_KEY: ${OPENAI_API_KEY:-}`, so the variable exists with
            // no value whenever the host does not set one, and a server that
            // tests whether a credential is *present* rather than whether it is
            // *usable* will take that empty string and stop looking. The real
            // credential here is an OAuth login in opencode's own store on the
            // volume, and an empty environment variable must not shadow it.
            let usable = std::env::var(var).is_ok_and(|v| !v.trim().is_empty());
            if declared && usable {
                c.env(var, std::env::var(var).expect("checked above"));
            }
        }
        let work = std::env::temp_dir().join("portfolio-agent");
        if std::fs::create_dir_all(&work).is_ok() {
            c.current_dir(work);
        }
        if let Some(flag) = &self.tool_flag {
            let list = crate::gates::open_tool_names().join(",");
            // Printed once, because the contents are load-bearing and were
            // silently wrong: Copilot's `--available-tools` is "only these", by
            // exact name, and it renames MCP tools to `<server>-<tool>`. A list
            // that omits the prefixed spelling hides the tool from the model
            // with nothing anywhere reporting a problem.
            static SAID: std::sync::OnceLock<()> = std::sync::OnceLock::new();
            if SAID.set(()).is_ok() {
                eprintln!("portfolio: tools offered as `{flag} {list}`");
            }
            c.arg(flag).arg(list);
        }
        // Our tool server, for a server that takes it this way. Nothing is set
        // when there is no session to draw on -- the hourly health check has no
        // screen -- and an empty variable would be worse than an absent one for
        // the same reason it is for a credential.
        match (&self.tools_env, tools) {
            (Some(var), Some(url)) => {
                c.env(var, format!("{}={url}", crate::mcp::SERVER_NAME));
            }
            (Some(var), None) => {
                c.env_remove(var);
            }
            (None, _) => {}
        }
        match &self.pin {
            Pin::OpencodeConfig => {
                c.env("OPENCODE_CONFIG_CONTENT", self.opencode_config(model));
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

    /// A server told where our tool server is gets the address, with the name
    /// the tools are namespaced under -- and gets nothing at all when there is
    /// no screen to draw on, rather than an empty variable.
    #[test]
    fn a_server_that_takes_our_tools_in_its_environment_is_told_where_they_are() {
        use super::*;
        let mut server = Server::default();
        server.command_line("envoy");
        server.tools_line("env ENVOY_MCP_HTTP");
        assert!(server.tools_by_env());

        let set = |c: &Command, var: &str| -> Option<String> {
            c.get_envs().find(|(k, _)| *k == var).and_then(|(_, v)| {
                v.map(|v| v.to_string_lossy().to_string())
            })
        };

        let c = server.spawn_command("gpt-5.6-luna", Some("http://127.0.0.1:9/mcp/tok"));
        assert_eq!(
            set(&c, "ENVOY_MCP_HTTP").as_deref(),
            Some("portfolio=http://127.0.0.1:9/mcp/tok"),
            "the address did not reach the agent, or lost the server name it \
             namespaces our tools under"
        );

        // No screen: the check has none, and neither does a session whose page
        // has gone. An empty variable would configure a server at no address.
        let c = server.spawn_command("gpt-5.6-luna", None);
        assert!(set(&c, "ENVOY_MCP_HTTP").is_none(), "an empty tool address was passed");

        // A server that does not take them this way is never given the variable
        // whatever we know.
        let plain = Server::default();
        let c = plain.spawn_command("openai/gpt", Some("http://127.0.0.1:9/mcp/tok"));
        assert!(set(&c, "ENVOY_MCP_HTTP").is_none());
    }

    /// `tools none` means neither route, not just the flag.
    #[test]
    fn tools_none_clears_both_routes() {
        use super::*;
        let mut server = Server::default();
        server.tools_line("flag --available-tools");
        server.tools_line("env ENVOY_MCP_HTTP");
        assert!(server.tool_flag.is_some() && server.tools_by_env());
        server.tools_line("none");
        assert!(server.tool_flag.is_none(), "the flag survived `tools none`");
        assert!(!server.tools_by_env(), "the address survived `tools none`");
    }

    /// An empty credential is stripped, not passed on.
    ///
    /// Compose passes `OPENAI_API_KEY: ${OPENAI_API_KEY:-}`, so the variable
    /// exists with no value whenever the host does not set one -- and the real
    /// credential for that provider is an OAuth login in opencode's own store.
    /// A server that checks whether a key is *present* rather than *usable*
    /// would take the empty string and stop looking, past a working token.
    #[test]
    fn an_empty_credential_is_not_handed_to_an_agent() {
        let _held = crate::visits::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let server = Server::default();
        assert!(server.secrets.iter().any(|s| s == "OPENAI_API_KEY"), "not declared");

        let seen = |c: &Command| -> Vec<(String, Option<String>)> {
            c.get_envs()
                .map(|(k, v)| {
                    (
                        k.to_string_lossy().to_string(),
                        v.map(|v| v.to_string_lossy().to_string()),
                    )
                })
                .collect()
        };
        let present = |c: &Command, var: &str| {
            seen(c).iter().any(|(key, value)| key == var && value.is_some())
        };

        unsafe { std::env::set_var("OPENAI_API_KEY", "") };
        let c = server.spawn_command("openai/gpt-5.6-luna", None);
        assert!(!present(&c, "OPENAI_API_KEY"), "an empty key was passed through");

        unsafe { std::env::set_var("OPENAI_API_KEY", "   ") };
        let c = server.spawn_command("openai/gpt-5.6-luna", None);
        assert!(!present(&c, "OPENAI_API_KEY"), "whitespace counted as a credential");

        unsafe { std::env::set_var("OPENAI_API_KEY", "sk-real") };
        let c = server.spawn_command("openai/gpt-5.6-luna", None);
        assert!(present(&c, "OPENAI_API_KEY"), "a real key was stripped");

        // And a secret this tier never declared stays gone whatever its value.
        unsafe { std::env::set_var("GH_TOKEN", "ghp-real") };
        let c = server.spawn_command("openai/gpt-5.6-luna", None);
        assert!(!present(&c, "GH_TOKEN"), "an undeclared secret leaked in");

        unsafe { std::env::remove_var("OPENAI_API_KEY") };
        unsafe { std::env::remove_var("GH_TOKEN") };
    }

    /// A per-model option reaches opencode's config where opencode looks for it.
    ///
    /// The shape is `provider.<id>.models.<id>.options`, and both halves of the
    /// model id address it -- read off the shipped binary rather than guessed,
    /// along with the key names it accepts.
    #[test]
    fn a_model_option_lands_where_opencode_reads_it() {
        let mut server = Server::default();
        server.option_line("reasoningEffort low");
        let doc = server.opencode_config("openai/gpt-5.6-luna");
        let v = crate::json::parse(&doc).expect("not json");

        assert_eq!(v.get("model").and_then(|m| m.as_str()), Some("openai/gpt-5.6-luna"));
        let opts = v
            .get("provider")
            .and_then(|p| p.get("openai"))
            .and_then(|p| p.get("models"))
            .and_then(|m| m.get("gpt-5.6-luna"))
            .and_then(|m| m.get("options"))
            .unwrap_or_else(|| panic!("no options in {doc}"));
        assert_eq!(opts.get("reasoningEffort").and_then(|r| r.as_str()), Some("low"));

        // The tool policy still travels with it -- that is the whole reason this
        // document exists and an option must not displace it.
        assert!(doc.contains("bash"), "the tool policy was lost: {doc}");

        // No options, and the document is what it always was.
        let plain = Server::default().opencode_config("openai/gpt-5.6-luna");
        assert!(!plain.contains("provider"), "an empty options list wrote a block: {plain}");
        assert!(crate::json::parse(&plain).is_some(), "not json: {plain}");

        // A model with no provider half has nowhere to hang them, and says so by
        // writing nothing rather than by writing something malformed.
        let mut bare = Server::default();
        bare.option_line("reasoningEffort low");
        let doc = bare.opencode_config("auto");
        assert!(!doc.contains("provider"), "{doc}");
        assert!(crate::json::parse(&doc).is_some(), "not json: {doc}");
    }
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
        let doc = Server::default().opencode_config("github-copilot/gpt-4.1");
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
        let doc = Server::default().opencode_config(r#"x","tools":{"bash":true},"x":"#);
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
        let c = s.spawn_command("sonnet", None);
        let args: Vec<_> = c.get_args().map(|a| a.to_string_lossy().to_string()).collect();
        assert_eq!(args, ["--stdio", "--model", "sonnet"]);
    }

    /// Copilot takes its allow-list on the command line, and it must be the
    /// gates' list rather than a second one written out by hand.
    #[test]
    fn a_tool_flag_carries_the_gates_allow_list() {
        let mut s = Server::default();
        s.command_line("copilot --acp");
        s.pin_line("flag --model");
        s.tools_line("flag --available-tools");
        let c = s.spawn_command("auto", None);
        let args: Vec<String> =
            c.get_args().map(|a| a.to_string_lossy().to_string()).collect();

        let at = args.iter().position(|a| a == "--available-tools").expect("no tool flag");
        let list = &args[at + 1];
        // Copilot's spelling has to be in there, or it is handed a list of names
        // it does not recognise and quietly loses its web tools.
        assert!(list.contains("web_fetch"), "{list}");
        assert!(list.contains("web_search"), "{list}");
        // And nothing shut may appear in an allow-list.
        for shut in ["bash", "shell", "view", "grep", "write"] {
            assert!(!list.split(',').any(|n| n == shut), "{shut} is in the allow-list: {list}");
        }
        // The model still gets pinned alongside it.
        assert!(args.windows(2).any(|w| w[0] == "--model" && w[1] == "auto"), "{args:?}");
    }

    /// Each agent is handed its own credentials and stripped of everybody
    /// else's. Separate directories were never the isolation that mattered --
    /// every spawned process inherited every token.
    #[test]
    fn an_agent_is_handed_only_the_credentials_its_tier_declares() {
        // Every secret set to something, because "declared" is no longer enough
        // on its own: an empty credential is stripped too, and this test used to
        // assert that a *declared* one survives while never setting it. It
        // passed only because the old rule never looked at the value.
        let _held = crate::visits::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        for var in SECRETS {
            unsafe { std::env::set_var(var, "set-for-this-test") };
        }

        // Copilot's tier: the PAT, and nothing from the providers.
        let mut copilot = Server::default();
        copilot.command_line("copilot --acp");
        copilot.secrets_line("GH_TOKEN GITHUB_TOKEN");
        let present = |c: &Command| -> Vec<String> {
            c.get_envs()
                .filter(|(_, value)| value.is_some())
                .map(|(k, _)| k.to_string_lossy().to_string())
                .collect()
        };
        let inherited = present(&copilot.spawn_command("auto", None));
        for gone in ["OLLAMA_API_KEY", "OPENCODE_API_KEY", "ANTHROPIC_API_KEY", "OPENAI_API_KEY"] {
            assert!(!inherited.contains(&gone.to_string()), "{gone} still reaches copilot: {inherited:?}");
        }
        for kept in ["GH_TOKEN", "GITHUB_TOKEN"] {
            assert!(inherited.contains(&kept.to_string()), "{kept} was taken from copilot");
        }

        // And the other way: opencode fronts the providers and has no business
        // holding a GitHub PAT.
        let inherited = present(&Server::default().spawn_command("a/b", None));
        for gone in ["GH_TOKEN", "GITHUB_TOKEN"] {
            assert!(!inherited.contains(&gone.to_string()), "{gone} still reaches opencode: {inherited:?}");
        }
        for kept in ["OPENCODE_API_KEY", "OLLAMA_API_KEY"] {
            assert!(inherited.contains(&kept.to_string()), "{kept} was taken from opencode");
        }

        for var in SECRETS {
            unsafe { std::env::remove_var(var) };
        }
    }

    /// `secrets none` means exactly that, and every known credential goes.
    #[test]
    fn a_server_can_be_given_no_credentials_at_all() {
        let mut s = Server::default();
        s.secrets_line("none");
        assert!(s.secrets.is_empty());
        let c = s.spawn_command("a/b", None);
        let inherited: Vec<String> = c
            .get_envs()
            .filter(|(_, value)| value.is_some())
            .map(|(k, _)| k.to_string_lossy().to_string())
            .collect();
        for var in SECRETS {
            assert!(!inherited.contains(&var.to_string()), "{var} survived `secrets none`");
        }
    }

    #[test]
    fn a_server_without_a_tool_flag_passes_none() {
        let s = Server::default();
        let c = s.spawn_command("a/b", None);
        let args: Vec<String> =
            c.get_args().map(|a| a.to_string_lossy().to_string()).collect();
        assert!(!args.iter().any(|a| a.starts_with("--available")), "{args:?}");
    }

    #[test]
    fn an_env_pin_carries_only_the_model_name() {
        let mut s = Server::default();
        s.command_line("some-acp-server");
        s.pin_line("env ACP_MODEL");
        let c = s.spawn_command("a/b", None);
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
