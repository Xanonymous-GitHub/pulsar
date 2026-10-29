//! The hand-over from ESP-IDF's MQTT task to the thread that runs the handlers.
//!
//! The MQTT task calls [`Outbound::forward`] for every event, and that call must never wait: while
//! the task is inside its callback it holds the client's internal lock, so a callback that waited
//! for a thread which in turn publishes or subscribes would deadlock. So [`Outbound`] only copies
//! the event into an owned [`Event`] and puts it on a channel. [`Inbound`] is the other end.
//!
//! Connection events are tiny and rare, and are never dropped. Incoming messages are: only
//! [`MQTT_QUEUE_BYTES`] of them may wait, so a slow handler cannot use up the memory.
//!
//! A message longer than the receive buffer arrives in several pieces; they are put back together
//! by [`reassemble`](super::reassemble).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

use esp_idf_svc::mqtt::client::{Details, EspMqttEvent, EventPayload};

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
}

/// A new channel: the end for the MQTT task and the end for the dispatcher thread.
pub fn channel() -> (Outbound, Inbound) {
    let (events, receiver) = mpsc::channel();
    let queued = Arc::new(AtomicUsize::new(0));
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
    /// Bytes of message pieces on the channel.
    queued: Arc<AtomicUsize>,
}

impl Outbound {
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
                let bytes = data.len();
                if self.queued.fetch_add(bytes, Ordering::Relaxed) + bytes > MQTT_QUEUE_BYTES {
                    self.queued.fetch_sub(bytes, Ordering::Relaxed);
                    log::warn!(
                        "MQTT: handlers are too slow, dropped a message piece of {bytes} bytes"
                    );
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
    queued: Arc<AtomicUsize>,
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
                if let Event::Data { payload, .. } = &event {
                    self.queued.fetch_sub(payload.len(), Ordering::Relaxed);
                }
                Wake::Event(event)
            }
            Err(RecvTimeoutError::Timeout) => Wake::Idle,
            Err(RecvTimeoutError::Disconnected) => Wake::Closed,
        }
    }
}
