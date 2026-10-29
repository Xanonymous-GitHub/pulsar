//! Every tunable constant in one place.

use std::time::Duration;

/// How long the user LED stays on or off.
pub const LED_PERIOD: Duration = Duration::from_millis(100);

/// Stack size of the LED thread, in bytes.
pub const LED_STACK_BYTES: usize = 2 * 1024;

/// The text that scrolls on the OLED until the first weather report arrives. Only ASCII is
/// supported by the built-in font.
pub const SCROLL_PLACEHOLDER: &str = "Fetching weather...";

/// Pixels the scrolling text moves on every frame.
pub const SCROLL_STEP: i32 = 2;

/// Time between two frames of the screen.
pub const FRAME_PERIOD: Duration = Duration::from_millis(40);

/// Time of one on-and-off cycle of the Bluetooth icon while it blinks (not paired yet). The icon is
/// on for the first half and off for the second.
pub const BLUETOOTH_BLINK_PERIOD: Duration = Duration::from_secs(1);

/// Height of the Wi-Fi status bar at the top of the screen, in pixels. The scroller gets the rest.
pub const STATUS_BAR_HEIGHT: u32 = 24;

/// Stack size of the screen thread, in bytes. It owns the 1 KiB display buffer.
pub const SCREEN_STACK_BYTES: usize = 8 * 1024;

/// Speed of the I2C bus the OLED is on, in hertz.
pub const I2C_BAUDRATE_HZ: u32 = 400_000;

/// Name of the secret that holds the Wi-Fi credentials.
pub const WIFI_SECRET_KEY: &str = "wifi";

/// Stack size of the Wi-Fi supervisor thread, in bytes.
pub const WIFI_STACK_BYTES: usize = 8 * 1024;

/// How often the Wi-Fi supervisor refreshes signal strength and address while connected.
pub const WIFI_POLL_PERIOD: Duration = Duration::from_secs(2);

/// How long one connection attempt may take, counted from the start of the scan, for the link and
/// for the IP address (each). Scanning both bands takes about 11 seconds, and Bluetooth, which
/// shares the 2.4 GHz radio, makes it longer.
pub const WIFI_CONNECT_TIMEOUT: Duration = Duration::from_secs(45);

/// Consecutive failed connection attempts before the status shows "failed". Retrying goes on.
pub const WIFI_FAILED_AFTER: u32 = 3;

/// Wait before the first retry; it doubles after every failure up to [`WIFI_BACKOFF_MAX`].
pub const WIFI_BACKOFF_START: Duration = Duration::from_secs(1);

/// Longest wait between two connection attempts.
pub const WIFI_BACKOFF_MAX: Duration = Duration::from_secs(30);

/// A 5 GHz access point wins over a 2.4 GHz one when its signal is at most this many dB weaker.
/// 5 GHz normally arrives 6 to 10 dB weaker than 2.4 GHz at the same place, so without this the
/// 2.4 GHz network would almost always win although 5 GHz is faster and less crowded. With it,
/// 2.4 GHz is still used when it is much stronger, or when no 5 GHz network is in range.
pub const WIFI_PREFER_5GHZ_MARGIN_DB: u8 = 10;

/// The name the router shows for this device (sent as the DHCP hostname). At most 30 bytes.
pub const HOSTNAME: &str = "pulsar";

/// A clock that shows a time before this (seconds since the Unix epoch) has not been set yet: the
/// board starts in 1970. This is 2026-01-01 00:00:00 UTC (490,896 hours). Raise it from time to
/// time.
pub const CLOCK_VALID_AFTER: Duration = Duration::from_hours(490_896);

/// The local time zone as an offset from UTC. Taipei (Asia/Taipei) is UTC+8 all year, with no
/// daylight saving time since 1979, so a fixed offset is exact. A zone with daylight saving time
/// would need a time zone database instead.
pub const LOCAL_UTC_OFFSET: Duration = Duration::from_hours(8);

/// Where the weather comes from: the free Open-Meteo forecast API (no key, HTTPS), asked for the
/// current conditions and today's range at the coordinates of Taipei, in Taipei local time.
pub const WEATHER_URL: &str = "https://api.open-meteo.com/v1/forecast?latitude=25.05&longitude=121.53&current=temperature_2m,weather_code,wind_speed_10m&daily=temperature_2m_max,temperature_2m_min,precipitation_probability_max&timezone=Asia%2FTaipei&forecast_days=1";

/// The place name shown in front of the weather report. It has to match [`WEATHER_URL`].
pub const WEATHER_PLACE: &str = "Taipei";

/// Time between two successful weather updates.
pub const WEATHER_REFRESH: Duration = Duration::from_mins(15);

/// Wait before trying again after a failed weather update. It doubles after every further
/// failure up to [`WEATHER_RETRY_MAX`].
pub const WEATHER_RETRY: Duration = Duration::from_secs(30);

/// Longest wait between two weather updates while they keep failing.
pub const WEATHER_RETRY_MAX: Duration = Duration::from_mins(5);

