//! Tools, and the three places they can come from.
//!
//! A tool is a name, a schema, and something to run. Where it runs is the only
//! interesting difference:
//!
//! - **Native** -- a Rust function compiled in. Anything that does not need the
//!   client's screen belongs here.
//! - **Client** -- implemented at the other end of the protocol, because it has
//!   to touch something we do not have. `show_map` draws on a panel that lives
//!   in the client's process; there is no way for this one to do it.
//! - **MCP** -- somebody else's server.
//!
//! All three arrive as `Arc<dyn Tool>` and the loop cannot tell them apart,
//! which is the point: adding a source does not change the loop.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use parley::types::Block;
use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

/// What the client should show while this runs. The names are ACP's, because
/// they are what a client picks an icon from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Kind {
    Read,
    Edit,
    Delete,
    Move,
    Search,
    Execute,
    Think,
    Fetch,
    SwitchMode,
    #[default]
    Other,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Read => "read",
            Kind::Edit => "edit",
            Kind::Delete => "delete",
            Kind::Move => "move",
            Kind::Search => "search",
            Kind::Execute => "execute",
            Kind::Think => "think",
            Kind::Fetch => "fetch",
            Kind::SwitchMode => "switch_mode",
            Kind::Other => "other",
        }
    }
}

/// Whether this tool may run alongside others in the same batch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Parallel,
    /// Nothing else runs while this does. One sequential tool in a batch makes
    /// the whole batch sequential -- less clever than interleaving, and much
    /// easier to reason about when something goes wrong.
    Sequential,
}

#[derive(Clone, Debug)]
pub struct Spec {
    /// What the model calls it.
    pub name: String,
    /// What a person reads. ACP wants a `title` per call; this is the default
    /// when a tool does not build a better one from its arguments.
    pub title: String,
    /// What the model reads to decide whether to call it.
    pub description: String,
    /// Hand-written JSON Schema.
    pub schema: Value,
    pub kind: Kind,
    pub mode: Mode,
}

/// What a tool produced.
#[derive(Clone, Debug, Default)]
pub struct Output {
    /// What the model reads.
    pub content: Vec<Block>,
    /// What the client renders, if it wants more than the text. Becomes ACP's
    /// `rawOutput`.
    pub raw: Option<Value>,
}

impl Output {
    pub fn text(s: impl Into<String>) -> Output {
        Output {
            content: vec![Block::text(s)],
            raw: None,
        }
    }
}

/// A tool failing is not an error in the program.
///
/// It goes back to the model as a tool result marked as a failure, because a
/// model that reads "no such place" can try a different spelling, and one that
/// gets an aborted turn can do nothing at all.
#[derive(Clone, Debug, thiserror::Error)]
#[error("{0}")]
pub struct Failed(pub String);

impl Failed {
    pub fn new(s: impl Into<String>) -> Failed {
        Failed(s.into())
    }
}

/// Handed to a tool for the duration of one call.
pub struct Cx {
    pub call_id: String,
    pub cancel: CancellationToken,
    progress: UnboundedSender<(String, Output)>,
}

impl Cx {
    pub fn new(
        call_id: String,
        cancel: CancellationToken,
        progress: UnboundedSender<(String, Output)>,
    ) -> Cx {
        Cx {
            call_id,
            cancel,
            progress,
        }
    }

    /// Report progress without finishing. Reaches the client as a
    /// `tool_call_update`; a fetch can say what it is downloading, a search can
    /// report hits as it finds them.
    pub fn progress(&self, output: Output) {
        // A closed receiver means the turn is over and nobody is listening.
        // Not worth telling the tool about.
        let _ = self.progress.send((self.call_id.clone(), output));
    }

    pub fn cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn spec(&self) -> &Spec;

    /// A title for this particular call, given its arguments. The default is
    /// the spec's title; overriding it is how a client gets "Fetch example.com"
    /// rather than "Fetch".
    fn title(&self, _args: &Value) -> String {
        self.spec().title.clone()
    }

    async fn call(&self, args: Value, cx: Cx) -> Result<Output, Failed>;
}

