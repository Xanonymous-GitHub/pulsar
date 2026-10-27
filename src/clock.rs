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
//! let now = clock::local_time(); // Some(14:05:09) in Taipei, None until the clock is set
//! ```
//!
//! SNTP keeps running and corrects the clock from time to time on its own. A clock that kept its
//! time through a reset of the board also counts as set, even before SNTP has run.

use std::fmt;
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use esp_idf_svc::sntp::{EspSntp, SntpConf};
use esp_idf_svc::sys::EspError;

use crate::config::{CLOCK_VALID_AFTER, LOCAL_UTC_OFFSET};

/// How often [`wait_synced`] looks at the clock.
const POLL_PERIOD: Duration = Duration::from_millis(250);

/// Seconds in a day.
const SECONDS_PER_DAY: u64 = 24 * 60 * 60;

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

/// A time of day without a date.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalTime {
    /// Hour, 0 to 23.
    pub hour: u8,
    /// Minute, 0 to 59.
    pub minute: u8,
    /// Second, 0 to 59.
    pub second: u8,
}

impl LocalTime {
    /// The local time of day at `since_epoch` after 1970-01-01 00:00 UTC. The zone is
    /// [`LOCAL_UTC_OFFSET`].
    #[must_use]
    pub fn from_unix(since_epoch: Duration) -> Self {
        let seconds = (since_epoch + LOCAL_UTC_OFFSET).as_secs() % SECONDS_PER_DAY;
        // The remainder is below one day, so each part is below 24, 60 and 60.
        let part = |value: u64| u8::try_from(value).unwrap_or_default();
        Self {
            hour: part(seconds / 3600),
            minute: part(seconds / 60 % 60),
            second: part(seconds % 60),
        }
    }
}

impl fmt::Display for LocalTime {
    /// Formats as `HH:MM:SS`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:02}:{:02}:{:02}", self.hour, self.minute, self.second)
    }
}

/// The local time of day, or `None` while the clock has not been set.
#[must_use]
pub fn local_time() -> Option<LocalTime> {
    synced_unix_time().map(LocalTime::from_unix)
}

/// Waits until the clock is set, for at most `timeout`. Returns whether it is set. It returns
/// after the full `timeout` if [`start`] was never called.
#[must_use]
pub fn wait_synced(timeout: Duration) -> bool {
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
