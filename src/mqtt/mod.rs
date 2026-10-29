//! A general-purpose MQTT client on top of ESP-IDF's `esp-mqtt`.
//!
//! ```ignore
//! let settings = mqtt::Settings::load_or_provision(&secrets)?.expect("MQTT is configured");
//! let options = mqtt::Options {
//!     client_id: Some("pulsar-a1b2c3".into()),
//!     will: Some(mqtt::Will::retained("pulsar/a1b2c3/availability", "offline")),
//!     ..mqtt::Options::default()
//! };
//! let client = mqtt::Client::connect(&settings, &options)?;
//!
//! client.subscribe("pulsar/a1b2c3/cmd/+", mqtt::QoS::AtLeastOnce, |client, message| {
//!     if let Some(text) = message.text() {
//!         log::info!("command on {}: {} bytes", message.topic, text.len());
//!         let _ = client.publish("pulsar/a1b2c3/pong", text, mqtt::QoS::AtMostOnce, false);
//!     }
//! })?;
//! client.on_connect(|client| {
//!     let _ = client.publish("pulsar/a1b2c3/availability", "online", mqtt::QoS::AtLeastOnce, true);
//! });
//! ```
//!
//! # What is supported
//!
//! - **MQTT 5.0 and 3.1.1.** `config::MQTT_VERSION` picks one. With 5.0 the client asks the
//!   broker to keep the session for a while, to hold back the Last Will so that a quick reconnect
//!   cancels it, and to send nothing longer than the incoming limit. Shared subscriptions
//!   (`$share/<group>/<filter>`) are accepted by the filter check.
//! - **Four transports,** chosen by the URL scheme: `mqtt://` (TCP), `mqtts://` (TLS),
//!   `ws://` and `wss://` (WebSocket, plain or over TLS). A WebSocket URL needs the path the
//!   broker listens on, for example `wss://broker.example/mqtt`. TLS 1.3 (1.2 as a fallback) checks
//!   the server against the Mozilla root certificates built into the firmware, including the host
//!   name and the dates, so TLS waits for the SNTP clock like HTTPS does.
//! - **User name and password** (never inside the URL), kept in the secret store, see [`Settings`].
//! - **`QoS` 0, 1 and 2,** retained messages and a **Last Will** ([`Will`]).
//! - **Wildcard subscriptions with a handler each,** matched by the rules of the specification
//!   (`+`, `#`, `$SYS` topics). Subscriptions are sent again after every reconnect that the broker
//!   does not remember.
//! - **Periodic tasks** ([`Client::every`]) run on the client's own thread, so a program that only
//!   sends readings from time to time needs no thread of its own.
//! - **Reconnecting** is done by the MQTT task. Messages queued while offline (the outbox) are sent
//!   when the connection is back.
//! - **Long messages** arrive in pieces and are put back together, up to
//!   `config::MQTT_MAX_INCOMING_BYTES` (6 KiB). Longer ones are dropped with a warning; with
//!   MQTT 5 the broker is told the limit, so it does not send them. Over TLS the limit also keeps
//!   a message inside one TLS record (8 KiB, see `sdkconfig.defaults`).
//!
//! # Things to know
//!
//! - **Never wait inside the event callback.** The MQTT task holds the client's lock while it
//!   calls back, so a callback that waits for a thread that then publishes deadlocks. That is why
//!   events go through a channel ([`session`]) to the `mqtt` thread, and handlers and connect hooks
//!   are free to call the [`Client`].
//! - **Handlers run one at a time** on the `mqtt` thread. A slow handler delays the others, and
//!   once `config::MQTT_QUEUE_BYTES` of messages are waiting, new ones are dropped.
//! - **What `esp-idf-svc` does not expose, this module cannot offer:** MQTT 5 properties of single
//!   messages (user properties, response topic, expiry), the reason codes of the broker's answers
//!   (a refused subscription or a refused login is only visible in the log), and the retain flag
//!   and `QoS` of an incoming message.
//! - **No client certificates** (mutual TLS) and no custom CA yet.
//! - The password is kept by the ESP-IDF client in RAM while it runs, and a build made with the
//!   environment variables contains it in the firmware image, like the Wi-Fi password.
//! - Payloads and credentials are never logged; topics only in warnings.

mod budget;
mod client;
mod reassemble;
mod session;
mod settings;
mod topic;

use std::io;

use esp_idf_svc::mqtt::client::QoS as EspQoS;
use esp_idf_svc::sys::EspError;

