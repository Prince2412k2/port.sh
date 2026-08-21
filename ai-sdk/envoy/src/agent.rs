//! The turn loop.
//!
//! One turn is one model call plus whatever tools it asked for. The loop is:
//! stream a message, collect its tool calls, run them, append the results, go
//! again -- until the model asks for nothing, or a ceiling is reached, or
//! somebody stops it. Everything the client sees comes out of here as an
//! [`Event`], in the order it happened.
//!
//! **It is a stream, not a task.** Nothing is spawned, so dropping the returned
//! stream drops the in-flight request and every running tool with it. That is
//! what makes cancellation honest: there is no detached work left writing to a
//! channel nobody reads. `session/cancel` is handled by the layer above, which
//! cancels the token and then stops polling.
//!
//! **Tool calls are announced before any of them run.** A model that asks for
//! three things at once should show up on screen as three things at once, rather
//! than appearing one at a time as each finishes and making a parallel batch
//! look sequential.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use async_stream::stream;
use futures_core::Stream;
use futures_util::stream::FuturesUnordered;
use futures_util::StreamExt;
use parley::stream::Event as Wired;
use parley::types::{Block, Context, Endpoint, Message, Model, Options, Request, Stop};
use parley::{Accumulator, Wire};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::budget::{Budget, Spent};
use crate::event::{End, Event};
use crate::tool::{self, Cx, Failed, Kind, Output, Set, Tool};

pub struct Config {
    pub wire: Arc<dyn Wire>,
    pub model: Model,
    pub endpoint: Endpoint,
    pub options: Options,
    pub tools: Arc<Set>,
    pub budget: Budget,
    pub system: Option<String>,
}

/// One job in a batch of tool calls.
struct Job {
    id: String,
    name: String,
    title: String,
    kind: Kind,
    args: Value,
    /// `None` when the call was refused before it ran, in which case `refused`
    /// says why.
    tool: Option<Arc<dyn Tool>>,
    refused: Option<String>,
}

