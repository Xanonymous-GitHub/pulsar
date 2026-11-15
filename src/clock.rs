//! The wall clock, set over the network with SNTP.
//!
//! The board has no battery-backed clock and starts in 1970. TLS needs the right time to check
//! the validity dates of certificates, so [`crate::http`] waits for [`wait_synced`] before every
//! HTTPS request.
//!
//! ```ignore
//! clock::start()?; // once, after Wi-Fi has started
//! if clock::wait_synced(Duration::from_secs(30)) {
//!     // `SystemTime::now()` is now correct
//! }
//! let now = clock::local_time(); // Some(14:05:09), None until the clock is set
//! ```
//!
//! SNTP keeps running and corrects the clock from time to time on its own. A clock that kept its
//! time through a reset of the board also counts as set, even before SNTP has run.
//!
//! # Getting the time quickly
//!
//! SNTP starts with Wi-Fi, so its first queries go out before there is a connection, and its
//! retry timer then backs off: left alone, the clock is set about 20 seconds after the address
//! arrives. [`resync`] makes it ask again at once. [`wait_synced`] calls it when the clock is not
//! set, and the weather thread calls it whenever Wi-Fi comes up.
//!
//! # The time zone
//!
//! The board has no time zone database. Local time is UTC plus an offset in seconds, which
//! starts as [`DEFAULT_UTC_OFFSET_SECONDS`] and is replaced with [`set_utc_offset`] by the
//! weather thread: the weather service resolves the zone of the board's location and reports the
//! offset that applies now, daylight saving time included. The arithmetic is in [`LocalTime`].

mod local_time;

use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use esp_idf_svc::sntp::{EspSntp, SntpConf};
use esp_idf_svc::sys::EspError;

use crate::config::{CLOCK_RESYNC_MIN_INTERVAL, CLOCK_VALID_AFTER, DEFAULT_UTC_OFFSET_SECONDS};
use crate::platform;

pub use local_time::{LocalTime, is_valid_utc_offset};

/// How often [`wait_synced`] looks at the clock.
const POLL_PERIOD: Duration = Duration::from_millis(250);

/// The offset of local time from UTC, in seconds. See [`set_utc_offset`].
static UTC_OFFSET_SECONDS: AtomicI32 = AtomicI32::new(DEFAULT_UTC_OFFSET_SECONDS);

/// When SNTP was last made to ask again, see [`resync`].
static LAST_RESYNC: Mutex<Option<Instant>> = Mutex::new(None);

/// Keeps the SNTP service alive: it stops when this handle is dropped.
static SNTP: OnceLock<EspSntp<'static>> = OnceLock::new();

/// Starts SNTP with the `pool.ntp.org` servers and returns at once. Does nothing if it is already
/// running. Call it after Wi-Fi has started, so the network stack exists.
///
/// # Errors
///
/// Fails if the SNTP service cannot be started.
pub fn start() -> Result<(), EspError> {
    if SNTP.get().is_some() {
        return Ok(());
    }
    let sntp = EspSntp::new_with_callback(&SntpConf::default(), |_| {
        if let Some(now) = local_time() {
            log::info!("clock synced, local time {now}");
        }
    })?;
    // Losing a race against another caller is fine: the extra handle is dropped.
    let _ = SNTP.set(sntp);
    Ok(())
}

/// The time since 1970-01-01 00:00 UTC, or `None` while the clock has not been set.
fn synced_unix_time() -> Option<Duration> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .filter(|since_epoch| *since_epoch >= CLOCK_VALID_AFTER)
}

/// Whether the clock has been set (it shows a date after [`CLOCK_VALID_AFTER`]).
#[must_use]
pub fn is_synced() -> bool {
    synced_unix_time().is_some()
}

/// Makes SNTP ask its servers for the time now, instead of when its retry timer runs out. Call
/// it when the network has just come up. It does nothing if SNTP is not running, or if it was
/// called less than [`CLOCK_RESYNC_MIN_INTERVAL`] ago (time servers must not be asked too often).
pub fn resync() {
    let mut last = LAST_RESYNC.lock().unwrap_or_else(PoisonError::into_inner);
    if last.is_some_and(|last| last.elapsed() < CLOCK_RESYNC_MIN_INTERVAL) {
        return;
    }
    if platform::sntp_restart() {
        *last = Some(Instant::now());
    }
}

/// The offset of local time from UTC that is in use, in seconds.
#[must_use]
pub fn utc_offset() -> i32 {
    UTC_OFFSET_SECONDS.load(Ordering::Relaxed)
}

/// Sets the offset of local time from UTC, in seconds east of Greenwich. Returns `false`, and
/// changes nothing, for an offset no time zone has (see [`is_valid_utc_offset`]).
pub fn set_utc_offset(seconds: i32) -> bool {
    if !is_valid_utc_offset(seconds) {
        return false;
    }
    if UTC_OFFSET_SECONDS.swap(seconds, Ordering::Relaxed) != seconds {
        let sign = if seconds < 0 { '-' } else { '+' };
        let minutes = seconds.unsigned_abs() / 60;
        log::info!(
            "local time is now UTC{sign}{}:{:02}",
            minutes / 60,
            minutes % 60
        );
    }
    true
}

/// The local time of day, or `None` while the clock has not been set.
#[must_use]
pub fn local_time() -> Option<LocalTime> {
    synced_unix_time().map(|since_epoch| LocalTime::from_unix(since_epoch.as_secs(), utc_offset()))
}

/// Waits until the clock is set, for at most `timeout`. Returns whether it is set. It returns
/// after the full `timeout` if [`start`] was never called.
///
/// A caller is about to use the network, so the network is most likely up: if the clock is not
/// set, SNTP is made to ask again now (see [`resync`]).
#[must_use]
pub fn wait_synced(timeout: Duration) -> bool {
    if !is_synced() {
        resync();
    }
    let mut waited = Duration::ZERO;
    while !is_synced() {
        if waited >= timeout {
            return false;
        }
        thread::sleep(POLL_PERIOD);
        waited += POLL_PERIOD;
    }
    true
}
