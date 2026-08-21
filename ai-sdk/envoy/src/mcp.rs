//! Somebody else's tools.
//!
//! MCP is JSON-RPC too, so this reuses the same [`Peer`] the protocol side uses
//! and differs only in what it says: `initialize`, then `tools/list`, then
//! `tools/call` for each invocation. A server's tools arrive as `Arc<dyn Tool>`
//! and the loop cannot tell them from native ones.
//!
//! **Two transports, one protocol.** A local server is a child process spoken to
//! over pipes; a remote one is a URL that answers either with a JSON body or
//! with a server-sent-event stream, depending on how it feels. Both are a
//! [`Transport`], and everything above that trait is shared -- the handshake,
//! the tool list, and the tool wrapper do not know which they have.
//!
//! Note what is *not* here: `portfolio`'s own two tools do not come back through
//! MCP. They are the client's, and they arrive over the protocol extension
//! instead -- no listener, no token, no second copy of a gazetteer.

use std::sync::Arc;

use async_trait::async_trait;
use parley::types::Block;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

use crate::acp::rpc::{self, Error, Incoming, Peer};
use crate::tool::{Cx, Failed, Kind, Mode, Output, Spec, Tool};

/// The protocol revision we ask for. A server that speaks a later one answers
/// with its own, which is recorded rather than argued with.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// Somewhere to send JSON-RPC.
#[async_trait]
pub trait Transport: Send + Sync {
    async fn request(&self, method: &str, params: Value) -> Result<Value, Error>;
    async fn notify(&self, method: &str, params: Value);
}

pub struct Connection {
    name: String,
    peer: Arc<Peer>,
    /// Held so the child is not reaped while its tools are still registered.
    _child: Option<tokio::process::Child>,
}

impl Connection {
    /// Talk MCP over any pair of pipes.
    pub fn over<R, W>(name: impl Into<String>, input: R, output: W) -> Arc<Connection>
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        Connection::with_child(name, input, output, None)
    }

    fn with_child<R, W>(
        name: impl Into<String>,
        input: R,
        output: W,
        child: Option<tokio::process::Child>,
    ) -> Arc<Connection>
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        tokio::spawn(async move {
            let mut output = output;
            while let Some(line) = rx.recv().await {
                if output.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
                if output.write_all(b"\n").await.is_err() {
                    break;
                }
                let _ = output.flush().await;
            }
        });

        let connection = Arc::new(Connection {
            name: name.into(),
            peer: Arc::new(Peer::new(tx)),
            _child: child,
        });

        let peer = connection.peer.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(input).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                match rpc::parse(&line) {
                    Some(Incoming::Response { id, result }) => peer.resolve(&id, result),
                    // Servers may send progress notifications and log messages.
                    // Nothing here acts on them yet, and dropping one is better
                    // than refusing to speak to a server that sends them.
                    _ => {}
                }
            }
        });

        connection
    }

    /// Start a server as a child process.
    pub async fn spawn(
        name: impl Into<String>,
        command: &str,
        args: &[String],
    ) -> Result<Arc<Connection>, Failed> {
        let mut child = tokio::process::Command::new(command)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            // The server's diagnostics belong on our stderr, not mixed into a
            // protocol stream.
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .map_err(|e| Failed::new(format!("cannot start `{command}`: {e}")))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| Failed::new("the child has no stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Failed::new("the child has no stdout"))?;
        Ok(Connection::with_child(name, stdout, stdin, Some(child)))
    }

    /// Handshake, then ask what it can do.
    pub async fn tools(self: &Arc<Self>) -> Result<Vec<Arc<dyn Tool>>, Failed> {
        let name = self.name.clone();
        tools(self.clone(), &name).await
    }
}

#[async_trait]
impl Transport for Connection {
    async fn request(&self, method: &str, params: Value) -> Result<Value, Error> {
        self.peer.request(method, params).await
    }

    async fn notify(&self, method: &str, params: Value) {
        self.peer.notify(method, params);
    }
}

/// A server reachable over HTTP.
///
/// The streamable transport lets a server answer a POST with either a JSON body
/// or an event stream, and does not say which in advance. So both are accepted:
/// a JSON body is the response, and a stream is read until the frame carrying
/// our id arrives. A session id handed out at `initialize` is echoed on every
/// later request, because a server that issued one will refuse requests without
/// it.
pub struct Http {
    name: String,
    url: String,
    headers: Vec<(String, String)>,
    session: std::sync::Mutex<Option<String>>,
    next: std::sync::atomic::AtomicI64,
}

