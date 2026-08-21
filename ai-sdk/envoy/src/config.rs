//! What to run, from a file rather than from the source.
//!
//! Model ids move faster than a release does, and the ones that matter here are
//! not discoverable: Ollama Cloud's `/v1/models` returns ids with no context
//! window and no prices, and the Codex backend publishes no list at all. So the
//! catalogue is configuration -- the same choice `data/models.txt` makes in
//! `portfolio`, and for the same reason.
//!
//! Tiers are tried in order. The first that answers is the one the conversation
//! uses, and a tier that stops answering mid-session is fallen through without
//! restarting anything.

use std::path::PathBuf;
use std::sync::Arc;

use parley::auth::Auth;
use parley::types::{Api, Cost, Endpoint, Model};
use parley::{Fallback, Tier};
use serde::{Deserialize, Serialize};

/// Where a whole config document can arrive instead of a path. See
/// `Config::from_env_or`.
pub const CONTENT_VAR: &str = "ENVOY_CONFIG_CONTENT";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    #[serde(default)]
    pub system: Option<String>,
    #[serde(default)]
    pub budget: BudgetConfig,
    #[serde(default)]
    pub compaction: CompactionConfig,
    pub tiers: Vec<TierConfig>,
    /// MCP servers to start and take tools from. Started once, at boot: a
    /// server that fails to start is reported and skipped rather than retried
    /// per prompt, because a broken server should not cost every turn a timeout.
    #[serde(default)]
    pub mcp_servers: Vec<McpConfig>,
    /// A model to summarise what compaction drops. Optional and explicit: a
    /// default would mean quietly spending somebody's tokens on housekeeping.
    #[serde(default)]
    pub summariser: Option<TierConfig>,
    /// Where to keep conversations. Absent means they live only as long as the
    /// process, and resuming is advertised as unavailable.
    #[serde(default)]
    pub session_dir: Option<PathBuf>,
}

/// One MCP server. A `url` means HTTP; a `command` means a child process.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpConfig {
    pub name: String,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub url: Option<String>,
    /// Extra headers for the HTTP transport -- an authorization, usually.
    #[serde(default)]
    pub headers: Vec<(String, String)>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BudgetConfig {
    pub turns: usize,
    pub tool_calls: usize,
}

