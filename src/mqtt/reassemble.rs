//! Puts the pieces of long incoming messages back together.
//!
//! A message longer than the receive buffer arrives in several pieces, and only the first one has
//! the topic. [`Reassembler`] is a small state machine: it has no hardware, no logging and no crate
//! types, so it can be tested on a computer (`rustc --test src/mqtt/reassemble.rs`).

use std::mem;

/// Which part of a message a piece is.
#[derive(Debug)]
pub enum Chunk {
    /// The whole message.
    Whole,
    /// The start of a message of `total` bytes.
    First { total: usize },
    /// A later piece, which starts `offset` bytes into a message of `total` bytes.
    Next { offset: usize, total: usize },
}

/// A whole incoming message.
#[derive(Debug, PartialEq, Eq)]
pub struct Incoming {
    pub topic: String,
    pub payload: Vec<u8>,
}

/// Why a message was thrown away.
#[derive(Debug, PartialEq, Eq)]
pub enum Dropped {
    /// The message is longer than the limit.
    TooLong { total: usize },
    /// A piece is missing or arrived out of order.
    MissingPiece,
    /// The message has no topic.
    NoTopic,
}

/// What a piece led to.
#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    /// More pieces are needed, or the rest of a dropped message is being skipped.
    Pending,
    /// The message is complete.
    Complete(Incoming),
    /// The message is lost. This is reported once per message: its remaining pieces give
    /// [`Step::Pending`].
    Dropped(Dropped),
}

/// Where the reassembly of the current message stands.
#[derive(Default)]
enum Assembly {
    /// No message is half-way.
    #[default]
    Idle,
    /// The pieces so far of a message of `total` bytes.
    Collecting {
        topic: String,
        payload: Vec<u8>,
        total: usize,
    },
    /// The rest of the current message is thrown away.
    Skipping,
}

/// Puts the pieces of long messages back together.
pub struct Reassembler {
    limit: usize,
    state: Assembly,
}

impl Reassembler {
    /// A reassembler that drops messages longer than `limit` bytes.
    pub const fn new(limit: usize) -> Self {
        Self {
            limit,
            state: Assembly::Idle,
        }
    }

    /// Forgets a half-finished message, for example after a disconnect.
    pub fn reset(&mut self) {
        self.state = Assembly::Idle;
    }

