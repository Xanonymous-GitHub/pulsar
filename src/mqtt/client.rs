//! The client: connection, publishing, subscriptions with handlers, and the dispatcher thread.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::thread;
use std::time::{Duration, Instant};

use esp_idf_svc::mqtt::client::{
    EspMqttClient, LwtConfiguration, Mqtt5ConnectionPropertyConfig, MqttClientConfiguration,
    MqttProtocolVersion,
};
use esp_idf_svc::sys;

use super::reassemble::{Incoming, Reassembler, Step};
use super::session::{self, Event, Inbound, Wake, Waker};
use super::settings::Settings;
use super::{Error, Message, Options, QoS, topic};
use crate::clock;
use crate::config::{
    HTTP_CLOCK_WAIT, MQTT_KEEP_ALIVE, MQTT_MAX_INCOMING_BYTES, MQTT_NETWORK_TIMEOUT,
    MQTT_OUTBOX_LIMIT_BYTES, MQTT_PROTOCOL_V5, MQTT_RECEIVE_MAXIMUM, MQTT_RECONNECT,
    MQTT_RX_BUFFER_BYTES, MQTT_SESSION_EXPIRY, MQTT_STACK_BYTES, MQTT_TASK_STACK_BYTES,
    MQTT_TLS_TASK_STACK_BYTES, MQTT_TX_BUFFER_BYTES, MQTT_WILL_DELAY,
};

/// Room for the packet header and the topic on top of the payload, for the MQTT 5 maximum packet
/// size we tell the broker.
const PACKET_OVERHEAD_BYTES: usize = 1024;

