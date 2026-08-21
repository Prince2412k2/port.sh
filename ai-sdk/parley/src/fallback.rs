//! Trying the next model when this one stops answering.
//!
//! `health.rs` in `portfolio` already keeps an ordered list of things to try;
//! today falling through it means starting a new agent process, which is why
//! there is a struct in `acp.rs` whose only job is to survive that. Here it is a
//! wire that wraps other wires, so the loop cannot tell a tier fell through and
//! the conversation never leaves memory.
//!
//! **Only retryable failures fall through.** A 400 means our request was wrong,
//! and trying it against two more providers turns one clear error message into
//! three vague ones. `Error::retryable` draws that line.
//!
//! **And only before anything has been said.** Once a delta has reached the
//! client, restarting elsewhere would replay a partial answer on top of itself.
//! A failure after that point is the turn's failure, and the loop records it as
//! one.

use std::collections::VecDeque;
use std::sync::Arc;

use futures_util::stream::{self, StreamExt};

use crate::error::Error;
use crate::stream::Event;
use crate::types::{Endpoint, Model, Request, Tuning};
use crate::wire::{EventStream, Wire};

pub struct Tier {
    pub wire: Arc<dyn Wire>,
    pub model: Model,
    pub endpoint: Endpoint,
    pub tuning: Tuning,
}

/// The longest we will sit on a rate limit before trying the next tier.
///
/// A provider asking for two minutes is not worth waiting for when somebody is
/// looking at a terminal: the next tier is a slower answer, and a blank screen
/// is not an answer at all.
pub const MAX_WAIT: u64 = 20;

pub struct Fallback {
    tiers: Vec<Tier>,
}

impl Fallback {
    pub fn new(tiers: Vec<Tier>) -> Fallback {
        Fallback { tiers }
    }

    pub fn first(&self) -> Option<&Tier> {
        self.tiers.first()
    }
}

impl Wire for Fallback {
    fn stream(&self, request: Request) -> EventStream {
        if self.tiers.is_empty() {
            return stream::iter(vec![Err(Error::NoCredential(
                "no models are configured".into(),
            ))])
            .boxed();
        }
        // Each attempt needs its own owned copy of everything, since the stream
        // outlives this call.
        let attempts: Vec<Attempt> = self
            .tiers
            .iter()
            .map(|t| Attempt {
                wire: t.wire.clone(),
                model: t.model.clone(),
                endpoint: t.endpoint.clone(),
                tuning: t.tuning.clone(),
            })
            .collect();

        stream::unfold(
            State {
                attempts,
                at: 0,
                current: None,
                request,
                spoke: false,
                announced: false,
                waited: false,
                queue: VecDeque::new(),
            },
            |mut state| async move {
                loop {
                    if let Some(event) = state.queue.pop_front() {
                        return Some((event, state));
                    }
                    if state.current.is_none() {
                        let Some(attempt) = state.attempts.get(state.at).cloned() else {
                            return None;
                        };
                        let mut request = state.request.clone();
                        request.model = attempt.model.clone();
                        request.endpoint = attempt.endpoint;
                        attempt.tuning.apply(&mut request.options);
                        state.current = Some((attempt.wire.stream(request), attempt.model));
                        state.spoke = false;
                    }
                    let (events, model) = state.current.as_mut().expect("just set");
                    match events.next().await {
                        // A stream that ends without an error is this tier's
                        // answer, whatever it amounted to.
                        None => return None,
                        Some(Ok(event)) => {
                            if matches!(
                                event,
                                Event::Text { .. } | Event::Thinking { .. } | Event::ToolArgs { .. }
                            ) {
                                state.spoke = true;
                            }
                            if state.at > 0 && !state.announced {
                                state.announced = true;
                                state.queue.push_back(Ok(Event::Model {
                                    provider: model.provider.clone(),
                                    model: model.id.clone(),
                                }));
                            }
                            state.queue.push_back(Ok(event));
                        }
                        Some(Err(e)) => {
                            // A provider that said when to come back is worth
                            // obeying once. Falling straight through spends the
                            // next tier on a limit that was going to clear.
                            let wait = match &e {
                                Error::RateLimited { retry_after: Some(s), .. }
                                    if !state.waited && *s <= MAX_WAIT && !state.spoke =>
                                {
                                    Some(*s)
                                }
                                _ => None,
                            };
                            if let Some(seconds) = wait {
                                state.waited = true;
                                state.current = None;
                                tokio::time::sleep(std::time::Duration::from_secs(seconds)).await;
                                continue;
                            }
                            let last = state.at + 1 >= state.attempts.len();
                            if last || !e.retryable() || state.spoke {
                                state.queue.push_back(Err(e));
                            } else {
                                state.at += 1;
                                state.waited = false;
                                state.current = None;
                            }
                        }
                    }
                }
            },
        )
        .boxed()
    }
}