/// Run a conversation forward.
///
/// `messages` is the whole history including the prompt that starts this run.
/// The loop does not mutate the caller's copy: `Event::Ended` carries exactly
/// what to append, so a caller that abandons a run has nothing to undo.
pub fn run(
    cfg: Arc<Config>,
    messages: Vec<Message>,
    cancel: CancellationToken,
) -> impl Stream<Item = Event> {
    stream! {
        let mut messages = messages;
        let start = messages.len();
        let mut spent = Spent::default();
        let mut cost = 0.0;

        loop {
            if spent.turns >= cfg.budget.turns {
                yield Event::Ended { reason: End::MaxTurns, appended: messages[start..].to_vec() };
                return;
            }
            spent.turns += 1;
            yield Event::TurnStart { turn: spent.turns };

            let request = Request {
                model: cfg.model.clone(),
                context: Context {
                    system: cfg.system.clone(),
                    messages: messages.clone(),
                    tools: cfg.tools.wire(),
                },
                endpoint: cfg.endpoint.clone(),
                options: cfg.options.clone(),
            };

            let mut wire = cfg.wire.stream(request);
            let mut acc = Accumulator::new();
            let mut failure: Option<String> = None;

            loop {
                let mut stopped = false;
                let next = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => { stopped = true; None }
                    n = wire.next() => n,
                };
                if stopped {
                    yield Event::Ended { reason: End::Cancelled, appended: messages[start..].to_vec() };
                    return;
                }
                let Some(item) = next else { break };
                match item {
                    Err(e) => { failure = Some(e.to_string()); break }
                    Ok(event) => {
                        acc.apply(&event);
                        match event {
                            Wired::Text { index, delta } => yield Event::Text { index, delta },
                            Wired::Thinking { index, delta } => yield Event::Thinking { index, delta },
                            _ => {}
                        }
                    }
                }
            }

            let mut message = acc.finish();
            if let Some(why) = &failure {
                message.stop = Stop::Error;
                message.error = Some(why.clone());
            }
            cost += message.usage.cost;
            let usage = message.usage;
            let stop = message.stop;

            yield Event::Message(message.clone());
            yield Event::Usage {
                used: usage.context_tokens(),
                size: cfg.model.context_window,
                cost,
                cache_read: usage.cache_read,
            };

            let calls: Vec<(String, String, Value)> = message
                .tool_calls()
                .map(|(id, name, args)| (id.to_string(), name.to_string(), args.clone()))
                .collect();
            messages.push(Message::Assistant(message));

            if let Some(why) = failure {
                yield Event::Ended { reason: End::Failed(why), appended: messages[start..].to_vec() };
                return;
            }

            if calls.is_empty() {
                let reason = match stop {
                    Stop::Length => End::MaxTokens,
                    Stop::Refusal => End::Refusal,
                    Stop::Aborted => End::Cancelled,
                    _ => End::EndTurn,
                };
                yield Event::Ended { reason, appended: messages[start..].to_vec() };
                return;
            }

            // Build the batch, deciding up front which calls can run at all.
            let mut jobs = Vec::new();
            for (id, name, args) in calls {
                let tool = cfg.tools.get(&name).cloned();
                let (title, kind, refused) = match &tool {
                    None => (name.clone(), Kind::Other, Some(format!("no tool named `{name}`"))),
                    Some(t) => {
                        let refused = if spent.tool_calls >= cfg.budget.tool_calls {
                            Some("tool call budget for this session is spent".to_string())
                        } else {
                            tool::check(&t.spec().schema, &args).err()
                        };
                        (t.title(&args), t.spec().kind, refused)
                    }
                };
                if refused.is_none() {
                    spent.tool_calls += 1;
                }
                jobs.push(Job {
                    id,
                    name,
                    title,
                    kind,
                    args,
                    tool: if refused.is_none() { tool } else { None },
                    refused,
                });
            }

            for job in &jobs {
                yield Event::ToolStart {
                    id: job.id.clone(),
                    name: job.name.clone(),
                    title: job.title.clone(),
                    kind: job.kind,
                    args: job.args.clone(),
                };
            }

            let names: Vec<String> = jobs.iter().map(|j| j.name.clone()).collect();
            let one_at_a_time = cfg.tools.sequential(&names);

            let mut outcomes: HashMap<String, (bool, Output)> = HashMap::new();
            let mut queue: VecDeque<Job> = VecDeque::new();

            for job in jobs {
                match job.refused {
                    Some(why) => {
                        let output = Output::text(why);
                        yield Event::ToolEnd { id: job.id.clone(), ok: false, output: output.clone() };
                        outcomes.insert(job.id, (false, output));
                    }
                    None => queue.push_back(job),
                }
            }

            let (progress, mut reports) = mpsc::unbounded_channel::<(String, Output)>();
            let mut running = FuturesUnordered::new();
            let limit = if one_at_a_time { 1 } else { usize::MAX };

            loop {
                while running.len() < limit {
                    let Some(job) = queue.pop_front() else { break };
                    let tool = job.tool.expect("queued jobs have a tool");
                    let cx = Cx::new(job.id.clone(), cancel.child_token(), progress.clone());
                    let id = job.id;
                    let args = job.args;
                    running.push(async move { (id, tool.call(args, cx).await) });
                }
                if running.is_empty() {
                    break;
                }

                enum Step {
                    Cancelled,
                    Progress(String, Output),
                    Finished(String, Result<Output, Failed>),
                }
                let step = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => Step::Cancelled,
                    Some((id, out)) = reports.recv() => Step::Progress(id, out),
                    Some((id, result)) = running.next() => Step::Finished(id, result),
                };
                match step {
                    Step::Cancelled => {
                        yield Event::Ended { reason: End::Cancelled, appended: messages[start..].to_vec() };
                        return;
                    }
                    Step::Progress(id, output) => yield Event::ToolProgress { id, output },
                    Step::Finished(id, result) => {
                        // Drain progress before announcing the end. A tool that
                        // reports and then returns without ever awaiting sends
                        // its updates into the channel and completes within a
                        // single poll, so the finish is ready before the
                        // messages have been read -- and a client would see the
                        // result before the progress that led to it.
                        while let Ok((from, output)) = reports.try_recv() {
                            yield Event::ToolProgress { id: from, output };
                        }
                        let (ok, output) = match result {
                            Ok(output) => (true, output),
                            // A tool that fails hands the model something to
                            // read rather than ending the turn: "no such place"
                            // is a sentence it can act on.
                            Err(Failed(why)) => (false, Output::text(why)),
                        };
                        yield Event::ToolEnd { id: id.clone(), ok, output: output.clone() };
                        outcomes.insert(id, (ok, output));
                    }
                }
            }

            // Anything a tool reported just before finishing.
            while let Ok((id, output)) = reports.try_recv() {
                yield Event::ToolProgress { id, output };
            }

            // Appended in the order the model asked, not the order they
            // finished: a provider pairs results with calls by id, but a
            // history that reads out of order is a nuisance to debug.
            let mut ordered: Vec<Message> = Vec::new();
            for (id, name) in order_of(&messages) {
                if let Some((ok, output)) = outcomes.remove(&id) {
                    ordered.push(Message::ToolResult {
                        call_id: id,
                        name,
                        content: if output.content.is_empty() {
                            vec![Block::text("(no output)")]
                        } else {
                            output.content
                        },
                        error: !ok,
                    });
                }
            }
            messages.extend(ordered);
        }
    }
}

