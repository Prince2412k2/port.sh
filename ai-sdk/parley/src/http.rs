//! One client, and the plumbing from bytes to events.
//!
//! **The TLS setup is not boilerplate.** reqwest's own `rustls` feature pulls
//! `quinn` for HTTP/3, which is not in the offline registry, so this crate uses
//! `rustls-no-provider` and builds the configuration by hand. Two consequences:
//! a crypto provider has to be installed before the first connection, and the
//! root certificates come from `webpki-roots` rather than from reqwest. Neither
//! is optional -- without them the first request fails at run time with an
//! error that says nothing about either.

use std::collections::VecDeque;
use std::sync::{Arc, OnceLock};

use futures_util::stream::{self, BoxStream, StreamExt};

use crate::error::{Error, Result};
use crate::ndjson;
use crate::sse;
use crate::stream::Event;
use crate::wire::{EventStream, Frames};

pub fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let tls = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .expect("ring provides the default protocol versions")
        .with_root_certificates(roots)
        .with_no_client_auth();
        reqwest::Client::builder()
            .use_preconfigured_tls(tls)
            .build()
            .expect("a client with preconfigured TLS and no proxy cannot fail to build")
    })
}

/// Bytes to documents. Which framing a provider uses is its own business, and
/// no parser above this has to know.
pub trait Framer: Send + 'static {
    fn push(&mut self, chunk: &[u8]) -> Vec<sse::Frame>;
    fn finish(&mut self) -> Option<sse::Frame>;
}

impl Framer for sse::Decoder {
    fn push(&mut self, chunk: &[u8]) -> Vec<sse::Frame> {
        sse::Decoder::push(self, chunk)
    }
    fn finish(&mut self) -> Option<sse::Frame> {
        sse::Decoder::finish(self)
    }
}

impl Framer for ndjson::Decoder {
    fn push(&mut self, chunk: &[u8]) -> Vec<sse::Frame> {
        ndjson::Decoder::push(self, chunk)
    }
    fn finish(&mut self) -> Option<sse::Frame> {
        ndjson::Decoder::finish(self)
    }
}

enum State {
    /// Nothing has been sent yet. The request is held so that the POST happens
    /// on the first poll rather than when the stream was constructed -- a
    /// stream nobody polls should not make a paid API call.
    Init(Option<reqwest::RequestBuilder>),
    Body {
        response: reqwest::Response,
    },
    Draining,
    Done,
}

struct Pump<F: Framer, P: Frames> {
    state: State,
    framer: F,
    parser: P,
    queue: VecDeque<Result<Event>>,
}

/// Send the request, frame the response, parse the frames.
///
/// Cancellation is by dropping the returned stream: the response is dropped
/// with it, which closes the connection.
pub fn sse_stream<P: Frames>(request: reqwest::RequestBuilder, parser: P) -> EventStream {
    stream_with(request, sse::Decoder::new(), parser)
}

/// The same, for a provider that streams newline-delimited JSON.
pub fn ndjson_stream<P: Frames>(request: reqwest::RequestBuilder, parser: P) -> EventStream {
    stream_with(request, ndjson::Decoder::new(), parser)
}

pub fn stream_with<F: Framer, P: Frames>(
    request: reqwest::RequestBuilder,
    framer: F,
    parser: P,
) -> EventStream {
    let pump = Pump {
        state: State::Init(Some(request)),
        framer,
        parser,
        queue: VecDeque::new(),
    };
    let s: BoxStream<'static, Result<Event>> =
        stream::unfold(pump, |mut p| async move {
            loop {
                if let Some(event) = p.queue.pop_front() {
                    return Some((event, p));
                }
                match &mut p.state {
                    State::Done => return None,
                    State::Init(request) => {
                        let request = request.take().expect("Init is entered once");
                        match request.send().await {
                            Err(e) => {
                                p.state = State::Done;
                                p.queue.push_back(Err(Error::from(e)));
                            }
                            Ok(response) => {
                                let status = response.status();
                                if !status.is_success() {
                                    let retry_after = response
                                        .headers()
                                        .get("retry-after")
                                        .and_then(|v| v.to_str().ok())
                                        .and_then(|v| v.parse().ok());
                                    let body = response.text().await.unwrap_or_default();
                                    p.state = State::Done;
                                    p.queue.push_back(Err(Error::from_status(
                                        status.as_u16(),
                                        &body,
                                        retry_after,
                                    )));
                                } else {
                                    p.state = State::Body { response };
                                }
                            }
                        }
                    }
                    State::Body { response } => match response.chunk().await {
                        Err(e) => {
                            p.state = State::Done;
                            p.queue.push_back(Err(Error::from(e)));
                        }
                        Ok(None) => {
                            let last = p.framer.finish();
                            let mut events = Vec::new();
                            if let Some(frame) = last {
                                events.extend(p.parser.frame(&frame));
                            }
                            events.extend(p.parser.finish());
                            p.state = State::Draining;
                            p.queue.extend(events);
                        }
                        Ok(Some(bytes)) => {
                            let frames = p.framer.push(&bytes);
                            let mut events = Vec::new();
                            for frame in &frames {
                                events.extend(p.parser.frame(frame));
                            }
                            p.queue.extend(events);
                        }
                    },
                    State::Draining => p.state = State::Done,
                }
            }
        })
        .boxed();
    s
}

/// Feed a recorded SSE body through a parser. No client, no socket.
pub fn replay<P: Frames>(body: &str, parser: P) -> EventStream {
    replay_with(body, sse::Decoder::new(), parser)
}

/// The same, for a recorded newline-delimited body.
pub fn replay_ndjson<P: Frames>(body: &str, parser: P) -> EventStream {
    replay_with(body, ndjson::Decoder::new(), parser)
}

pub fn replay_with<F: Framer, P: Frames>(body: &str, mut framer: F, mut parser: P) -> EventStream {
    let mut events: Vec<Result<Event>> = Vec::new();
    for frame in framer.push(body.as_bytes()) {
        events.extend(parser.frame(&frame));
    }
    if let Some(frame) = framer.finish() {
        events.extend(parser.frame(&frame));
    }
    events.extend(parser.finish());
    stream::iter(events).boxed()
}