#[derive(Clone)]
struct Attempt {
    wire: Arc<dyn Wire>,
    model: Model,
    endpoint: Endpoint,
    tuning: Tuning,
}

struct State {
    attempts: Vec<Attempt>,
    at: usize,
    current: Option<(EventStream, Model)>,
    request: Request,
    /// Whether this attempt has produced anything a client could have seen.
    spoke: bool,
    announced: bool,
    /// Whether this tier has already been given its one rate-limit wait.
    waited: bool,
    queue: VecDeque<Result<Event, Error>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Api, Context, Cost, Options, Stop};
    use crate::Canned;

    fn model(id: &str) -> Model {
        Model {
            id: id.into(),
            name: id.into(),
            provider: format!("{id}-co"),
            api: Api::OpenaiCompletions,
            context_window: 1000,
            max_output: None,
            reasoning: false,
            cost: Cost::default(),
        }
    }

    fn tier(id: &str, turns: Vec<Vec<Event>>) -> Tier {
        Tier {
            wire: Arc::new(Canned::new(turns)),
            model: model(id),
            endpoint: Endpoint::default(),
            tuning: Tuning::default(),
        }
    }

    /// A wire that always fails with the given error.
    struct Fails(Error);
    impl Wire for Fails {
        fn stream(&self, _request: Request) -> EventStream {
            let e = match &self.0 {
                Error::RateLimited { message, .. } => Error::RateLimited {
                    message: message.clone(),
                    retry_after: None,
                },
                Error::Rejected { status, message } => Error::Rejected {
                    status: *status,
                    message: message.clone(),
                },
                other => Error::Transport(other.to_string()),
            };
            stream::iter(vec![Err(e)]).boxed()
        }
    }

    fn failing(id: &str, error: Error) -> Tier {
        Tier {
            wire: Arc::new(Fails(error)),
            model: model(id),
            endpoint: Endpoint::default(),
            tuning: Tuning::default(),
        }
    }

    fn request() -> Request {
        Request {
            model: model("unused"),
            context: Context::default(),
            endpoint: Endpoint::default(),
            options: Options::default(),
        }
    }

    async fn collect(wire: &Fallback) -> Vec<Result<Event, Error>> {
        wire.stream(request()).collect().await
    }

    fn answer(text: &str) -> Vec<Event> {
        vec![
            Event::Text { index: 0, delta: text.into() },
            Event::Done { stop: Stop::End },
        ]
    }

    #[tokio::test]
    async fn the_first_tier_answers_and_nothing_is_announced() {
        let wire = Fallback::new(vec![tier("a", vec![answer("from a")])]);
        let events = collect(&wire).await;
        assert!(!events.iter().any(|e| matches!(e, Ok(Event::Model { .. }))));
        assert!(matches!(&events[0], Ok(Event::Text { delta, .. }) if delta == "from a"));
    }

    #[tokio::test]
    async fn a_rate_limited_tier_falls_through_and_says_which_model_answered() {
        let wire = Fallback::new(vec![
            failing("a", Error::RateLimited { message: "slow down".into(), retry_after: None }),
            tier("b", vec![answer("from b")]),
        ]);
        let events = collect(&wire).await;
        // The notice comes before the text, so a client relabels the model
        // before it renders anything attributed to it.
        assert!(matches!(
            &events[0],
            Ok(Event::Model { model, .. }) if model == "b"
        ));
        assert!(matches!(&events[1], Ok(Event::Text { delta, .. }) if delta == "from b"));
    }

    #[tokio::test]
    async fn a_rejected_request_does_not_fall_through() {
        // Our own mistake. Trying it twice more would bury the message that
        // says what was wrong.
        let wire = Fallback::new(vec![
            failing("a", Error::Rejected { status: 400, message: "bad tool schema".into() }),
            tier("b", vec![answer("never reached")]),
        ]);
        let events = collect(&wire).await;
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], Err(Error::Rejected { message, .. }) if message == "bad tool schema"));
    }

    #[tokio::test]
    async fn a_failure_after_the_answer_started_is_not_retried_elsewhere() {
        // Restarting here would replay a partial answer on top of itself.
        let half = vec![
            Event::Text { index: 0, delta: "half an ans".into() },
        ];
        let broken = Tier {
            wire: Arc::new(Canned::new(vec![half])),
            model: model("a"),
            endpoint: Endpoint::default(),
            tuning: Tuning::default(),
        };
        // Canned yields the events then, on the *next* turn, an error -- so
        // drive two tiers and assert the second never speaks.
        let wire = Fallback::new(vec![broken, tier("b", vec![answer("from b")])]);
        let events = collect(&wire).await;
        let text: Vec<String> = events
            .iter()
            .filter_map(|e| match e {
                Ok(Event::Text { delta, .. }) => Some(delta.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(text, vec!["half an ans"]);
    }

    /// Fails with a rate limit the first time, answers the second.
    struct LimitedOnce {
        calls: std::sync::Mutex<usize>,
        retry_after: Option<u64>,
    }
    impl Wire for LimitedOnce {
        fn stream(&self, _request: Request) -> EventStream {
            let mut calls = self.calls.lock().unwrap();
            *calls += 1;
            if *calls == 1 {
                return stream::iter(vec![Err(Error::RateLimited {
                    message: "slow down".into(),
                    retry_after: self.retry_after,
                })])
                .boxed();
            }
            stream::iter(answer("second try").into_iter().map(Ok)).boxed()
        }
    }

    /// Records what it was asked for, so tuning can be asserted on.
    struct Records(std::sync::Mutex<Option<Options>>);
    impl Wire for Records {
        fn stream(&self, request: Request) -> EventStream {
            *self.0.lock().unwrap() = Some(request.options.clone());
            stream::iter(answer("ok").into_iter().map(Ok)).boxed()
        }
    }

    #[tokio::test]
    async fn a_short_retry_after_is_waited_out_on_the_same_tier() {
        let limited = Arc::new(LimitedOnce {
            calls: std::sync::Mutex::new(0),
            retry_after: Some(0),
        });
        let wire = Fallback::new(vec![
            Tier {
                wire: limited.clone(),
                model: model("a"),
                endpoint: Endpoint::default(),
                tuning: Tuning::default(),
            },
            tier("b", vec![answer("from b")]),
        ]);
        let events = collect(&wire).await;
        let text: Vec<String> = events
            .iter()
            .filter_map(|e| match e {
                Ok(Event::Text { delta, .. }) => Some(delta.clone()),
                _ => None,
            })
            .collect();
        // The first tier answered on its second attempt, so the second tier was
        // never spent and no switch was announced.
        assert_eq!(text, vec!["second try"]);
        assert_eq!(*limited.calls.lock().unwrap(), 2);
        assert!(!events.iter().any(|e| matches!(e, Ok(Event::Model { .. }))));
    }

    #[tokio::test]
    async fn a_long_retry_after_is_not_waited_for() {
        // Nobody watching a terminal wants to wait two minutes; the next tier
        // is a slower answer, and a blank screen is not an answer at all.
        let limited = Arc::new(LimitedOnce {
            calls: std::sync::Mutex::new(0),
            retry_after: Some(MAX_WAIT + 1),
        });
        let wire = Fallback::new(vec![
            Tier {
                wire: limited.clone(),
                model: model("a"),
                endpoint: Endpoint::default(),
                tuning: Tuning::default(),
            },
            tier("b", vec![answer("from b")]),
        ]);
        let events = collect(&wire).await;
        assert!(events
            .iter()
            .any(|e| matches!(e, Ok(Event::Text { delta, .. }) if delta == "from b")));
        assert_eq!(*limited.calls.lock().unwrap(), 1, "no second attempt");
    }

    #[tokio::test]
    async fn a_tier_applies_its_own_tuning_without_touching_the_session_key() {
        let records = Arc::new(Records(std::sync::Mutex::new(None)));
        let wire = Fallback::new(vec![Tier {
            wire: records.clone(),
            model: model("a"),
            endpoint: Endpoint::default(),
            tuning: Tuning {
                temperature: Some(0.2),
                effort: Some(crate::types::Effort::Low),
                max_output: Some(4096),
            },
        }]);
        let mut request = request();
        request.options.cache_key = Some("session-7".into());
        let _: Vec<_> = wire.stream(request).collect().await;
        let seen = records.0.lock().unwrap().clone().expect("a request");
        assert_eq!(seen.temperature, Some(0.2));
        assert_eq!(seen.effort, crate::types::Effort::Low);
        assert_eq!(seen.max_output, Some(4096));
        // The session's cache key is not a tier's business.
        assert_eq!(seen.cache_key.as_deref(), Some("session-7"));
    }

    #[tokio::test]
    async fn the_last_tier_reports_its_own_failure() {
        let wire = Fallback::new(vec![
            failing("a", Error::Transport("no route".into())),
            failing("b", Error::Transport("no route either".into())),
        ]);
        let events = collect(&wire).await;
        assert!(matches!(events.last(), Some(Err(Error::Transport(_)))));
    }

    #[tokio::test]
    async fn no_tiers_at_all_is_an_error_rather_than_silence() {
        let events = collect(&Fallback::new(vec![])).await;
        assert!(matches!(&events[0], Err(Error::NoCredential(_))));
    }
}