/// A report older than this is shown with `(old)` in front, so a long outage is not mistaken for
/// current weather. It is a bit more than two missed updates.
pub const WEATHER_STALE_AFTER: Duration = Duration::from_mins(35);

/// Stack size of the weather thread, in bytes. The TLS handshake needs most of it.
pub const WEATHER_STACK_BYTES: usize = 16 * 1024;

/// How long an HTTP request may wait for network data (connect, send or receive) before it fails.
pub const HTTP_TIMEOUT: Duration = Duration::from_secs(15);

/// Receive buffer of the HTTP client, in bytes. It also limits the longest single response header
/// (long `Set-Cookie` or `Content-Security-Policy` headers do not fit in the 512-byte default).
pub const HTTP_RX_BUFFER_BYTES: usize = 4 * 1024;

/// Send buffer of the HTTP client, in bytes. It limits the request line plus all request headers.
pub const HTTP_TX_BUFFER_BYTES: usize = 2 * 1024;

/// The most bytes `Response::bytes` and `Response::text` read into memory. Stream larger bodies
/// with `std::io::Read` instead.
pub const HTTP_MAX_BODY_BYTES: usize = 64 * 1024;

/// How long an HTTPS request waits for the clock to be set before it gives up. Certificates cannot
/// be checked while the clock still shows 1970.
pub const HTTP_CLOCK_WAIT: Duration = Duration::from_secs(30);

/// Name of the secret that holds the Bluetooth bonds (the keys of the paired phones).
pub const BLE_BONDS_SECRET_KEY: &str = "ble-bonds";

/// How long Bluetooth waits for Wi-Fi to finish its first connection attempt before it starts
/// advertising. While an advertising set is on, Wi-Fi's scan of both bands takes about 24 seconds
/// instead of 13 (measured, whatever the advertising interval and PHY), so Wi-Fi goes first.
/// Bluetooth starts at once when Wi-Fi is not trying to connect at all.
pub const BLE_WIFI_WAIT: Duration = Duration::from_secs(60);

/// Time between two advertising packets of each set, in milliseconds. Shorter is found faster
/// and costs more power.
pub const BLE_ADV_INTERVAL_MS: u32 = 100;

/// The largest ATT payload (MTU) the board offers a phone, in bytes. The phone picks the final
/// value; 512 is the largest value an attribute can have (the MTU itself may be up to 527).
pub const BLE_PREFERRED_MTU: u16 = 512;

/// How often the Bluetooth thread looks for something to publish: new Wi-Fi or weather text for
/// the phone, and bonds that have to be saved.
pub const BLE_POLL_PERIOD: Duration = Duration::from_secs(1);

/// Stack size of the Bluetooth thread, in bytes.
pub const BLE_STACK_BYTES: usize = 6 * 1024;

/// How often the Bluetooth thread looks at Wi-Fi while it waits for Wi-Fi's first connection
/// attempt to end, see [`BLE_WIFI_WAIT`].
pub const BLE_WIFI_POLL: Duration = Duration::from_millis(250);

/// The manufacturer name in the Device Information service.
pub const BLE_MANUFACTURER: &str = "pulsar";

/// The model name in the Device Information service.
pub const BLE_MODEL: &str = "XIAO ESP32-C5";

/// How long one pass of the Observer (scanning) lasts, as the default for `bluetooth::scan`.
pub const BLE_SCAN_DURATION: Duration = Duration::from_secs(5);

/// Scan interval of the Observer, in milliseconds.
pub const BLE_SCAN_INTERVAL_MS: u16 = 100;

/// Scan window of the Observer, in milliseconds. It cannot be longer than the interval; a shorter
/// window leaves more radio time for Wi-Fi.
pub const BLE_SCAN_WINDOW_MS: u16 = 50;

/// A scan once advertising has started, to show in the log that the Observer works.
/// `Duration::ZERO` turns it off.
pub const BLE_BOOT_SCAN: Duration = Duration::from_secs(3);

/// Name of the secret that holds the MQTT broker URL and login.
pub const MQTT_SECRET_KEY: &str = "mqtt";

/// Whether to speak MQTT 5.0. `false` speaks 3.1.1, for brokers that do not know version 5. The
/// MQTT 5 settings below (session expiry, will delay, receive maximum) are ignored then.
pub const MQTT_PROTOCOL_V5: bool = true;

/// How often the client pings the broker while it is idle. The broker drops the connection after
/// one and a half times this. It is also how fast a dead link is noticed.
pub const MQTT_KEEP_ALIVE: Duration = Duration::from_secs(30);

/// Wait between two reconnection attempts after the connection is lost (the MQTT task does the
/// reconnecting by itself).
pub const MQTT_RECONNECT: Duration = Duration::from_secs(10);

/// How long the broker connection (connect, TLS handshake, send, receive) may wait for network
/// data before it fails.
pub const MQTT_NETWORK_TIMEOUT: Duration = Duration::from_secs(10);

