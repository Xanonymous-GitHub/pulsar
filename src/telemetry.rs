//! What the board does over MQTT: it says whether it is online, reports its state, and takes a few
//! commands. It also announces itself to [Home Assistant](crate::home_assistant).
//!
//! ```ignore
//! if let Some(settings) = mqtt::Settings::load_or_provision(&secrets)? {
//!     telemetry::spawn(settings, status.clone(), message.clone())?;
//! }
//! ```
//!
//! The thread waits until Wi-Fi has an address, starts the [`mqtt::Client`], registers the
//! handlers and ends: it costs no stack afterwards. The state goes out every [`TELEMETRY_PERIOD`]
//! from the client's own thread. `<id>` below is the last three
//! bytes of the board's MAC address in hex, for example `a1b2c3`, so many boards can share a
//! broker. The client id is `pulsar-<id>`.
//!
//! | Topic | Direction | Content |
//! |---|---|---|
//! | `pulsar/<id>/availability` | board → broker | `online` (retained) once connected; `offline` (retained) is the Last Will, and is sent on purpose before a restart |
//! | `pulsar/<id>/state` | board → broker | retained JSON, see [`Report`], on connect and every minute |
//! | `pulsar/<id>/cmd/ping` | broker → board | any text; the board answers on `pulsar/<id>/pong` with the same text |
//! | `pulsar/<id>/cmd/notice` | broker → board | text to show on the screen for [`NOTICE_DURATION`] (ASCII, at most [`NOTICE_MAX_LEN`] characters) |
//! | `pulsar/<id>/cmd/restart` | broker → board | any message restarts the board, except in the first [`RESTART_GRACE`] after the connection came up |
//! | `homeassistant/device/pulsar-<id>/config` | board → broker | retained discovery message |
//!
//! # Things to know
//!
//! - **Anyone who can publish to the command topics controls the board.** The broker must have
//!   access rules (and TLS, so the login is not readable on the way); commands have no password of
//!   their own.
//! - **The session is clean on purpose.** With a kept session the broker would queue commands for
//!   an offline board and run them at the next boot, so an old `restart` would restart it again.
//! - **A retained `restart` cannot make the board restart for ever.** The broker replays a
//!   retained message after every subscribe, so a `restart` is ignored in the first
//!   [`RESTART_GRACE`] after the connection comes up (and the board tries to clear it on the
//!   broker, which an access rule may forbid). A real command inside that window has to be sent
//!   again. Empty messages are never commands.
//! - **The screen text is shared with Bluetooth:** a notice also shows up in the `weather`
//!   characteristic while it lasts.

use std::io;
use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::config::{
    HOSTNAME, MQTT_TOPIC_PREFIX, NOTICE_DURATION, NOTICE_MAX_LEN, RESTART_DELAY, RESTART_GRACE,
    TELEMETRY_PERIOD, TELEMETRY_RETRY, TELEMETRY_RETRY_MAX, TELEMETRY_STACK_BYTES,
};
use crate::home_assistant::{self, Device};
use crate::message::SharedMessage;
use crate::mqtt::{Client, Message, Options, QoS, Settings, Will};
use crate::platform;
use crate::status::{Band, SharedStatus};

/// The availability payloads Home Assistant expects by default.
const ONLINE: &str = "online";
const OFFLINE: &str = "offline";

/// The client, kept for as long as the program runs: the connection ends with the last handle.
static CLIENT: OnceLock<Client> = OnceLock::new();

/// How often the thread looks whether Wi-Fi has an address yet.
const NETWORK_POLL: Duration = Duration::from_secs(1);

/// Every topic of this board.
struct Topics {
    availability: String,
    state: String,
    pong: String,
    /// The filter for all commands.
    commands: String,
    /// What comes before the command name in a command topic.
    command_prefix: String,
    restart: String,
    notice: String,
}

impl Topics {
    fn new(id: &str) -> Self {
        let base = format!("{MQTT_TOPIC_PREFIX}/{id}");
        Self {
            availability: format!("{base}/availability"),
            state: format!("{base}/state"),
            pong: format!("{base}/pong"),
            commands: format!("{base}/cmd/+"),
            command_prefix: format!("{base}/cmd/"),
            restart: format!("{base}/cmd/restart"),
            notice: format!("{base}/cmd/notice"),
        }
    }
}

