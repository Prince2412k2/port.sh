//! The one thing a provider has to be able to do.
//!
//! A wire takes an owned request and hands back a stream of events. That is the
//! entire contract, and it is deliberately small enough that a recorded
//! conversation, a fake, and a real provider are indistinguishable to whatever
//! is above -- which is what makes an agent loop testable without a network.

use futures_util::stream::BoxStream;

use crate::error::Result;
use crate::stream::Event;
use crate::types::Request;

/// Events, or the error that ended them.
pub type EventStream = BoxStream<'static, Result<Event>>;

pub trait Wire: Send + Sync {
    fn stream(&self, request: Request) -> EventStream;
}

/// Turns SSE frames into events.
///
/// Split out from the streaming so it can be tested against frames typed by
/// hand or captured from a real provider, with no client and no socket.
pub trait Frames: Send + 'static {
    fn frame(&mut self, frame: &crate::sse::Frame) -> Vec<Result<Event>>;
    /// End of stream. A parser that has been holding something back -- an
    /// unfinished tool call, a usage total -- emits it here.
    fn finish(&mut self) -> Vec<Result<Event>> {
        Vec::new()
    }
}
