//! Providers that answer without a network.
//!
//! Two of them, and the difference matters. [`Canned`] is handed events
//! directly, which is what a test asserting loop behaviour wants: it says what
//! the model does without caring how any provider spells it. [`Cassette`]
//! replays a *recorded response body* through the real parser, which is what a
//! test asserting protocol behaviour wants -- it exercises the framing and the
//! chunk translation exactly as a live call would.
//!
//! Cassettes are the only way to test the wire code where it is written: every
//! provider host is somebody else's, and a test suite that needs one is a test
//! suite that does not run.

use std::collections::VecDeque;
use std::sync::Mutex;

use futures_util::stream::{self, StreamExt};

use crate::error::{Error, Result};
use crate::stream::Event;
use crate::types::{Api, Cost, Request};
use crate::wire::{EventStream, Wire};

/// Answers with prepared events, one turn per call.
pub struct Canned {
    turns: Mutex<VecDeque<Vec<Result<Event>>>>,
    /// What to do once the script runs out. A loop that asks for more turns than
    /// were scripted is usually a bug in the test, so saying so beats answering
    /// with silence that looks like a finished conversation.
    exhausted: Mutex<usize>,
}

impl Canned {
    pub fn new(turns: Vec<Vec<Event>>) -> Canned {
        Canned {
            turns: Mutex::new(
                turns
                    .into_iter()
                    .map(|t| t.into_iter().map(Ok).collect())
                    .collect(),
            ),
            exhausted: Mutex::new(0),
        }
    }

    /// How many calls arrived after the script ran out.
    pub fn overrun(&self) -> usize {
        *self.exhausted.lock().unwrap()
    }
}

impl Wire for Canned {
    fn stream(&self, _request: Request) -> EventStream {
        match self.turns.lock().unwrap().pop_front() {
            Some(events) => stream::iter(events).boxed(),
            None => {
                *self.exhausted.lock().unwrap() += 1;
                stream::iter(vec![Err(Error::Malformed(
                    "the canned provider ran out of scripted turns".into(),
                ))])
                .boxed()
            }
        }
    }
}

/// Replays a recorded response body through the parser for its api.
pub struct Cassette {
    api: Api,
    body: String,
    cost: Cost,
}

impl Cassette {
    pub fn new(api: Api, body: impl Into<String>) -> Cassette {
        Cassette {
            api,
            body: body.into(),
            cost: Cost::default(),
        }
    }

    /// Read a recording from disk. Paths are relative to the caller, so tests
    /// usually build them from `CARGO_MANIFEST_DIR`.
    pub fn from_file(api: Api, path: impl AsRef<std::path::Path>) -> std::io::Result<Cassette> {
        Ok(Cassette::new(api, std::fs::read_to_string(path)?))
    }

    /// Price the recorded usage as if this model had produced it.
    pub fn priced(mut self, cost: Cost) -> Cassette {
        self.cost = cost;
        self
    }
}

impl Wire for Cassette {
    fn stream(&self, _request: Request) -> EventStream {
        match self.api {
            Api::OpenaiCompletions => crate::http::replay(
                &self.body,
                crate::api::openai_completions::Parser::new(self.cost),
            ),
            Api::OpenaiResponses => crate::http::replay(
                &self.body,
                crate::api::openai_responses::Parser::new(self.cost),
            ),
            // Framed by lines rather than by blank-line dispatch.
            Api::OllamaChat => crate::http::replay_ndjson(
                &self.body,
                crate::api::ollama_chat::Parser::new(self.cost),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Endpoint, Model, Options, Stop};
    use crate::Accumulator;

    fn request() -> Request {
        Request {
            model: Model {
                id: "m".into(),
                name: "m".into(),
                provider: "p".into(),
                api: Api::OpenaiCompletions,
                context_window: 1000,
                max_output: None,
                reasoning: false,
                cost: Cost::default(),
            },
            context: Default::default(),
            endpoint: Endpoint::default(),
            options: Options::default(),
        }
    }

    #[tokio::test]
    async fn a_cassette_goes_through_the_real_parser() {
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"replayed\"},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        let wire = Cassette::new(Api::OpenaiCompletions, body);
        let mut acc = Accumulator::new();
        let mut events = wire.stream(request());
        while let Some(event) = events.next().await {
            acc.apply(&event.unwrap());
        }
        let message = acc.finish();
        assert_eq!(message.text(), "replayed");
        assert_eq!(message.stop, Stop::End);
    }

    #[tokio::test]
    async fn running_out_of_script_is_reported_rather_than_looking_finished() {
        let wire = Canned::new(vec![vec![Event::Done { stop: Stop::End }]]);
        let _ = wire.stream(request()).next().await;
        let second = wire.stream(request()).next().await.unwrap();
        assert!(second.is_err());
        assert_eq!(wire.overrun(), 1);
    }
}