/// The state message. The field names are used by the Home Assistant sensors.
#[derive(Serialize)]
struct Report<'a> {
    /// The Wi-Fi network, empty when unknown.
    ssid: &'a str,
    /// Signal strength in dBm.
    rssi: Option<i32>,
    /// `2.4 GHz` or `5 GHz`.
    band: Option<&'static str>,
    ip: Option<Ipv4Addr>,
    /// Seconds since boot.
    uptime_s: u64,
    /// Free heap in bytes.
    free_heap: u32,
    /// The least free heap since boot, in bytes.
    min_free_heap: u32,
    /// The weather text of the screen (without any notice).
    weather: &'a str,
}

/// What the handlers and the connect hook need.
struct Context {
    topics: Topics,
    discovery_topic: String,
    discovery: Vec<u8>,
    wifi: SharedStatus,
    message: SharedMessage,
    /// When the connection last came up, which is when the broker replays retained messages.
    online_since: Mutex<Option<Instant>>,
}

impl Context {
    /// Runs on every (re)connection: says the board is online, describes it to Home Assistant and
    /// sends the first state.
    fn announce(&self, client: &Client) {
        *self
            .online_since
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(Instant::now());
        publish(
            client,
            &self.topics.availability,
            ONLINE,
            QoS::AtLeastOnce,
            true,
        );
        publish(
            client,
            &self.discovery_topic,
            &self.discovery,
            QoS::AtLeastOnce,
            true,
        );
        self.publish_state(client);
        log::info!(
            "MQTT online, free heap {} bytes (lowest {}, largest block {})",
            platform::free_heap(),
            platform::min_free_heap(),
            platform::largest_free_block()
        );
    }

    fn publish_state(&self, client: &Client) {
        let wifi = self.wifi.snapshot();
        let weather = self.message.text();
        let report = Report {
            ssid: wifi.ssid.as_str(),
            rssi: wifi.rssi_dbm,
            band: wifi.band.map(band_label),
            ip: wifi.ip,
            uptime_s: platform::uptime().as_secs(),
            free_heap: platform::free_heap(),
            min_free_heap: platform::min_free_heap(),
            weather: &weather,
        };
        match serde_json::to_vec(&report) {
            // Retained, so a subscriber that comes later sees the latest state at once.
            Ok(json) => publish(client, &self.topics.state, json, QoS::AtMostOnce, true),
            Err(error) => log::warn!("MQTT: cannot build the state message: {error}"),
        }
    }

    fn handle_command(&self, client: &Client, message: &Message<'_>) {
        // An empty message is how a retained command is cleared on the broker, not a command.
        if message.payload.is_empty() {
            return;
        }
        let Some(command) = message
            .topic
            .strip_prefix(self.topics.command_prefix.as_str())
        else {
            return;
        };
        match command {
            "ping" => publish(
                client,
                &self.topics.pong,
                message.payload,
                QoS::AtMostOnce,
                false,
            ),
            "notice" => self.show_notice(message),
            "restart" => self.restart(client, message),
            other => log::warn!("MQTT: unknown command {other:?}"),
        }
    }

    fn show_notice(&self, message: &Message<'_>) {
        let text = message
            .text()
            .map(|text| sanitize(text, NOTICE_MAX_LEN))
            .filter(|text| !text.is_empty());
        let Some(text) = text else {
            log::warn!("MQTT: the notice has no text that can be shown");
            return;
        };
        log::info!("MQTT: showing a notice of {} characters", text.len());
        self.message.show_notice(text, NOTICE_DURATION);
    }

    /// Whether the connection came up so recently that a command may be a retained message that
    /// the broker replays after the subscribe, see [`RESTART_GRACE`].
    fn just_connected(&self) -> bool {
        self.online_since
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_none_or(|since| since.elapsed() < RESTART_GRACE)
    }

    fn restart(&self, client: &Client, message: &Message<'_>) {
        // Clear the command on the broker if the broker lets us. This is only a courtesy: the
        // publish may be refused (an access rule) or lost (a dropped connection), so the
        // protection against a restart loop is the grace period below.
        publish(client, message.topic, b"", QoS::AtLeastOnce, true);
        if self.just_connected() {
            log::warn!(
                "MQTT: ignored a restart command that came right after connecting \
                 (it is probably a retained message)"
            );
            return;
        }
        log::warn!("restart requested over MQTT");
        // Say so on purpose, instead of leaving it to the Last Will.
        publish(
            client,
            &self.topics.availability,
            OFFLINE,
            QoS::AtLeastOnce,
            true,
        );
        thread::sleep(RESTART_DELAY);
        platform::restart()
    }
}

