//! The Wi-Fi status that the network code publishes and the screen shows.
//!
//! Nothing here touches hardware. [`SharedStatus`] is a cheap handle (clone it for every thread
//! that needs it); the lock is only held while copying a few bytes.

use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex, PoisonError};

/// Longest SSID in bytes (802.11 limit).
pub const MAX_SSID_LEN: usize = 32;

/// How the Wi-Fi link is doing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LinkState {
    /// Wi-Fi has not been started.
    #[default]
    Idle,
    /// No credentials were available, so Wi-Fi was not started.
    NoCredentials,
    /// Trying to join the network.
    Connecting,
    /// Associated with the access point (an IP address may still be pending).
    Connected,
    /// Several attempts in a row failed; still retrying.
    Failed,
}

/// The frequency band of the access point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Band {
    /// 2.4 GHz (channels 1 to 14).
    Ghz2_4,
    /// 5 GHz.
    Ghz5,
}

impl Band {
    /// The band a Wi-Fi channel number belongs to.
    #[must_use]
    pub const fn from_channel(channel: u8) -> Self {
        if channel <= 14 {
            Self::Ghz2_4
        } else {
            Self::Ghz5
        }
    }
}

/// A snapshot of the Wi-Fi link.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WifiStatus {
    /// How the link is doing.
    pub state: LinkState,
    /// The network name, empty when unknown. Longer names are cut to [`MAX_SSID_LEN`] bytes.
    pub ssid: heapless::String<MAX_SSID_LEN>,
    /// Signal strength of the access point in dBm (about -30 is excellent, -90 is unusable).
    pub rssi_dbm: Option<i32>,
    /// The band of the access point.
    pub band: Option<Band>,
    /// The address from DHCP, once there is one.
    pub ip: Option<Ipv4Addr>,
}

/// Number of levels of the signal icon (0 is no usable signal, so there are one more values).
pub const SIGNAL_LEVELS: u8 = 5;

/// Turns a signal strength into 0 to [`SIGNAL_LEVELS`] bars (the usual phone-style scale).
#[must_use]
pub const fn signal_bars(rssi_dbm: i32) -> u8 {
    match rssi_dbm {
        -55.. => 5,
        -62..=-56 => 4,
        -69..=-63 => 3,
        -76..=-70 => 2,
        -85..=-77 => 1,
        _ => 0,
    }
}

/// A [`WifiStatus`] shared between threads.
#[derive(Clone, Debug, Default)]
pub struct SharedStatus(Arc<Mutex<WifiStatus>>);

impl SharedStatus {
    /// A copy of the current status.
    #[must_use]
    pub fn snapshot(&self) -> WifiStatus {
        // A poisoned lock only means another thread panicked; the data is still a valid status.
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Changes the status in place. Keep `change` short: the lock is held while it runs.
    pub fn update(&self, change: impl FnOnce(&mut WifiStatus)) {
        change(&mut self.0.lock().unwrap_or_else(PoisonError::into_inner));
    }
}