impl Http {
    pub fn new(
        name: impl Into<String>,
        url: impl Into<String>,
        headers: Vec<(String, String)>,
    ) -> Arc<Http> {
        Arc::new(Http {
            name: name.into(),
            url: url.into(),
            headers,
            session: std::sync::Mutex::new(None),
            next: std::sync::atomic::AtomicI64::new(1),
        })
    }

    pub async fn tools(self: &Arc<Self>) -> Result<Vec<Arc<dyn Tool>>, Failed> {
        let name = self.name.clone();
        tools(self.clone(), &name).await
    }

    fn post(&self, body: &Value) -> reqwest::RequestBuilder {
        let mut builder = parley::http::client()
            .post(&self.url)
            .header("content-type", "application/json")
            // Either is acceptable, which is the whole awkwardness of this
            // transport.
            .header("accept", "application/json, text/event-stream")
            .json(body);
        for (key, value) in &self.headers {
            builder = builder.header(key, value);
        }
        if let Some(session) = self.session.lock().unwrap().as_ref() {
            builder = builder.header("mcp-session-id", session);
        }
        builder
    }
}

#[async_trait]
impl Transport for Http {
    async fn request(&self, method: &str, params: Value) -> Result<Value, Error> {
        let id = self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let body = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let response = self
            .post(&body)
            .send()
            .await
            .map_err(|e| Error::new(rpc::INTERNAL_ERROR, format!("{}: {e}", self.name)))?;

        if let Some(session) = response
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
        {
            *self.session.lock().unwrap() = Some(session.to_string());
        }
        let status = response.status();
        let kind = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let text = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(Error::new(
                rpc::INTERNAL_ERROR,
                format!("{}: HTTP {status}: {}", self.name, first_line(&text)),
            ));
        }

        let documents: Vec<Value> = if kind.contains("text/event-stream") {
            let mut decoder = parley::sse::Decoder::new();
            let mut frames = decoder.push(text.as_bytes());
            frames.extend(decoder.finish());
            frames
                .iter()
                .filter_map(|f| serde_json::from_str(&f.data).ok())
                .collect()
        } else {
            serde_json::from_str::<Value>(&text)
                .map(|v| vec![v])
                .unwrap_or_default()
        };

        // Servers interleave notifications with the answer, so the answer is the
        // document carrying our id rather than simply the last one.
        let mine = documents
            .into_iter()
            .find(|d| d.get("id").and_then(Value::as_i64) == Some(id))
            .ok_or_else(|| {
                Error::new(
                    rpc::INTERNAL_ERROR,
                    format!("{}: no answer to `{method}`", self.name),
                )
            })?;
        match mine.get("error") {
            Some(e) => Err(Error {
                code: e.get("code").and_then(Value::as_i64).unwrap_or(rpc::INTERNAL_ERROR),
                message: e
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("no message")
                    .to_string(),
            }),
            None => Ok(mine.get("result").cloned().unwrap_or(Value::Null)),
        }
    }

    async fn notify(&self, method: &str, params: Value) {
        let body = json!({"jsonrpc": "2.0", "method": method, "params": params});
        // A notification has no answer, so a failure here is only worth
        // reporting if somebody is watching stderr.
        if let Err(e) = self.post(&body).send().await {
            eprintln!("envoy: mcp `{}`: {method}: {e}", self.name);
        }
    }
}

fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or("").chars().take(200).collect()
}

/// Handshake with a server, then wrap what it offers as tools.
pub async fn tools(
    transport: Arc<dyn Transport>,
    name: &str,
) -> Result<Vec<Arc<dyn Tool>>, Failed> {
    {
        let _hello = transport
            .request(
                "initialize",
                json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": { "name": "envoy", "version": env!("CARGO_PKG_VERSION") }
                }),
            )
            .await
            .map_err(|e| Failed::new(format!("{name}: initialize failed: {}", e.message)))?;
    }
    // MCP requires this before any other request; a server is within its rights
    // to refuse everything until it arrives.
    transport.notify("notifications/initialized", json!({})).await;

    let listed = transport
        .request("tools/list", json!({}))
        .await
        .map_err(|e| Failed::new(format!("{name}: tools/list failed: {}", e.message)))?;

    let Some(described) = listed.get("tools").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    Ok(described
        .iter()
        .filter_map(|one| {
            let tool_name = one.get("name").and_then(Value::as_str)?.to_string();
            let description = one
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let title = one
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or(&tool_name)
                .to_string();
            let schema = one
                .get("inputSchema")
                .cloned()
                .unwrap_or_else(|| json!({"type": "object", "properties": {}, "required": []}));
            Some(Arc::new(Remote {
                spec: Spec {
                    name: tool_name,
                    title,
                    description,
                    schema,
                    kind: Kind::Other,
                    mode: Mode::Parallel,
                },
                transport: transport.clone(),
            }) as Arc<dyn Tool>)
        })
        .collect())
}