/// Publishes, and only logs a failure: a lost reading is not worth stopping for.
fn publish(client: &Client, topic: &str, payload: impl AsRef<[u8]>, qos: QoS, retain: bool) {
    if let Err(error) = client.publish(topic, payload, qos, retain) {
        log::warn!("MQTT: cannot publish to '{topic}': {error}");
    }
}

/// Keeps what the display font can show: printable ASCII stays, control characters (a line break,
/// say) become a space, everything else becomes `?`. At most `limit` characters, without spaces at
/// the ends.
fn sanitize(text: &str, limit: usize) -> String {
    let shown: String = text
        .chars()
        .map(|c| match c {
            ' '..='~' => c,
            _ if c.is_control() => ' ',
            _ => '?',
        })
        .take(limit)
        .collect();
    shown.trim().to_owned()
}

const fn band_label(band: Band) -> &'static str {
    match band {
        Band::Ghz2_4 => "2.4 GHz",
        Band::Ghz5 => "5 GHz",
    }
}

/// The last three bytes of the MAC address in hex. A random number takes its place in the
/// (unexpected) case that the address cannot be read.
fn device_id() -> String {
    let [.., a, b, c] = platform::base_mac().unwrap_or_else(|error| {
        log::warn!("cannot read the MAC address: {error}");
        let [_, x, y, z] = platform::random_u32().to_be_bytes();
        [0, 0, 0, x, y, z]
    });
    format!("{a:02x}{b:02x}{c:02x}")
}

/// Starts the set-up thread, see the [module documentation](self).
///
/// # Errors
///
/// Fails if the thread cannot be created. A broker that cannot be reached is **not** an error
/// here: the client keeps trying.
pub fn spawn(
    settings: Settings,
    wifi: SharedStatus,
    message: SharedMessage,
) -> io::Result<JoinHandle<()>> {
    thread::Builder::new()
        .name("telemetry".into())
        .stack_size(TELEMETRY_STACK_BYTES)
        .spawn(move || run(&settings, wifi, message))
}

fn run(settings: &Settings, wifi: SharedStatus, message: SharedMessage) {
    let id = device_id();
    let topics = Topics::new(&id);
    let discovery = home_assistant::config(&Device {
        id: &id,
        availability_topic: &topics.availability,
        state_topic: &topics.state,
        restart_topic: &topics.restart,
        notice_topic: &topics.notice,
    });
    let options = Options {
        client_id: Some(format!("{HOSTNAME}-{id}")),
        will: Some(Will::retained(topics.availability.clone(), OFFLINE)),
        persistent_session: false,
    };
    let context = Arc::new(Context {
        discovery_topic: home_assistant::config_topic(&id),
        discovery: discovery.to_string().into_bytes(),
        topics,
        wifi,
        message,
        online_since: Mutex::new(None),
    });

    while !context.wifi.snapshot().is_online() {
        thread::sleep(NETWORK_POLL);
    }
    let client = connect(settings, &options);

    let handler_context = Arc::clone(&context);
    let subscribed = client.subscribe(
        context.topics.commands.clone(),
        QoS::AtLeastOnce,
        move |client, message| handler_context.handle_command(client, message),
    );
    if let Err(error) = subscribed {
        log::error!("MQTT: cannot subscribe to the commands: {error}");
    }
    let hook_context = Arc::clone(&context);
    client.on_connect(move |client| hook_context.announce(client));

    // The state goes out from the client's own thread, so this one has nothing left to do.
    let tick_context = Arc::clone(&context);
    client.every(TELEMETRY_PERIOD, move |client| {
        if client.is_connected() {
            tick_context.publish_state(client);
        }
    });
    if CLIENT.set(client).is_err() {
        log::error!("MQTT: telemetry was started twice");
    }
}

/// Starts the client, trying again with a growing wait if that fails.
fn connect(settings: &Settings, options: &Options) -> Client {
    let mut failures: u32 = 0;
    loop {
        match Client::connect(settings, options) {
            Ok(client) => return client,
            Err(error) => {
                failures = failures.saturating_add(1);
                let wait = retry_delay(failures);
                log::warn!(
                    "MQTT not started (attempt {failures}): {error}; retrying in {}s",
                    wait.as_secs()
                );
                thread::sleep(wait);
            }
        }
    }
}

/// The wait after the `failures`-th failure in a row: doubles every time up to
/// [`TELEMETRY_RETRY_MAX`].
fn retry_delay(failures: u32) -> Duration {
    let doublings = failures.saturating_sub(1).min(8);
    TELEMETRY_RETRY
        .saturating_mul(1 << doublings)
        .min(TELEMETRY_RETRY_MAX)
}
