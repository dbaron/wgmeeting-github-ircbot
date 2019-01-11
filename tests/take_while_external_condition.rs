// A modified form of TakeWhileExternalCondition in the futures library, which
// is: Copyright (c) 2016 Alex Crichton
// (forked from futures 0.1.18)

extern crate futures;

use futures::stream::Stream;
use futures::{Async, Poll};

/// A stream combinator which takes elements from a stream while a predicate
/// holds.
#[derive(Debug)]
#[must_use = "streams do nothing unless polled"]
pub struct TakeWhileExternalCondition<S, P>
where
    S: Stream,
{
    stream: S,
    pred: P,
    done_taking: bool,
}

pub fn new<S, P>(s: S, p: P) -> TakeWhileExternalCondition<S, P>
where
    S: Stream,
    P: FnMut() -> bool,
{
    TakeWhileExternalCondition {
        stream: s,
        pred: p,
        done_taking: false,
    }
}

#[allow(dead_code)]
impl<S, P> TakeWhileExternalCondition<S, P>
where
    S: Stream,
{
    /// Acquires a reference to the underlying stream that this combinator is
    /// pulling from.
    pub fn get_ref(&self) -> &S {
        &self.stream
    }

    /// Acquires a mutable reference to the underlying stream that this
    /// combinator is pulling from.
    ///
    /// Note that care must be taken to avoid tampering with the state of the
    /// stream which may otherwise confuse this combinator.
    pub fn get_mut(&mut self) -> &mut S {
        &mut self.stream
    }

    /// Consumes this combinator, returning the underlying stream.
    ///
    /// Note that this may discard intermediate state of this combinator, so
    /// care should be taken to avoid losing resources when this is called.
    pub fn into_inner(self) -> S {
        self.stream
    }
}

// Forwarding impl of Sink from the underlying stream
impl<S, P> futures::sink::Sink for TakeWhileExternalCondition<S, P>
where
    S: futures::sink::Sink + Stream,
{
    type SinkItem = S::SinkItem;
    type SinkError = S::SinkError;

    fn start_send(&mut self, item: S::SinkItem) -> futures::StartSend<S::SinkItem, S::SinkError> {
        self.stream.start_send(item)
    }

    fn poll_complete(&mut self) -> Poll<(), S::SinkError> {
        self.stream.poll_complete()
    }

    fn close(&mut self) -> Poll<(), S::SinkError> {
        self.stream.close()
    }
}

impl<S, P> Stream for TakeWhileExternalCondition<S, P>
where
    S: Stream,
    P: FnMut() -> bool,
{
    type Item = S::Item;
    type Error = S::Error;

    fn poll(&mut self) -> Poll<Option<S::Item>, S::Error> {
        if self.done_taking {
            return Ok(Async::Ready(None));
        }

        if !(self.pred)() {
            self.done_taking = true;
            return Ok(Async::Ready(None));
        }

        self.stream.poll()
    }
}