type Handler = Arc<dyn Fn(&Client, &Message<'_>) + Send + Sync>;
type Hook = Arc<dyn Fn(&Client) + Send + Sync>;

struct Subscription {
    filter: String,
    qos: QoS,
    handler: Handler,
    /// Whether the broker has been asked for it on the current session.
    sent: bool,
}

/// A task that runs again and again.
struct Tick {
    period: Duration,
    due: Instant,
    task: Hook,
}

/// Everything the dispatcher thread and the callers share. One lock, so that "connected" and
/// "what is subscribed" never disagree (see [`Client::subscribe`]).
#[derive(Default)]
struct State {
    connected: bool,
    subscriptions: Vec<Subscription>,
    hooks: Vec<Hook>,
    ticks: Vec<Tick>,
}

struct Shared {
    /// Lock order: `state`, then `esp`.
    esp: Mutex<EspMqttClient<'static>>,
    state: Mutex<State>,
    /// Wakes the dispatcher when its schedule changes.
    waker: Waker,
    /// `host:port`, for the log.
    host: String,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // A poisoned lock only means another thread panicked; the data is still consistent.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A connection to an MQTT broker, see the [module documentation](super).
///
/// Cloning is cheap and gives another handle to the same connection. The connection ends when the
/// last handle is dropped.
#[derive(Clone)]
pub struct Client(Arc<Shared>);

impl Client {
    /// Starts connecting to the broker in `settings` and returns at once. The connection comes up
    /// in the background (and is re-established after a loss); use [`Client::on_connect`] to be
    /// told, and [`Client::subscribe`] before or after it is up.
    ///
    /// # Errors
    ///
    /// Fails if the clock is not set in time (TLS only), the will topic is invalid, the client
    /// cannot be created, or its thread cannot be started. A broker that is unreachable or
    /// refuses the login is **not** an error here: the client keeps trying.
    pub fn connect(settings: &Settings, options: &Options) -> Result<Self, Error> {
        if options
            .will
            .as_ref()
            .is_some_and(|will| !topic::is_valid_name(&will.topic))
        {
            return Err(Error::InvalidTopic);
        }
        // Certificates cannot be checked while the clock still shows 1970.
        if settings.uses_tls() && !clock::wait_synced(HTTP_CLOCK_WAIT) {
            return Err(Error::ClockNotSynced);
        }

        let (outbound, inbound) = session::channel();
        let waker = outbound.waker();
        let esp = EspMqttClient::new_cb(
            settings.url(),
            &configuration(settings, options),
            move |event| outbound.forward(&event),
        )?;
        let shared = Arc::new(Shared {
            esp: Mutex::new(esp),
            state: Mutex::new(State::default()),
            waker,
            host: settings.host().to_owned(),
        });

        // The thread holds only a weak reference, so dropping the last `Client` ends it.
        let weak = Arc::downgrade(&shared);
        thread::Builder::new()
            .name("mqtt".into())
            .stack_size(MQTT_STACK_BYTES)
            .spawn(move || dispatch(&weak, &inbound))?;
        Ok(Self(shared))
    }

    /// Whether the broker connection is up right now.
    #[must_use]
    pub fn is_connected(&self) -> bool {
        lock(&self.0.state).connected
    }

    /// How many bytes of messages wait to be sent or acknowledged.
    #[must_use]
    pub fn outbox_bytes(&self) -> usize {
        lock(&self.0.esp).get_outbox_size()
    }

    /// Queues a message and returns at once; it is sent when the connection is up (also after a
    /// loss), as long as the outbox has room. Without a connection a `QoS::AtMostOnce` message
    /// waits too, so check [`Client::is_connected`] first for readings that go stale.
    ///
    /// # Errors
    ///
    /// Fails if the topic is invalid, the outbox is full ([`Error::OutboxFull`]), or the client
    /// fails.
    pub fn publish(
        &self,
        topic: &str,
        payload: impl AsRef<[u8]>,
        qos: QoS,
        retain: bool,
    ) -> Result<(), Error> {
        if !topic::is_valid_name(topic) {
            return Err(Error::InvalidTopic);
        }
        lock(&self.0.esp).enqueue(topic, qos.into(), retain, payload.as_ref())?;
        Ok(())
    }

    /// Subscribes to `filter`, and calls `handler` for every message that matches it. Subscribing
    /// again to the same filter replaces the handler.
    ///
    /// Returns once the subscription is stored. It is sent to the broker right away if the
    /// connection is up, and otherwise (and again after every reconnect without a kept session)
    /// when it comes up. The client does not see the broker's answer, so a subscription that the
    /// broker refuses (an access rule, say) fails silently.
    ///
    /// Handlers run one at a time on the `mqtt` thread: keep them short, and do not panic. They
    /// may call any method of the [`Client`] they are given.
    ///
    /// # Errors
    ///
    /// Fails if the filter is invalid.
    pub fn subscribe(
        &self,
        filter: impl Into<String>,
        qos: QoS,
        handler: impl Fn(&Self, &Message<'_>) + Send + Sync + 'static,
    ) -> Result<(), Error> {
        let filter = filter.into();
        if !topic::is_valid_filter(&filter) {
            return Err(Error::InvalidTopic);
        }
        let mut state = lock(&self.0.state);
        state
            .subscriptions
            .retain(|subscription| subscription.filter != filter);
        state.subscriptions.push(Subscription {
            filter,
            qos,
            handler: Arc::new(handler),
            sent: false,
        });
        if state.connected {
            self.send_pending(&mut state);
        }
        Ok(())
    }

    /// Cancels the subscription to `filter`. Returns whether there was one.
    ///
    /// # Errors
    ///
    /// Fails if the broker cannot be told.
    pub fn unsubscribe(&self, filter: &str) -> Result<bool, Error> {
        let mut state = lock(&self.0.state);
        let Some(position) = state
            .subscriptions
            .iter()
            .position(|subscription| subscription.filter == filter)
        else {
            return Ok(false);
        };
        let removed = state.subscriptions.remove(position);
        if removed.sent && state.connected {
            lock(&self.0.esp).unsubscribe(filter)?;
        }
        Ok(true)
    }

    /// Calls `hook` every time the connection comes up, after the subscriptions have been sent.
    /// If it is up already, `hook` also runs once now. This is the place to publish the "online"
    /// message and the first readings.
    ///
    /// The hook runs on the `mqtt` thread (or on the calling thread for the run right now): keep
    /// it short.
    pub fn on_connect(&self, hook: impl Fn(&Self) + Send + Sync + 'static) {
        let hook: Hook = Arc::new(hook);
        let run_now = {
            let mut state = lock(&self.0.state);
            state.hooks.push(Arc::clone(&hook));
            state.connected
        };
        if run_now {
            hook(self);
        }
    }

    /// Calls `task` every `period`, the first time after one `period`, whether or not the
    /// connection is up (check [`Client::is_connected`] if it matters). It saves a thread of your
    /// own for the readings that are sent from time to time.
    ///
    /// The task runs on the `mqtt` thread: keep it short. A period that passed while a handler was
    /// busy is not made up for.
    pub fn every(&self, period: Duration, task: impl Fn(&Self) + Send + Sync + 'static) {
        lock(&self.0.state).ticks.push(Tick {
            period,
            due: Instant::now() + period,
            task: Arc::new(task),
        });
        // The dispatcher may be waiting with no deadline at all.
        self.0.waker.reschedule();
    }

    /// How long until the next periodic task is due, if there is one.
    fn next_tick_in(&self) -> Option<Duration> {
        let now = Instant::now();
        lock(&self.0.state)
            .ticks
            .iter()
            .map(|tick| tick.due.saturating_duration_since(now))
            .min()
    }

    /// Runs the periodic tasks that are due.
    fn run_due_ticks(&self) {
        let now = Instant::now();
        let due: Vec<Hook> = lock(&self.0.state)
            .ticks
            .iter_mut()
            .filter(|tick| tick.due <= now)
            .map(|tick| {
                tick.due = now + tick.period;
                Arc::clone(&tick.task)
            })
            .collect();
        for task in due {
            task(self);
        }
    }

    /// Asks the broker for every subscription that it has not been asked for yet. A failure is
    /// logged and left for the next connection. Call it with the state locked.
    fn send_pending(&self, state: &mut State) {
        let mut esp = lock(&self.0.esp);
        for subscription in state.subscriptions.iter_mut().filter(|s| !s.sent) {
            match esp.subscribe(&subscription.filter, subscription.qos.into()) {
                Ok(_) => subscription.sent = true,
                Err(error) => log::warn!(
                    "MQTT: cannot subscribe to '{}': {error}",
                    subscription.filter
                ),
            }
        }
    }

    fn on_connected(&self, session_present: bool) {
        log::info!(
            "MQTT connected to {} (session kept: {session_present})",
            self.0.host
        );
        let hooks = {
            let mut state = lock(&self.0.state);
            state.connected = true;
            if !session_present {
                for subscription in &mut state.subscriptions {
                    subscription.sent = false;
                }
            }
            self.send_pending(&mut state);
            state.hooks.clone()
        };
        for hook in hooks {
            hook(self);
        }
    }

    fn on_disconnected(&self) {
        log::warn!("MQTT disconnected from {}", self.0.host);
        lock(&self.0.state).connected = false;
    }

    /// Calls the handler of every subscription that `incoming` matches.
    fn deliver(&self, incoming: &Incoming) {
        let handlers: Vec<Handler> = lock(&self.0.state)
            .subscriptions
            .iter()
            .filter(|subscription| topic::matches(&subscription.filter, &incoming.topic))
            .map(|subscription| Arc::clone(&subscription.handler))
            .collect();
        let message = Message {
            topic: &incoming.topic,
            payload: &incoming.payload,
        };
        for handler in handlers {
            handler(self, &message);
        }
    }
}

/// The body of the `mqtt` thread: reacts to the events of the MQTT task and runs the periodic
/// tasks, until the client is gone.
fn dispatch(client: &Weak<Shared>, inbound: &Inbound) {
    let mut reassembler = Reassembler::new(MQTT_MAX_INCOMING_BYTES);
    loop {
        // Not holding a strong reference while waiting, so that dropping the last `Client` ends
        // the connection.
        let Some(wait) = client.upgrade().map(|shared| Client(shared).next_tick_in()) else {
            return;
        };
        let wake = inbound.recv(wait);
        let Some(shared) = client.upgrade() else {
            return;
        };
        let client = Client(shared);
        match wake {
            Wake::Closed => return,
            // Nothing to do but go round again: the wait is worked out afresh at the top.
            Wake::Idle | Wake::Event(Event::Reschedule) => {}
            Wake::Event(Event::Connected { session_present }) => {
                reassembler.reset();
                client.on_connected(session_present);
            }
            Wake::Event(Event::Disconnected) => {
                reassembler.reset();
                client.on_disconnected();
            }
            Wake::Event(Event::Failed) => {
                log::warn!("MQTT: connection error (see the lines above)");
            }
            Wake::Event(Event::Data {
                topic,
                payload,
                chunk,
            }) => match reassembler.push(topic, payload, chunk) {
                Step::Complete(incoming) => client.deliver(&incoming),
                Step::Dropped(reason) => log::warn!("MQTT: dropped a message: {reason:?}"),
                Step::Pending => {}
            },
        }
        // After every wake-up, so that a busy stream of messages cannot hold the tasks back.
        client.run_due_ticks();
    }
}

/// The settings for the ESP-IDF client.
fn configuration<'a>(settings: &'a Settings, options: &'a Options) -> MqttClientConfiguration<'a> {
    // MQTT 5 only: the connect properties. They ask the broker to keep the session a while, to
    // hold back the Last Will so that a quick reconnect cancels it, and to stay within our limits.
    let mqtt5 = MQTT_PROTOCOL_V5.then(|| Mqtt5ConnectionPropertyConfig {
        session_expiry_interval: Some(seconds(MQTT_SESSION_EXPIRY)),
        will_delay_interval: Some(seconds(MQTT_WILL_DELAY)),
        receive_maximum: Some(MQTT_RECEIVE_MAXIMUM),
        maximum_packet_size: u32::try_from(MQTT_MAX_INCOMING_BYTES + PACKET_OVERHEAD_BYTES).ok(),
        ..Mqtt5ConnectionPropertyConfig::default()
    });

    MqttClientConfiguration {
        protocol_version: Some(if MQTT_PROTOCOL_V5 {
            MqttProtocolVersion::V5
        } else {
            MqttProtocolVersion::V3_1_1
        }),
        mqtt5_connection_property: mqtt5,
        client_id: options.client_id.as_deref(),
        keep_alive_interval: Some(MQTT_KEEP_ALIVE),
        reconnect_timeout: Some(MQTT_RECONNECT),
        network_timeout: MQTT_NETWORK_TIMEOUT,
        lwt: options.will.as_ref().map(|will| LwtConfiguration {
            topic: &will.topic,
            payload: &will.payload,
            qos: will.qos.into(),
            retain: will.retain,
        }),
        disable_clean_session: options.persistent_session,
        task_stack: if settings.uses_tls() {
            MQTT_TLS_TASK_STACK_BYTES
        } else {
            MQTT_TASK_STACK_BYTES
        },
        buffer_size: MQTT_RX_BUFFER_BYTES,
        out_buffer_size: MQTT_TX_BUFFER_BYTES,
        outbox_limit: Some(MQTT_OUTBOX_LIMIT_BYTES),
        username: settings.username(),
        password: settings.password(),
        // The server certificate is checked against the Mozilla bundle, as for HTTPS.
        crt_bundle_attach: Some(sys::esp_crt_bundle_attach),
        ..MqttClientConfiguration::default()
    }
}

/// Whole seconds, saturating, as the MQTT 5 properties want them.
fn seconds(duration: std::time::Duration) -> u32 {
    u32::try_from(duration.as_secs()).unwrap_or(u32::MAX)
}