/// Somewhere tools come from. The seam MCP arrives through.
#[async_trait]
pub trait Source: Send + Sync {
    async fn tools(&self) -> Vec<Arc<dyn Tool>>;
}

/// The tools available for a turn, by name.
#[derive(Default)]
pub struct Set {
    by_name: BTreeMap<String, Arc<dyn Tool>>,
}

impl Set {
    pub fn new() -> Set {
        Set::default()
    }

    pub fn with(mut self, tool: Arc<dyn Tool>) -> Set {
        self.add(tool);
        self
    }

    pub fn add(&mut self, tool: Arc<dyn Tool>) {
        self.by_name.insert(tool.spec().name.clone(), tool);
    }

    pub fn get(&self, name: &str) -> Option<&Arc<dyn Tool>> {
        self.by_name.get(name)
    }

    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Arc<dyn Tool>> {
        self.by_name.values()
    }

    /// The same tools as the provider needs to see them.
    pub fn wire(&self) -> Vec<parley::Tool> {
        self.by_name
            .values()
            .map(|t| {
                let spec = t.spec();
                parley::Tool {
                    name: spec.name.clone(),
                    description: spec.description.clone(),
                    schema: spec.schema.clone(),
                }
            })
            .collect()
    }

    /// Whether the batch has to run one at a time.
    pub fn sequential(&self, names: &[String]) -> bool {
        names.iter().any(|n| {
            self.get(n)
                .is_some_and(|t| t.spec().mode == Mode::Sequential)
        })
    }
}

/// Check arguments against a schema, as far as we can without a validator.
///
/// `jsonschema` is not in the offline registry, so this covers the three
/// mistakes a model actually makes: sending something that is not an object,
/// omitting a required property, and inventing one. Anything subtler -- a
/// string where a number belongs, a value outside an enum -- reaches the tool,
/// which is why a tool still has to check what it uses. Saying so here is
/// better than implying a validation that is not happening.
pub fn check(schema: &Value, args: &Value) -> Result<(), String> {
    let Some(object) = args.as_object() else {
        return Err(format!(
            "arguments must be a JSON object, got {}",
            kind_of(args)
        ));
    };
    if let Some(required) = schema.get("required").and_then(Value::as_array) {
        for name in required.iter().filter_map(Value::as_str) {
            if !object.contains_key(name) {
                return Err(format!("missing required property `{name}`"));
            }
        }
    }
    let closed = schema
        .get("additionalProperties")
        .and_then(Value::as_bool)
        .is_some_and(|b| !b);
    if closed {
        if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
            for name in object.keys() {
                if !properties.contains_key(name) {
                    return Err(format!("unknown property `{name}`"));
                }
            }
        }
    }
    Ok(())
}

fn kind_of(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema() -> Value {
        json!({
            "type": "object",
            "properties": { "name": { "type": "string" } },
            "required": ["name"],
            "additionalProperties": false
        })
    }

    #[test]
    fn good_arguments_pass() {
        assert!(check(&schema(), &json!({"name": "Jaipur"})).is_ok());
    }

    #[test]
    fn a_string_where_an_object_belongs_is_named_as_such() {
        // This is what an unparseable tool call turns into upstream, so the
        // message has to read well to a model.
        let e = check(&schema(), &json!("{not json")).unwrap_err();
        assert!(e.contains("must be a JSON object"), "{e}");
        assert!(e.contains("a string"), "{e}");
    }

    #[test]
    fn a_missing_required_property_is_named() {
        let e = check(&schema(), &json!({})).unwrap_err();
        assert!(e.contains("`name`"), "{e}");
    }

    #[test]
    fn an_invented_property_is_refused_when_the_schema_is_closed() {
        let e = check(&schema(), &json!({"name": "x", "zoom": 3})).unwrap_err();
        assert!(e.contains("`zoom`"), "{e}");
    }

    #[test]
    fn an_open_schema_tolerates_extras() {
        let open = json!({"type": "object", "properties": {}, "required": []});
        assert!(check(&open, &json!({"anything": 1})).is_ok());
    }
}