impl Default for BudgetConfig {
    fn default() -> BudgetConfig {
        let d = crate::Budget::default();
        BudgetConfig {
            turns: d.turns,
            tool_calls: d.tool_calls,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionConfig {
    pub enabled: bool,
    pub reserve: u64,
    pub keep_recent: u64,
}

impl Default for CompactionConfig {
    fn default() -> CompactionConfig {
        let d = crate::Compaction::default();
        CompactionConfig {
            enabled: d.enabled,
            reserve: d.reserve,
            keep_recent: d.keep_recent,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TierConfig {
    pub provider: String,
    pub model: String,
    pub api: ApiName,
    pub base_url: String,
    pub context_window: u64,
    #[serde(default)]
    pub max_output: Option<u64>,
    #[serde(default)]
    pub reasoning: bool,
    #[serde(default)]
    pub auth: AuthConfig,
    /// Price per million tokens. Absent means unpriced, which is reported as no
    /// cost rather than as zero cost.
    #[serde(default)]
    pub cost: Option<CostConfig>,
    /// How hard this tier should think. `models.txt` already spells this per
    /// tier, so it belongs per tier here too.
    #[serde(default)]
    pub effort: Option<EffortName>,
    #[serde(default)]
    pub temperature: Option<f64>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EffortName {
    Off,
    Minimal,
    Low,
    Medium,
    High,
}

impl From<EffortName> for parley::types::Effort {
    fn from(name: EffortName) -> parley::types::Effort {
        use parley::types::Effort;
        match name {
            EffortName::Off => Effort::Off,
            EffortName::Minimal => Effort::Minimal,
            EffortName::Low => Effort::Low,
            EffortName::Medium => Effort::Medium,
            EffortName::High => Effort::High,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ApiName {
    OpenaiCompletions,
    OpenaiResponses,
    OllamaChat,
}

impl From<ApiName> for Api {
    fn from(name: ApiName) -> Api {
        match name {
            ApiName::OpenaiCompletions => Api::OpenaiCompletions,
            ApiName::OpenaiResponses => Api::OpenaiResponses,
            ApiName::OllamaChat => Api::OllamaChat,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AuthConfig {
    #[default]
    None,
    Env(Vec<String>),
    Codex(PathBuf),
    /// opencode's `auth.json`, which is usually the login that exists: an
    /// operator who has run anything has run `opencode auth login`.
    Opencode(PathBuf),
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CostConfig {
    pub input: f64,
    pub output: f64,
    #[serde(default)]
    pub cache_read: f64,
    #[serde(default)]
    pub cache_write: f64,
}

impl TierConfig {
    pub fn model(&self) -> Model {
        Model {
            id: self.model.clone(),
            name: self.model.clone(),
            provider: self.provider.clone(),
            api: self.api.into(),
            context_window: self.context_window,
            max_output: self.max_output,
            reasoning: self.reasoning,
            cost: self
                .cost
                .map(|c| Cost {
                    input: c.input,
                    output: c.output,
                    cache_read: c.cache_read,
                    cache_write: c.cache_write,
                })
                .unwrap_or_default(),
        }
    }

    pub fn tuning(&self) -> parley::types::Tuning {
        parley::types::Tuning {
            temperature: self.temperature,
            effort: self.effort.map(Into::into),
            max_output: self.max_output,
        }
    }

    pub fn auth(&self) -> Auth {
        match &self.auth {
            AuthConfig::None => Auth::None,
            AuthConfig::Env(names) => Auth::Env(names.clone()),
            AuthConfig::Codex(path) => Auth::Codex(expand(path)),
            AuthConfig::Opencode(path) => Auth::Opencode(expand(path)),
        }
    }
}

/// `~` at the front of a path, since a config file is written by a person.
fn expand(path: &std::path::Path) -> PathBuf {
    let text = path.to_string_lossy();
    match text.strip_prefix("~/") {
        Some(rest) => match std::env::var_os("HOME") {
            Some(home) => PathBuf::from(home).join(rest),
            None => path.to_path_buf(),
        },
        None => path.to_path_buf(),
    }
}

/// What a tier resolved to, and why it did not if it did not.
pub struct Ready {
    pub tiers: Vec<Tier>,
    /// One line per tier that could not be used. Reported rather than hidden:
    /// a session that quietly falls to the third choice looks like a slow
    /// model, and the reason belongs in a log an operator can read.
    pub skipped: Vec<String>,
}

impl Config {
    pub fn read(path: impl AsRef<std::path::Path>) -> std::io::Result<Config> {
        let text = std::fs::read_to_string(path)?;
        serde_json::from_str(&text).map_err(std::io::Error::other)
    }

    /// The whole document, or a file to read it from.
    ///
    /// `--config` names a file, which is right for a person and wrong for a
    /// program: a client that generates a document per session -- because it
    /// carries that session's tool server address -- has nowhere to put it, and
    /// the container this runs in has no writable filesystem at all. opencode
    /// has the same problem and answers it the same way, with
    /// `OPENCODE_CONFIG_CONTENT`, so the spelling here is deliberately familiar.
    ///
    /// The variable wins over the path. A client that has gone to the trouble of
    /// composing a document did not mean to be overruled by whatever file
    /// happened to be in the working directory.
    pub fn from_env_or(path: &str) -> std::io::Result<(Config, String)> {
        match std::env::var(CONTENT_VAR) {
            Ok(text) if !text.trim().is_empty() => {
                let config = serde_json::from_str(&text).map_err(std::io::Error::other)?;
                Ok((config, format!("${CONTENT_VAR}")))
            }
            _ => Ok((Config::read(path)?, path.to_string())),
        }
    }

    /// Turn tiers into wires, dropping the ones with no usable credential.
    pub fn ready(&self) -> Ready {
        let mut tiers = Vec::new();
        let mut skipped = Vec::new();
        for tier in &self.tiers {
            match parley::auth::resolve(&tier.auth()) {
                Err(e) => skipped.push(format!("{}/{}: {e}", tier.provider, tier.model)),
                Ok(resolved) => tiers.push(Tier {
                    wire: Arc::from(parley::api::wire(tier.api.into())),
                    model: tier.model(),
                    endpoint: Endpoint {
                        base_url: tier.base_url.clone(),
                        api_key: resolved.api_key,
                        headers: resolved.headers,
                    },
                    tuning: tier.tuning(),
                }),
            }
        }
        Ready { tiers, skipped }
    }

    pub fn budget(&self) -> crate::Budget {
        crate::Budget {
            turns: self.budget.turns,
            tool_calls: self.budget.tool_calls,
        }
    }

    pub fn compaction(&self) -> crate::Compaction {
        crate::Compaction {
            enabled: self.compaction.enabled,
            reserve: self.compaction.reserve,
            keep_recent: self.compaction.keep_recent,
        }
    }

    /// The summarising model, if one is configured and its credential resolves.
    pub fn summariser(&self) -> Option<crate::compact::Summariser> {
        let tier = self.summariser.as_ref()?;
        let resolved = parley::auth::resolve(&tier.auth()).ok()?;
        let mut options = parley::types::Options::default();
        tier.tuning().apply(&mut options);
        Some(crate::compact::Summariser {
            wire: Arc::from(parley::api::wire(tier.api.into())),
            model: tier.model(),
            endpoint: Endpoint {
                base_url: tier.base_url.clone(),
                api_key: resolved.api_key,
                headers: resolved.headers,
            },
            options,
        })
    }

    /// Open the session store, if one is configured.
    pub fn store(&self) -> Option<Arc<crate::Store>> {
        let dir = expand(self.session_dir.as_ref()?);
        match crate::Store::open(&dir) {
            Ok(store) => Some(Arc::new(store)),
            Err(e) => {
                eprintln!("envoy: cannot keep sessions in {}: {e}", dir.display());
                None
            }
        }
    }

    pub fn fallback(&self) -> (Arc<Fallback>, Vec<String>) {
        let ready = self.ready();
        (Arc::new(Fallback::new(ready.tiers)), ready.skipped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
      "system": "be terse",
      "budget": { "turns": 12, "toolCalls": 24 },
      "tiers": [
        {
          "provider": "ollama-cloud",
          "model": "gpt-oss:120b",
          "api": "openai-completions",
          "baseUrl": "https://ollama.com/v1",
          "contextWindow": 131072,
          "auth": { "env": ["OLLAMA_API_KEY"] }
        },
        {
          "provider": "openai-codex",
          "model": "gpt-5-codex",
          "api": "openai-responses",
          "baseUrl": "https://chatgpt.com/backend-api/codex",
          "contextWindow": 272000,
          "reasoning": true,
          "auth": { "codex": "~/.codex/auth.json" }
        }
      ]
    }"#;

    #[test]
    fn a_config_file_parses_into_tiers() {
        let config: Config = serde_json::from_str(SAMPLE).unwrap();
        assert_eq!(config.tiers.len(), 2);
        assert_eq!(config.system.as_deref(), Some("be terse"));
        assert_eq!(config.budget().turns, 12);
        let codex = config.tiers[1].model();
        assert!(codex.reasoning);
        assert_eq!(codex.api, Api::OpenaiResponses);
    }

    #[test]
    fn defaults_apply_when_the_file_is_terse() {
        let config: Config = serde_json::from_str(
            r#"{"tiers":[{"provider":"p","model":"m","api":"openai-completions","baseUrl":"http://x","contextWindow":1000}]}"#,
        )
        .unwrap();
        assert_eq!(config.budget().turns, crate::Budget::default().turns);
        assert!(config.compaction().enabled);
        assert!(matches!(config.tiers[0].auth(), Auth::None));
    }

    #[test]
    fn a_tier_with_no_credential_is_skipped_and_named() {
        std::env::remove_var("ENVOY_TEST_ABSENT");
        let config: Config = serde_json::from_str(
            r#"{"tiers":[
                {"provider":"gone","model":"m","api":"openai-completions","baseUrl":"http://x","contextWindow":10,
                 "auth":{"env":["ENVOY_TEST_ABSENT"]}},
                {"provider":"open","model":"m2","api":"openai-completions","baseUrl":"http://y","contextWindow":10}
            ]}"#,
        )
        .unwrap();
        let ready = config.ready();
        assert_eq!(ready.tiers.len(), 1);
        assert_eq!(ready.skipped.len(), 1);
        assert!(ready.skipped[0].contains("gone/m"), "{:?}", ready.skipped);
    }

    #[test]
    fn a_home_relative_credential_path_is_expanded() {
        let config: Config = serde_json::from_str(SAMPLE).unwrap();
        let Auth::Codex(path) = config.tiers[1].auth() else {
            panic!("expected a codex path")
        };
        assert!(!path.to_string_lossy().starts_with('~'), "{path:?}");
        assert!(path.to_string_lossy().ends_with(".codex/auth.json"));
    }

    #[test]
    fn an_mcp_server_is_stdio_or_http_by_which_field_is_set() {
        let config: Config = serde_json::from_str(
            r#"{"tiers":[],"mcpServers":[
                {"name":"files","command":"npx","args":["-y","server-filesystem"]},
                {"name":"remote","url":"https://example.test/mcp","headers":[["authorization","Bearer x"]]}
            ]}"#,
        )
        .unwrap();
        assert!(config.mcp_servers[0].url.is_none());
        assert_eq!(config.mcp_servers[0].command.as_deref(), Some("npx"));
        assert_eq!(config.mcp_servers[1].url.as_deref(), Some("https://example.test/mcp"));
        assert_eq!(config.mcp_servers[1].headers[0].0, "authorization");
    }

    #[test]
    fn per_tier_tuning_is_read() {
        let config: Config = serde_json::from_str(
            r#"{"tiers":[{"provider":"p","model":"m","api":"openai-responses","baseUrl":"http://x",
                "contextWindow":1000,"effort":"low","temperature":0.2,"maxOutput":4096}]}"#,
        )
        .unwrap();
        let tuning = config.tiers[0].tuning();
        assert_eq!(tuning.effort, Some(parley::types::Effort::Low));
        assert_eq!(tuning.temperature, Some(0.2));
        assert_eq!(tuning.max_output, Some(4096));
    }

    #[test]
    fn a_tier_that_says_nothing_about_tuning_overrides_nothing() {
        let config: Config = serde_json::from_str(SAMPLE).unwrap();
        let tuning = config.tiers[0].tuning();
        assert!(tuning.effort.is_none() && tuning.temperature.is_none());
    }

    /// A whole document in the environment beats a path, and beats it even when
    /// the path would have worked -- a client that composed one meant it.
    ///
    /// Serialised in one place so the two tests cannot disagree about the
    /// variable's name, which is the mistake this is guarding against.
    #[test]
    fn a_document_in_the_environment_wins_over_a_file() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("envoy-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("envoy.json");
        std::fs::write(&path, SAMPLE).unwrap();
        let file = path.to_string_lossy().to_string();

        std::env::remove_var(CONTENT_VAR);
        let (from_file, source) = Config::from_env_or(&file).expect("the file did not read");
        assert_eq!(source, file);
        let in_file = from_file.tiers.len();

        std::env::set_var(
            CONTENT_VAR,
            r#"{"tiers":[
                {"provider":"a","model":"one","api":"openai-responses","baseUrl":"http://x","contextWindow":9},
                {"provider":"b","model":"two","api":"ollama-chat","baseUrl":"http://y","contextWindow":9},
                {"provider":"c","model":"three","api":"ollama-chat","baseUrl":"http://z","contextWindow":9}
            ]}"#,
        );
        let (from_env, source) = Config::from_env_or(&file).expect("the document did not read");
        assert_eq!(source, format!("${CONTENT_VAR}"));
        assert_eq!(from_env.tiers.len(), 3, "the file overruled the document");
        assert_ne!(in_file, 3, "the two cases are indistinguishable");

        // Empty is the same as absent: a variable that exists and is blank is
        // what a compose file passes on a host that has not set one.
        std::env::set_var(CONTENT_VAR, "  ");
        let (fallen_back, source) = Config::from_env_or(&file).expect("did not fall back");
        assert_eq!(source, file);
        assert_eq!(fallen_back.tiers.len(), in_file);

        // A document that is not JSON is an error, not a silent fall back to
        // whatever file is lying around -- the client would never learn.
        std::env::set_var(CONTENT_VAR, "{ this is not json");
        assert!(Config::from_env_or(&file).is_err());

        std::env::remove_var(CONTENT_VAR);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// One test in this file sets an environment variable, and cargo runs tests
    /// on threads that share it.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn an_opencode_login_is_a_credential_a_tier_can_name() {
        let config: Config = serde_json::from_str(
            r#"{"tiers":[{"provider":"openai","model":"gpt","api":"openai-responses",
                "baseUrl":"http://x","contextWindow":9,
                "auth":{"opencode":"~/.local/share/opencode/auth.json"}}]}"#,
        )
        .expect("did not parse");
        match config.tiers[0].auth() {
            parley::auth::Auth::Opencode(path) => {
                assert!(path.is_absolute(), "`~` was not expanded: {path:?}");
                assert!(path.ends_with(".local/share/opencode/auth.json"), "{path:?}");
            }
            other => panic!("wrong credential kind: {other:?}"),
        }
    }

    #[test]
    fn an_unpriced_model_reports_no_cost_rather_than_zero() {
        let config: Config = serde_json::from_str(SAMPLE).unwrap();
        assert_eq!(config.tiers[0].model().cost, Cost::default());
    }
}
