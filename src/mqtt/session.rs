//! The hand-over from ESP-IDF's MQTT task to the thread that runs the handlers.
//!
//! The MQTT task calls [`Outbound::forward`] for every event, and that call must never wait: while
//! the task is inside its callback it holds the client's internal lock, so a callback that waited
//! for a thread which in turn publishes or subscribes would deadlock. So [`Outbound`] only copies
//! the event into an owned [`Event`] and puts it on a channel. [`Inbound`] is the other end.
//!
//! Connection events are tiny and rare, and are never dropped. Incoming messages are: only
//! [`MQTT_QUEUE_BYTES`] of them may wait (the payload, the topic and the bookkeeping of each, see
//! [`Event::cost`]), so a slow handler cannot use up the memory.
//!
//! A message longer than the receive buffer arrives in several pieces; they are put back together
//! by [`reassemble`](super::reassemble).

use std::mem::size_of;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

use esp_idf_svc::mqtt::client::{Details, EspMqttEvent, EventPayload};

use super::budget::Budget;
use super::reassemble::Chunk;
use crate::config::MQTT_QUEUE_BYTES;

/// What the MQTT task reports, copied out of its buffers.
#[derive(Debug)]
pub enum Event {
    /// The broker accepted the connection. `session_present` is true if it kept our session
    /// (and with it the subscriptions).
    Connected { session_present: bool },
    /// The connection was lost. The MQTT task reconnects by itself.
    Disconnected,
    /// The transport or the protocol failed. A disconnect follows.
    Failed,
    /// A piece of an incoming message. Only the first piece has a topic.
    Data {
        topic: Option<String>,
        payload: Vec<u8>,
        chunk: Chunk,
    },
    /// The schedule of the periodic tasks changed: the dispatcher has to work out its next wake-up
    /// again. See [`Waker`].
    Reschedule,
}

/// What an allocation costs on top of its contents (the allocator's header and rounding), for the
/// two allocations of a message: the topic and the payload.
const ALLOCATION_OVERHEAD_BYTES: usize = 32;

impl Event {
    /// What a piece of a message holds in memory while it waits: the event itself (and its place
    /// on the channel), the topic and the payload. This is what the queue budget counts, so that
    /// messages with a long topic and no payload cannot fill the heap either.
    fn cost(topic_len: usize, payload_len: usize) -> usize {
        size_of::<Self>() + ALLOCATION_OVERHEAD_BYTES + topic_len + payload_len
    }
}

/// A new channel: the end for the MQTT task and the end for the dispatcher thread.
pub fn channel() -> (Outbound, Inbound) {
    let (events, receiver) = mpsc::channel();
    let queued = Arc::new(Budget::new(MQTT_QUEUE_BYTES));
    (
        Outbound {
            events,
            queued: Arc::clone(&queued),
        },
        Inbound {
            events: receiver,
            queued,
        },
    )
}

/// The end that lives in the MQTT task's callback.
pub struct Outbound {
    events: Sender<Event>,
    /// What the message pieces on the channel hold.
    queued: Arc<Budget>,
}

/// Wakes the dispatcher thread from anywhere.
#[derive(Clone)]
pub struct Waker(Sender<Event>);

impl Waker {
    /// Makes the dispatcher look at its schedule again. Without this it could sleep through a task
    /// that was added while it waited with no deadline.
    pub fn reschedule(&self) {
        // The receiver is gone only while the client is being dropped.
        let _ = self.0.send(Event::Reschedule);
    }
}

impl Outbound {
    /// A handle to wake the dispatcher with.
    pub fn waker(&self) -> Waker {
        Waker(self.events.clone())
    }

    /// Copies `event` onto the channel without waiting. Events that are of no use are skipped.
    pub fn forward(&self, event: &EspMqttEvent<'_>) {
        let event = match event.payload() {
            EventPayload::Connected(session_present) => Event::Connected { session_present },
            EventPayload::Disconnected => Event::Disconnected,
            EventPayload::Error(_) => Event::Failed,
            EventPayload::Received {
                topic,
                data,
                details,
                ..
            } => {
                let cost = Event::cost(topic.map_or(0, str::len), data.len());
                if !self.queued.try_charge(cost) {
                    log::warn!("MQTT: handlers are too slow, dropped a message piece");
                    return;
                }
                Event::Data {
                    topic: topic.map(str::to_owned),
                    payload: data.to_vec(),
                    chunk: match details {
                        Details::Complete => Chunk::Whole,
                        Details::InitialChunk(first) => Chunk::First {
                            total: first.total_data_size,
                        },
                        Details::SubsequentChunk(next) => Chunk::Next {
                            offset: next.current_data_offset,
                            total: next.total_data_size,
                        },
                    },
                }
            }
            // Acknowledgements and the like: nothing waits for them.
            _ => return,
        };
        // The receiver is gone only while the client is being dropped.
        let _ = self.events.send(event);
    }
}

/// The end that the dispatcher thread reads.
pub struct Inbound {
    events: Receiver<Event>,
    queued: Arc<Budget>,
}

/// What the dispatcher thread woke up for.
pub enum Wake {
    /// The MQTT task reported something.
    Event(Event),
    /// The wait ran out with nothing to report.
    Idle,
    /// The client is gone.
    Closed,
}

impl Inbound {
    /// Waits for the next event, for at most `timeout` (for ever if `None`).
    pub fn recv(&self, timeout: Option<Duration>) -> Wake {
        let received = match timeout {
            Some(timeout) => self.events.recv_timeout(timeout),
            None => self
                .events
                .recv()
                .map_err(|_| RecvTimeoutError::Disconnected),
        };
        match received {
            Ok(event) => {
                if let Event::Data { topic, payload, .. } = &event {
                    self.queued.release(Event::cost(
                        topic.as_ref().map_or(0, String::len),
                        payload.len(),
                    ));
                }
                Wake::Event(event)
            }
            Err(RecvTimeoutError::Timeout) => Wake::Idle,
            Err(RecvTimeoutError::Disconnected) => Wake::Closed,
        }
    }
}