    /// Adds a piece.
    ///
    /// What comes out is always a whole message: one that is too long, or has a missing piece, is
    /// dropped (once), and so is the rest of it.
    pub fn push(&mut self, topic: Option<String>, payload: Vec<u8>, chunk: Chunk) -> Step {
        match (chunk, mem::take(&mut self.state)) {
            (Chunk::Whole, _) => match topic {
                Some(topic) => Step::Complete(Incoming { topic, payload }),
                None => Step::Dropped(Dropped::NoTopic),
            },

            (Chunk::First { total }, _) if total > self.limit => {
                self.state = Assembly::Skipping;
                Step::Dropped(Dropped::TooLong { total })
            }
            (Chunk::First { total }, _) => {
                let Some(topic) = topic else {
                    self.state = Assembly::Skipping;
                    return Step::Dropped(Dropped::NoTopic);
                };
                let mut collected = Vec::with_capacity(total);
                collected.extend_from_slice(&payload);
                self.state = Assembly::Collecting {
                    topic,
                    payload: collected,
                    total,
                };
                Step::Pending
            }

            (
                Chunk::Next { offset, total },
                Assembly::Collecting {
                    topic,
                    payload: mut collected,
                    total: expected,
                },
            ) if offset == collected.len() && total == expected => {
                collected.extend_from_slice(&payload);
                if collected.len() >= total {
                    return Step::Complete(Incoming {
                        topic,
                        payload: collected,
                    });
                }
                self.state = Assembly::Collecting {
                    topic,
                    payload: collected,
                    total,
                };
                Step::Pending
            }

            // The rest of a message that is being skipped, or a piece that does not follow on
            // from the last one: throw the message away, and the pieces still to come with it.
            (Chunk::Next { offset, total }, state) => {
                let report = !matches!(state, Assembly::Skipping);
                if offset + payload.len() < total {
                    self.state = Assembly::Skipping;
                }
                if report {
                    Step::Dropped(Dropped::MissingPiece)
                } else {
                    Step::Pending
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMIT: usize = 100;

    fn topic() -> Option<String> {
        Some("a/b".to_owned())
    }

    fn message(topic: &str, payload: &[u8]) -> Step {
        Step::Complete(Incoming {
            topic: topic.to_owned(),
            payload: payload.to_vec(),
        })
    }

    #[test]
    fn whole_messages_pass_through() {
        let mut r = Reassembler::new(LIMIT);
        assert_eq!(
            r.push(topic(), b"hello".to_vec(), Chunk::Whole),
            message("a/b", b"hello")
        );
        assert_eq!(
            r.push(topic(), Vec::new(), Chunk::Whole),
            message("a/b", b"")
        );
        assert_eq!(
            r.push(None, b"x".to_vec(), Chunk::Whole),
            Step::Dropped(Dropped::NoTopic)
        );
    }

    #[test]
    fn pieces_are_joined() {
        let mut r = Reassembler::new(LIMIT);
        assert_eq!(
            r.push(topic(), b"abc".to_vec(), Chunk::First { total: 8 }),
            Step::Pending
        );
        assert_eq!(
            r.push(
                None,
                b"def".to_vec(),
                Chunk::Next {
                    offset: 3,
                    total: 8
                }
            ),
            Step::Pending
        );
        assert_eq!(
            r.push(
                None,
                b"gh".to_vec(),
                Chunk::Next {
                    offset: 6,
                    total: 8
                }
            ),
            message("a/b", b"abcdefgh")
        );
        // Ready for the next one.
        assert_eq!(
            r.push(topic(), b"z".to_vec(), Chunk::Whole),
            message("a/b", b"z")
        );
    }

    #[test]
    fn too_long_messages_are_dropped_once() {
        let mut r = Reassembler::new(LIMIT);
        assert_eq!(
            r.push(topic(), vec![0; 40], Chunk::First { total: 150 }),
            Step::Dropped(Dropped::TooLong { total: 150 })
        );
        assert_eq!(
            r.push(
                None,
                vec![0; 40],
                Chunk::Next {
                    offset: 40,
                    total: 150
                }
            ),
            Step::Pending
        );
        assert_eq!(
            r.push(
                None,
                vec![0; 40],
                Chunk::Next {
                    offset: 80,
                    total: 150
                }
            ),
            Step::Pending
        );
        // The last piece ends the skipping.
        assert_eq!(
            r.push(
                None,
                vec![0; 30],
                Chunk::Next {
                    offset: 120,
                    total: 150
                }
            ),
            Step::Pending
        );
        assert_eq!(
            r.push(topic(), b"ok".to_vec(), Chunk::Whole),
            message("a/b", b"ok")
        );
    }

    #[test]
    fn a_message_exactly_at_the_limit_is_kept() {
        let mut r = Reassembler::new(LIMIT);
        assert_eq!(
            r.push(topic(), vec![1; 60], Chunk::First { total: LIMIT }),
            Step::Pending
        );
        assert_eq!(
            r.push(
                None,
                vec![2; 40],
                Chunk::Next {
                    offset: 60,
                    total: LIMIT
                }
            ),
            message("a/b", &[vec![1; 60], vec![2; 40]].concat())
        );
    }

    #[test]
    fn a_missing_piece_drops_the_message_once() {
        let mut r = Reassembler::new(LIMIT);
        assert_eq!(
            r.push(topic(), b"abc".to_vec(), Chunk::First { total: 9 }),
            Step::Pending
        );
        // The piece at offset 3 never arrived.
        assert_eq!(
            r.push(
                None,
                b"ghi".to_vec(),
                Chunk::Next {
                    offset: 6,
                    total: 9
                }
            ),
            Step::Dropped(Dropped::MissingPiece)
        );
        assert_eq!(
            r.push(topic(), b"ok".to_vec(), Chunk::Whole),
            message("a/b", b"ok")
        );
    }

    #[test]
    fn a_lost_first_piece_drops_the_rest() {
        let mut r = Reassembler::new(LIMIT);
        assert_eq!(
            r.push(
                None,
                b"def".to_vec(),
                Chunk::Next {
                    offset: 3,
                    total: 9
                }
            ),
            Step::Dropped(Dropped::MissingPiece)
        );
        assert_eq!(
            r.push(
                None,
                b"ghi".to_vec(),
                Chunk::Next {
                    offset: 6,
                    total: 9
                }
            ),
            Step::Pending
        );
        assert_eq!(
            r.push(topic(), b"ok".to_vec(), Chunk::Whole),
            message("a/b", b"ok")
        );
    }

    #[test]
    fn a_new_message_replaces_a_half_finished_one() {
        let mut r = Reassembler::new(LIMIT);
        assert_eq!(
            r.push(topic(), b"abc".to_vec(), Chunk::First { total: 9 }),
            Step::Pending
        );
        assert_eq!(
            r.push(topic(), b"new".to_vec(), Chunk::Whole),
            message("a/b", b"new")
        );
    }

    #[test]
    fn reset_forgets_the_partial_message() {
        let mut r = Reassembler::new(LIMIT);
        assert_eq!(
            r.push(topic(), b"abc".to_vec(), Chunk::First { total: 6 }),
            Step::Pending
        );
        r.reset();
        assert_eq!(
            r.push(
                None,
                b"def".to_vec(),
                Chunk::Next {
                    offset: 3,
                    total: 6
                }
            ),
            Step::Dropped(Dropped::MissingPiece)
        );
    }

    #[test]
    fn a_first_piece_without_topic_is_dropped() {
        let mut r = Reassembler::new(LIMIT);
        assert_eq!(
            r.push(None, b"abc".to_vec(), Chunk::First { total: 6 }),
            Step::Dropped(Dropped::NoTopic)
        );
        assert_eq!(
            r.push(
                None,
                b"def".to_vec(),
                Chunk::Next {
                    offset: 3,
                    total: 6
                }
            ),
            Step::Pending
        );
    }
}