struct Remote {
    spec: Spec,
    transport: Arc<dyn Transport>,
}

#[async_trait]
impl Tool for Remote {
    fn spec(&self) -> &Spec {
        &self.spec
    }

    async fn call(&self, args: Value, cx: Cx) -> Result<Output, Failed> {
        let answer = tokio::select! {
            biased;
            _ = cx.cancel.cancelled() => return Err(Failed::new("cancelled")),
            answer = self.transport.request(
                "tools/call",
                json!({ "name": self.spec.name, "arguments": args }),
            ) => answer,
        };
        match answer {
            Err(Error { message, .. }) => Err(Failed::new(message)),
            Ok(value) => {
                let text = text_of(&value);
                // MCP reports a tool's own failure in the result rather than as
                // a JSON-RPC error, which is the same distinction this codebase
                // makes: the tool failed, the call did not.
                if value
                    .get("isError")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    return Err(Failed::new(
                        text.unwrap_or_else(|| "the server reported a failure".into()),
                    ));
                }
                Ok(Output {
                    content: vec![Block::text(text.unwrap_or_default())],
                    raw: value.get("structuredContent").cloned(),
                })
            }
        }
    }
}

fn text_of(value: &Value) -> Option<String> {
    let blocks = value.get("content").and_then(Value::as_array)?;
    let joined: String = blocks
        .iter()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|b| b.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n");
    (!joined.is_empty()).then_some(joined)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{duplex, DuplexStream};
    use tokio_util::sync::CancellationToken;

    /// A server that answers from a script, so the transport is exercised
    /// without needing anybody's MCP server installed.
    fn fake(answers: Vec<(&'static str, Value)>) -> (Arc<Connection>, tokio::task::JoinHandle<()>) {
        let (client_side, server_side) = duplex(16 * 1024);
        let (server_out, client_in) = duplex(16 * 1024);
        let job = tokio::spawn(async move {
            let mut lines = BufReader::new(server_side).lines();
            let mut out: DuplexStream = server_out;
            while let Ok(Some(line)) = lines.next_line().await {
                let message: Value = match serde_json::from_str(&line) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let Some(method) = message.get("method").and_then(Value::as_str) else {
                    continue;
                };
                let Some(id) = message.get("id") else {
                    continue; // a notification
                };
                let reply = answers
                    .iter()
                    .find(|(m, _)| *m == method)
                    .map(|(_, r)| r.clone())
                    .unwrap_or(Value::Null);
                let text = json!({"jsonrpc": "2.0", "id": id, "result": reply}).to_string();
                let _ = out.write_all(text.as_bytes()).await;
                let _ = out.write_all(b"\n").await;
                let _ = out.flush().await;
            }
        });
        (Connection::over("fake", client_in, client_side), job)
    }

    #[tokio::test]
    async fn tools_are_listed_and_described() {
        let (connection, _job) = fake(vec![
            ("initialize", json!({"protocolVersion": PROTOCOL_VERSION})),
            (
                "tools/list",
                json!({"tools": [
                    {
                        "name": "read_file",
                        "title": "Read a file",
                        "description": "read one",
                        "inputSchema": {"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]}
                    },
                    { "description": "no name, skipped" }
                ]}),
            ),
        ]);
        let tools = connection.tools().await.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].spec().name, "read_file");
        assert_eq!(tools[0].spec().title, "Read a file");
        assert_eq!(tools[0].spec().schema["required"][0], "path");
    }

    #[tokio::test]
    async fn a_call_returns_the_text_content() {
        let (connection, _job) = fake(vec![
            ("initialize", json!({})),
            (
                "tools/list",
                json!({"tools": [{"name": "t", "inputSchema": {"type": "object"}}]}),
            ),
            (
                "tools/call",
                json!({"content": [{"type": "text", "text": "line one"}, {"type": "text", "text": "line two"}],
                       "structuredContent": {"n": 2}}),
            ),
        ]);
        let tools = connection.tools().await.unwrap();
        let (tx, _rx) = mpsc::unbounded_channel();
        let cx = Cx::new("c1".into(), CancellationToken::new(), tx);
        let output = tools[0].call(json!({}), cx).await.unwrap();
        assert_eq!(output.content[0].as_text(), Some("line one\nline two"));
        assert_eq!(output.raw, Some(json!({"n": 2})));
    }

    #[tokio::test]
    async fn a_server_reporting_is_error_becomes_a_tool_failure() {
        let (connection, _job) = fake(vec![
            ("initialize", json!({})),
            ("tools/list", json!({"tools": [{"name": "t"}]})),
            (
                "tools/call",
                json!({"isError": true, "content": [{"type": "text", "text": "no such path"}]}),
            ),
        ]);
        let tools = connection.tools().await.unwrap();
        let (tx, _rx) = mpsc::unbounded_channel();
        let cx = Cx::new("c1".into(), CancellationToken::new(), tx);
        let failed = tools[0].call(json!({}), cx).await.unwrap_err();
        assert_eq!(failed.0, "no such path");
    }

    #[tokio::test]
    async fn a_server_with_no_tools_is_not_an_error() {
        let (connection, _job) = fake(vec![("initialize", json!({})), ("tools/list", json!({}))]);
        assert!(connection.tools().await.unwrap().is_empty());
    }

    /// A minimal HTTP server that answers each POST from a script. Real socket,
    /// real headers, because the awkward part of this transport is the headers.
    async fn http_server(
        replies: Vec<(&'static str, bool)>,
    ) -> (String, tokio::task::JoinHandle<()>) {
        use tokio::io::AsyncReadExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        let job = tokio::spawn(async move {
            for (body, as_sse) in replies {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let mut buffer = vec![0u8; 8192];
                let _ = socket.read(&mut buffer).await;
                let (kind, payload) = if as_sse {
                    ("text/event-stream", format!("data: {body}\n\n"))
                } else {
                    ("application/json", body.to_string())
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: {kind}\r\nmcp-session-id: sess-1\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{payload}",
                    payload.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.flush().await;
            }
        });
        (url, job)
    }

    #[tokio::test]
    async fn http_reads_an_answer_from_a_json_body() {
        let (url, _job) = http_server(vec![
            (r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-06-18"}}"#, false),
            (r#"{"jsonrpc":"2.0","id":2,"result":{}}"#, false), // the notification POST
            (r#"{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"remote_tool","inputSchema":{"type":"object"}}]}}"#, false),
        ])
        .await;
        let http = Http::new("remote", url, vec![]);
        let tools = http.tools().await.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].spec().name, "remote_tool");
        // The session id the server issued is remembered for later requests.
        assert_eq!(http.session.lock().unwrap().as_deref(), Some("sess-1"));
    }

    #[tokio::test]
    async fn http_reads_an_answer_out_of_an_event_stream() {
        // The same transport may answer either way, and does not say in advance.
        let (url, _job) = http_server(vec![
            (r#"{"jsonrpc":"2.0","id":1,"result":{}}"#, true),
            (r#"{}"#, true),
            (r#"{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"streamed"}]}}"#, true),
        ])
        .await;
        let tools = Http::new("remote", url, vec![]).tools().await.unwrap();
        assert_eq!(tools[0].spec().name, "streamed");
    }

    #[tokio::test]
    async fn http_reports_a_jsonrpc_error_from_the_server() {
        let (url, _job) = http_server(vec![(
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32000,"message":"no such server"}}"#,
            false,
        )])
        .await;
        let e = match Http::new("remote", url, vec![]).tools().await {
            Err(e) => e,
            Ok(_) => panic!("should have failed"),
        };
        assert!(e.0.contains("no such server"), "{}", e.0);
        assert!(e.0.contains("remote"), "the server is named: {}", e.0);
    }

    #[tokio::test]
    async fn http_that_cannot_be_reached_names_the_server() {
        // Port 1 on loopback: nothing is listening, and nothing will be.
        let e = match Http::new("remote", "http://127.0.0.1:1/mcp", vec![])
            .tools()
            .await
        {
            Err(e) => e,
            Ok(_) => panic!("should have failed"),
        };
        assert!(e.0.contains("remote"), "{}", e.0);
    }

    #[tokio::test]
    async fn starting_a_command_that_does_not_exist_says_which() {
        let e = match Connection::spawn("mcp", "definitely-not-a-real-binary-xyz", &[]).await {
            Err(e) => e,
            Ok(_) => panic!("that binary should not exist"),
        };
        assert!(e.0.contains("definitely-not-a-real-binary-xyz"), "{}", e.0);
    }
}
