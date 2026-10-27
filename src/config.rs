//! Every tunable constant in one place.

use std::time::Duration;

/// How long the user LED stays on or off.
pub const LED_PERIOD: Duration = Duration::from_millis(100);

/// Stack size of the LED thread, in bytes.
pub const LED_STACK_BYTES: usize = 2 * 1024;

/// The text that scrolls on the OLED. Only ASCII is supported by the built-in font.
pub const SCROLL_TEXT: &str = "Hello from ESP32-C5!";

/// Pixels the scrolling text moves on every frame.
pub const SCROLL_STEP: i32 = 2;

/// Time between two frames of the screen.
pub const FRAME_PERIOD: Duration = Duration::from_millis(40);

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