/// MQTT 5: how long the broker keeps the session after a disconnect. It must be at least
/// [`MQTT_WILL_DELAY`], or the Last Will goes out at once.
pub const MQTT_SESSION_EXPIRY: Duration = Duration::from_mins(5);

/// MQTT 5: how long the broker waits after a lost connection before it publishes the Last Will. A
/// quick reconnect (a Wi-Fi hiccup) cancels it, so the availability does not flap.
pub const MQTT_WILL_DELAY: Duration = Duration::from_secs(15);

/// MQTT 5: how many messages with `QoS` 1 or 2 the broker may send without waiting for our
/// acknowledgement.
pub const MQTT_RECEIVE_MAXIMUM: u16 = 8;

/// Receive buffer of the MQTT client, in bytes. A longer message arrives in several pieces, which
/// the client puts back together.
pub const MQTT_RX_BUFFER_BYTES: usize = 2 * 1024;

/// Send buffer of the MQTT client, in bytes. It must hold the packet header and the topic; a
/// longer payload (the Home Assistant discovery message) is sent in pieces.
pub const MQTT_TX_BUFFER_BYTES: usize = 1024;

/// How many bytes of messages may wait to be sent (while offline, or for an acknowledgement). A
/// publish beyond that fails with `mqtt::Error::OutboxFull`.
pub const MQTT_OUTBOX_LIMIT_BYTES: usize = 16 * 1024;

/// The longest incoming message the client accepts, in bytes. Longer ones are dropped with a
/// warning. For MQTT 5 the broker is told, so it does not even send them. Keep it well below the
/// largest TLS record (`CONFIG_MBEDTLS_SSL_IN_CONTENT_LEN`, 8 KiB in `sdkconfig.defaults`): a
/// message that does not fit in one record breaks the TLS connection, and with MQTT 3.1.1 the broker
/// cannot be told.
pub const MQTT_MAX_INCOMING_BYTES: usize = 6 * 1024;

/// How many bytes of incoming messages may wait for the dispatcher thread, counting the payload,
/// the topic and the bookkeeping of each. Beyond that, messages are dropped (and logged), so a slow
/// handler cannot fill the memory, whatever the messages look like.
pub const MQTT_QUEUE_BYTES: usize = 12 * 1024;

/// Stack size of the ESP-IDF MQTT task for `mqtt://` and `ws://`, in bytes. This is ESP-IDF's own
/// default.
pub const MQTT_TASK_STACK_BYTES: usize = 6 * 1024;

/// Stack size of the ESP-IDF MQTT task for `mqtts://` and `wss://`, in bytes. The TLS handshake
/// runs on it.
pub const MQTT_TLS_TASK_STACK_BYTES: usize = 10 * 1024;

/// Stack size of the thread that runs the subscription handlers, connect hooks and periodic
/// tasks, in bytes. Every thread costs its whole stack in heap, which is scarce: about 80 KB are
/// free with Wi-Fi, Bluetooth and the display running, and one TLS session needs about 40 KB.
pub const MQTT_STACK_BYTES: usize = 5 * 1024;

/// The first part of every topic the board publishes and subscribes to: `pulsar/<device>/...`.
pub const MQTT_TOPIC_PREFIX: &str = "pulsar";

/// Time between two state messages.
pub const TELEMETRY_PERIOD: Duration = Duration::from_mins(1);

/// Wait before trying to connect to the broker again after a failed start. It doubles after every
/// further failure up to [`TELEMETRY_RETRY_MAX`].
pub const TELEMETRY_RETRY: Duration = Duration::from_secs(5);

/// Longest wait between two attempts to start the MQTT client.
pub const TELEMETRY_RETRY_MAX: Duration = Duration::from_mins(2);

/// Stack size of the telemetry set-up thread, in bytes. It builds the Home Assistant discovery
/// message (JSON), connects, and then ends, which gives the stack back.
pub const TELEMETRY_STACK_BYTES: usize = 8 * 1024;

/// How long a text sent with the `notice` command stays on the screen.
pub const NOTICE_DURATION: Duration = Duration::from_secs(30);

/// The longest notice in characters; more is cut off. Only ASCII is shown (the display font has
/// no other characters).
pub const NOTICE_MAX_LEN: usize = 100;

/// How long the restart command waits after it announced that the board goes offline, so that the
/// message can leave before the chip resets.
pub const RESTART_DELAY: Duration = Duration::from_secs(2);

/// A restart command that arrives within this time after the connection came up is ignored. A
/// command that was published retained is sent again by the broker after every subscribe, so this
/// is what keeps such a message from restarting the board over and over, whether or not the board
/// is allowed to clear it. A real command inside the window has to be sent again.
pub const RESTART_GRACE: Duration = Duration::from_secs(10);

/// The topic prefix Home Assistant listens to for discovery messages (its default).
pub const HA_DISCOVERY_PREFIX: &str = "homeassistant";