/// The tool calls of the most recent assistant message, in order.
fn order_of(messages: &[Message]) -> Vec<(String, String)> {
    for message in messages.iter().rev() {
        if let Message::Assistant(a) = message {
            return a
                .tool_calls()
                .map(|(id, name, _)| (id.to_string(), name.to_string()))
                .collect();
        }
    }
    Vec::new()
}

/// A convenience for the common shape: build a config for one model.
pub fn config(
    wire: Arc<dyn Wire>,
    model: Model,
    endpoint: Endpoint,
    tools: Arc<Set>,
) -> Config {
    Config {
        wire,
        model,
        endpoint,
        options: Options::default(),
        tools,
        budget: Budget::default(),
        system: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::{Mode, Spec};
    use futures_util::stream::{self as fstream, StreamExt};
    use parley::types::{Api, Assistant, Cost, Usage};
    use parley::EventStream;
    use serde_json::json;
    use std::sync::Mutex;

    /// A wire that answers with canned turns, one per call.
    struct Script(Mutex<VecDeque<Vec<Wired>>>);

    impl Script {
        fn of(turns: Vec<Vec<Wired>>) -> Arc<Script> {
            Arc::new(Script(Mutex::new(turns.into())))
        }
    }

    impl Wire for Script {
        fn stream(&self, _request: Request) -> EventStream {
            let events = self.0.lock().unwrap().pop_front().unwrap_or_default();
            fstream::iter(events.into_iter().map(Ok)).boxed()
        }
    }

    fn says(text: &str) -> Vec<Wired> {
        vec![
            Wired::Start { response_id: None },
            Wired::Text { index: 0, delta: text.to_string() },
            Wired::Usage(Usage { input: 10, output: 5, ..Usage::default() }),
            Wired::Done { stop: Stop::End },
        ]
    }

    fn asks(id: &str, name: &str, args: Value) -> Vec<Wired> {
        vec![
            Wired::Start { response_id: None },
            Wired::BlockStart {
                index: 0,
                kind: parley::Kind::ToolCall,
                name: Some(name.into()),
                id: Some(id.into()),
            },
            Wired::ToolArgs { index: 0, delta: args.to_string() },
            Wired::BlockEnd { index: 0 },
            Wired::Done { stop: Stop::ToolUse },
        ]
    }

    fn model() -> Model {
        Model {
            id: "gpt-oss:120b".into(),
            name: "gpt-oss".into(),
            provider: "ollama-cloud".into(),
            api: Api::OpenaiCompletions,
            context_window: 131_072,
            max_output: None,
            reasoning: false,
            cost: Cost::default(),
        }
    }

    struct Echo;
    #[async_trait::async_trait]
    impl Tool for Echo {
        fn spec(&self) -> &Spec {
            static S: std::sync::OnceLock<Spec> = std::sync::OnceLock::new();
            S.get_or_init(|| Spec {
                name: "echo".into(),
                title: "Echo".into(),
                description: "repeat something".into(),
                schema: json!({
                    "type": "object",
                    "properties": { "text": { "type": "string" } },
                    "required": ["text"],
                    "additionalProperties": false
                }),
                kind: Kind::Other,
                mode: Mode::Parallel,
            })
        }
        fn title(&self, args: &Value) -> String {
            format!("Echo {}", args["text"].as_str().unwrap_or("?"))
        }
        async fn call(&self, args: Value, _cx: Cx) -> Result<Output, Failed> {
            Ok(Output::text(args["text"].as_str().unwrap_or("").to_string()))
        }
    }

    struct Boom;
    #[async_trait::async_trait]
    impl Tool for Boom {
        fn spec(&self) -> &Spec {
            static S: std::sync::OnceLock<Spec> = std::sync::OnceLock::new();
            S.get_or_init(|| Spec {
                name: "boom".into(),
                title: "Boom".into(),
                description: "always fails".into(),
                schema: json!({"type": "object", "properties": {}, "required": []}),
                kind: Kind::Other,
                mode: Mode::Parallel,
            })
        }
        async fn call(&self, _args: Value, _cx: Cx) -> Result<Output, Failed> {
            Err(Failed::new("the well is dry"))
        }
    }

    struct Chatty;
    #[async_trait::async_trait]
    impl Tool for Chatty {
        fn spec(&self) -> &Spec {
            static S: std::sync::OnceLock<Spec> = std::sync::OnceLock::new();
            S.get_or_init(|| Spec {
                name: "chatty".into(),
                title: "Chatty".into(),
                description: "reports as it goes".into(),
                schema: json!({"type": "object", "properties": {}, "required": []}),
                kind: Kind::Fetch,
                mode: Mode::Parallel,
            })
        }
        async fn call(&self, _args: Value, cx: Cx) -> Result<Output, Failed> {
            cx.progress(Output::text("connecting"));
            cx.progress(Output::text("downloading"));
            Ok(Output::text("done"))
        }
    }

    fn tools() -> Arc<Set> {
        Arc::new(
            Set::new()
                .with(Arc::new(Echo))
                .with(Arc::new(Boom))
                .with(Arc::new(Chatty)),
        )
    }

    fn cfg(script: Arc<Script>) -> Arc<Config> {
        Arc::new(Config {
            wire: script,
            model: model(),
            endpoint: Endpoint::default(),
            options: Options::default(),
            tools: tools(),
            budget: Budget::default(),
            system: None,
        })
    }

    async fn drive(cfg: Arc<Config>, prompt: &str) -> Vec<Event> {
        run(
            cfg,
            vec![Message::user(prompt)],
            CancellationToken::new(),
        )
        .collect()
        .await
    }

    fn ended(events: &[Event]) -> (&End, &Vec<Message>) {
        match events.last().expect("a run always ends") {
            Event::Ended { reason, appended } => (reason, appended),
            other => panic!("last event was {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_turn_with_nothing_to_do_ends_after_one_call() {
        let events = drive(cfg(Script::of(vec![says("Jaipur is in Rajasthan.")])), "where?").await;
        let (reason, appended) = ended(&events);
        assert_eq!(reason, &End::EndTurn);
        assert_eq!(appended.len(), 1, "just the assistant message");
        assert!(matches!(&appended[0], Message::Assistant(a) if a.text().contains("Rajasthan")));
    }

    #[tokio::test]
    async fn a_tool_call_runs_and_its_result_is_appended() {
        let script = Script::of(vec![
            asks("c1", "echo", json!({"text": "hello"})),
            says("it said hello"),
        ]);
        let events = drive(cfg(script), "echo hello").await;
        let (reason, appended) = ended(&events);
        assert_eq!(reason, &End::EndTurn);
        // assistant(tool call), tool result, assistant(final)
        assert_eq!(appended.len(), 3, "{appended:?}");
        assert!(matches!(
            &appended[1],
            Message::ToolResult { call_id, error: false, .. } if call_id == "c1"
        ));
        let start = events.iter().find_map(|e| match e {
            Event::ToolStart { title, args, .. } => Some((title.clone(), args.clone())),
            _ => None,
        });
        // The title is built from the arguments, and rawInput is passed through
        // untouched -- both are what the client needs to show what happened.
        assert_eq!(start, Some(("Echo hello".into(), json!({"text": "hello"}))));
    }

    #[tokio::test]
    async fn talk_and_a_tool_call_in_one_message_keep_their_order() {
        let mut turn = vec![
            Wired::Start { response_id: None },
            Wired::Text { index: 0, delta: "let me check. ".into() },
            Wired::BlockStart {
                index: 1,
                kind: parley::Kind::ToolCall,
                name: Some("echo".into()),
                id: Some("c1".into()),
            },
            Wired::ToolArgs { index: 1, delta: json!({"text": "x"}).to_string() },
            Wired::BlockEnd { index: 1 },
        ];
        turn.push(Wired::Done { stop: Stop::ToolUse });
        let events = drive(cfg(Script::of(vec![turn, says("done")])), "go").await;
        // The text delta must arrive before the tool starts, or a transcript
        // built by appending shows the tool call first.
        let text_at = events.iter().position(|e| matches!(e, Event::Text { .. })).unwrap();
        let tool_at = events.iter().position(|e| matches!(e, Event::ToolStart { .. })).unwrap();
        assert!(text_at < tool_at, "{events:?}");
    }

    #[tokio::test]
    async fn a_failing_tool_is_reported_to_the_model_rather_than_ending_the_turn() {
        let script = Script::of(vec![asks("c1", "boom", json!({})), says("that did not work")]);
        let events = drive(cfg(script), "try it").await;
        let (reason, appended) = ended(&events);
        assert_eq!(reason, &End::EndTurn, "the run continued");
        assert!(matches!(
            &appended[1],
            Message::ToolResult { error: true, content, .. }
                if content[0].as_text() == Some("the well is dry")
        ));
        assert!(events.iter().any(|e| matches!(e, Event::ToolEnd { ok: false, .. })));
    }

    #[tokio::test]
    async fn a_tool_nobody_registered_is_refused_by_name() {
        let script = Script::of(vec![asks("c1", "ghost", json!({})), says("no such thing")]);
        let events = drive(cfg(script), "call ghost").await;
        let (_, appended) = ended(&events);
        assert!(matches!(
            &appended[1],
            Message::ToolResult { error: true, content, .. }
                if content[0].as_text().unwrap().contains("no tool named `ghost`")
        ));
    }

    #[tokio::test]
    async fn arguments_that_do_not_fit_the_schema_never_reach_the_tool() {
        let script = Script::of(vec![asks("c1", "echo", json!({})), says("ok")]);
        let events = drive(cfg(script), "echo nothing").await;
        let (_, appended) = ended(&events);
        assert!(matches!(
            &appended[1],
            Message::ToolResult { error: true, content, .. }
                if content[0].as_text().unwrap().contains("missing required property `text`")
        ));
    }

    #[tokio::test]
    async fn a_tool_can_report_progress_before_it_finishes() {
        let script = Script::of(vec![asks("c1", "chatty", json!({})), says("fetched")]);
        let events = drive(cfg(script), "fetch").await;
        let progress: Vec<String> = events
            .iter()
            .filter_map(|e| match e {
                Event::ToolProgress { output, .. } => {
                    Some(output.content[0].as_text().unwrap().to_string())
                }
                _ => None,
            })
            .collect();
        assert_eq!(progress, vec!["connecting", "downloading"]);
        let end_at = events.iter().position(|e| matches!(e, Event::ToolEnd { .. })).unwrap();
        let last_progress = events
            .iter()
            .rposition(|e| matches!(e, Event::ToolProgress { .. }))
            .unwrap();
        assert!(last_progress < end_at, "progress must precede the end");
    }

    #[tokio::test]
    async fn several_tool_calls_are_all_announced_before_any_result_arrives() {
        let turn = vec![
            Wired::Start { response_id: None },
            Wired::BlockStart { index: 0, kind: parley::Kind::ToolCall, name: Some("echo".into()), id: Some("a".into()) },
            Wired::ToolArgs { index: 0, delta: json!({"text": "one"}).to_string() },
            Wired::BlockStart { index: 1, kind: parley::Kind::ToolCall, name: Some("echo".into()), id: Some("b".into()) },
            Wired::ToolArgs { index: 1, delta: json!({"text": "two"}).to_string() },
            Wired::Done { stop: Stop::ToolUse },
        ];
        let events = drive(cfg(Script::of(vec![turn, says("both")])), "two things").await;
        let starts: Vec<usize> = events
            .iter()
            .enumerate()
            .filter(|(_, e)| matches!(e, Event::ToolStart { .. }))
            .map(|(i, _)| i)
            .collect();
        let first_end = events.iter().position(|e| matches!(e, Event::ToolEnd { .. })).unwrap();
        assert_eq!(starts.len(), 2);
        assert!(starts[1] < first_end, "a batch should not look sequential");

        // Results are appended in the order the model asked for them.
        let (_, appended) = ended(&events);
        let ids: Vec<&str> = appended
            .iter()
            .filter_map(|m| match m {
                Message::ToolResult { call_id, .. } => Some(call_id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(ids, vec!["a", "b"]);
    }

    #[tokio::test]
    async fn the_turn_ceiling_stops_a_model_that_will_not_stop() {
        // A script that asks for a tool every single time.
        struct Forever;
        impl Wire for Forever {
            fn stream(&self, _request: Request) -> EventStream {
                fstream::iter(asks("c", "echo", json!({"text": "again"})).into_iter().map(Ok)).boxed()
            }
        }
        let cfg = Arc::new(Config {
            wire: Arc::new(Forever),
            model: model(),
            endpoint: Endpoint::default(),
            options: Options::default(),
            tools: tools(),
            budget: Budget { turns: 3, tool_calls: 64 },
            system: None,
        });
        let events = drive(cfg, "loop forever").await;
        assert_eq!(ended(&events).0, &End::MaxTurns);
        let turns = events.iter().filter(|e| matches!(e, Event::TurnStart { .. })).count();
        assert_eq!(turns, 3);
    }

    #[tokio::test]
    async fn the_tool_ceiling_refuses_calls_without_ending_the_run() {
        let script = Script::of(vec![
            asks("c1", "echo", json!({"text": "one"})),
            asks("c2", "echo", json!({"text": "two"})),
            says("stopped"),
        ]);
        let cfg = Arc::new(Config {
            wire: script,
            model: model(),
            endpoint: Endpoint::default(),
            options: Options::default(),
            tools: tools(),
            budget: Budget { turns: 8, tool_calls: 1 },
            system: None,
        });
        let events = drive(cfg, "two calls").await;
        let refusals: Vec<&Event> = events
            .iter()
            .filter(|e| matches!(e, Event::ToolEnd { ok: false, .. }))
            .collect();
        assert_eq!(refusals.len(), 1);
        assert_eq!(ended(&events).0, &End::EndTurn);
    }

    #[tokio::test]
    async fn a_cancelled_run_says_so_and_appends_nothing() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let events: Vec<Event> = run(
            cfg(Script::of(vec![says("never seen")])),
            vec![Message::user("stop")],
            cancel,
        )
        .collect()
        .await;
        let (reason, appended) = ended(&events);
        assert_eq!(reason, &End::Cancelled);
        assert!(appended.is_empty(), "{appended:?}");
    }

    #[tokio::test]
    async fn a_provider_failure_lands_in_the_history_as_a_failed_turn() {
        struct Broken;
        impl Wire for Broken {
            fn stream(&self, _request: Request) -> EventStream {
                fstream::iter(vec![Err(parley::Error::Auth("no credential".into()))]).boxed()
            }
        }
        let cfg = Arc::new(Config {
            wire: Arc::new(Broken),
            model: model(),
            endpoint: Endpoint::default(),
            options: Options::default(),
            tools: tools(),
            budget: Budget::default(),
            system: None,
        });
        let events = drive(cfg, "hello").await;
        let (reason, appended) = ended(&events);
        assert!(matches!(reason, End::Failed(why) if why.contains("no credential")));
        // The failed turn is still a turn: it stays in the history so the
        // record matches what the provider was actually sent.
        assert!(matches!(
            &appended[0],
            Message::Assistant(Assistant { stop: Stop::Error, error: Some(_), .. })
        ));
    }

    #[tokio::test]
    async fn usage_is_reported_against_the_model_context_window() {
        let events = drive(cfg(Script::of(vec![says("hi")])), "hi").await;
        let usage = events.iter().find_map(|e| match e {
            Event::Usage { used, size, .. } => Some((*used, *size)),
            _ => None,
        });
        assert_eq!(usage, Some((15, 131_072)));
    }
}