pub use client::Client;
pub use settings::Settings;

use crate::secrets;

/// The code `esp_mqtt_client_enqueue` returns when the message does not fit in the outbox.
const OUTBOX_FULL_CODE: i32 = -2;

/// What can go wrong with MQTT.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A topic or filter is empty, too long, or has a wildcard where it is not allowed (or a
    /// name with one).
    #[error("invalid MQTT topic or filter")]
    InvalidTopic,
    /// The broker URL is not `mqtt://`, `mqtts://`, `ws://` or `wss://` with a host, has
    /// whitespace or a user name inside, or is too long (or the stored record is damaged).
    #[error(
        "invalid MQTT broker URL (mqtt://, mqtts://, ws:// or wss:// and a host, no user name)"
    )]
    InvalidUrl,
    /// A user name or password is too long or has a NUL byte, or there is a password without a
    /// user name.
    #[error("invalid MQTT user name or password")]
    InvalidCredentials,
    /// A user name or password was set at build time without `PULSAR_MQTT_URL`.
    #[error("set PULSAR_MQTT_URL at build time together with the user name and password")]
    IncompleteBuildEnv,
    /// The clock was not set in time, so the certificate cannot be checked.
    #[error("the clock is not set, cannot check the certificate (was clock::start called?)")]
    ClockNotSynced,
    /// The message does not fit in the outbox, which is full of messages waiting to be sent.
    #[error("the MQTT outbox is full")]
    OutboxFull,
    /// The secret store failed.
    #[error("secret store: {0}")]
    Secrets(#[from] secrets::Error),
    /// The MQTT client failed.
    #[error("MQTT client: {0}")]
    Driver(EspError),
    /// The thread could not be created.
    #[error("cannot start the MQTT thread: {0}")]
    Thread(#[from] io::Error),
}

impl From<EspError> for Error {
    fn from(error: EspError) -> Self {
        if error.code() == OUTBOX_FULL_CODE {
            Self::OutboxFull
        } else {
            Self::Driver(error)
        }
    }
}

/// How hard MQTT tries to deliver a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QoS {
    /// At most once: sent once, no acknowledgement. It can be lost.
    AtMostOnce,
    /// At least once: repeated until acknowledged. It can arrive twice.
    AtLeastOnce,
    /// Exactly once: a four-step exchange; the slowest.
    ExactlyOnce,
}

impl From<QoS> for EspQoS {
    fn from(qos: QoS) -> Self {
        match qos {
            QoS::AtMostOnce => Self::AtMostOnce,
            QoS::AtLeastOnce => Self::AtLeastOnce,
            QoS::ExactlyOnce => Self::ExactlyOnce,
        }
    }
}

/// A message that arrived, as handlers see it.
#[derive(Clone, Copy, Debug)]
pub struct Message<'a> {
    /// The topic it was published to.
    pub topic: &'a str,
    /// The payload, which may be empty.
    pub payload: &'a [u8],
}

impl<'a> Message<'a> {
    /// The payload as text, if it is valid UTF-8.
    #[must_use]
    pub fn text(&self) -> Option<&'a str> {
        core::str::from_utf8(self.payload).ok()
    }
}

/// The message the broker publishes for the client when the connection ends without a goodbye.
#[derive(Clone, Debug)]
pub struct Will {
    /// Where it is published.
    pub topic: String,
    /// What is published.
    pub payload: Vec<u8>,
    /// How hard the broker tries to deliver it.
    pub qos: QoS,
    /// Whether the broker keeps it for subscribers that come later.
    pub retain: bool,
}

impl Will {
    /// A will with `QoS` 1 that the broker keeps: what an availability topic wants.
    #[must_use]
    pub fn retained(topic: impl Into<String>, payload: impl Into<Vec<u8>>) -> Self {
        Self {
            topic: topic.into(),
            payload: payload.into(),
            qos: QoS::AtLeastOnce,
            retain: true,
        }
    }
}

/// What a connection needs besides the broker address.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// The name the broker knows this client by; it must be unique among the clients of the broker
    /// (a second client with the same name throws the first one out). `None` lets the broker pick.
    pub client_id: Option<String>,
    /// The Last Will.
    pub will: Option<Will>,
    /// Ask the broker to keep the session (subscriptions, and messages with `QoS` 1 and 2 that
    /// arrived meanwhile) while the client is offline. Off by default: after a reboot an old
    /// command, such as "restart", would run again.
    pub persistent_session: bool,
}
