//! The Observer role: listing the Bluetooth devices that advertise nearby.
//!
//! It scans with extended scanning, so it also hears advertisements on the LE Coded PHY. Scanning
//! goes on while the board advertises and while a phone is connected.

use std::time::Duration;

use esp_idf_svc::hal::task::block_on;
use esp32_nimble::enums::PrimPhy;
use esp32_nimble::{BLEDevice, BLEScan};

use super::Error;
use crate::config::{BLE_SCAN_INTERVAL_MS, BLE_SCAN_WINDOW_MS};

/// The shortest and longest scans the stack can run, in milliseconds. See [`scan`].
const MIN_SCAN_MS: u128 = 10;
const MAX_SCAN_MS: u128 = 655_340;

/// A device that was heard during a scan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Advertiser {
    /// The Bluetooth address, for example `AA:BB:CC:DD:EE:FF`. Many phones and trackers change it
    /// every few minutes (private addresses).
    pub address: String,
    /// The local name, if the device sent one.
    pub name: Option<String>,
    /// The signal strength in dBm.
    pub rssi_dbm: i8,
    /// Whether the device advertised on the LE Coded PHY.
    pub coded_phy: bool,
}

/// Listens for `duration` (held between 10 ms and about 655 s, the range the stack can count) and
/// returns each device once, strongest signal first. It blocks the calling thread, so do not call
/// it from the screen thread.
///
/// # Errors
///
/// Fails if the Bluetooth stack cannot start the scan.
pub fn scan(duration: Duration) -> Result<Vec<Advertiser>, Error> {
    let device = BLEDevice::take();
    let mut scan = BLEScan::new();
    // Every report is wanted: a name often arrives in a scan response, after the first report of
    // the same device, so the controller must not filter repeats. Repeats are merged below.
    scan.active_scan(true)
        .filter_duplicates(false)
        .interval(BLE_SCAN_INTERVAL_MS)
        .window(BLE_SCAN_WINDOW_MS);

    let mut found: Vec<Advertiser> = Vec::new();
    // The stack counts in units of 10 ms in a `u16`, and 0 means "never stop": round up to a whole
    // unit and stay below the largest one.
    let milliseconds =
        i32::try_from(duration.as_millis().clamp(MIN_SCAN_MS, MAX_SCAN_MS)).unwrap_or(i32::MAX);
    block_on(scan.start(device, milliseconds, |seen, data| {
        let address = seen.addr().to_string();
        let name = data.name().map(ToString::to_string);
        match found.iter_mut().find(|known| known.address == address) {
            Some(known) => {
                known.name = known.name.take().or(name);
                known.rssi_dbm = known.rssi_dbm.max(seen.rssi());
                // The same address can advertise on both PHYs: one Coded report is enough.
                known.coded_phy |= matches!(seen.prim_phy(), PrimPhy::Coded);
            }
            None => found.push(Advertiser {
                address,
                name,
                rssi_dbm: seen.rssi(),
                coded_phy: matches!(seen.prim_phy(), PrimPhy::Coded),
            }),
        }
        // Not a result: keep scanning until the time is up.
        None::<()>
    }))?;

    found.sort_by_key(|advertiser| std::cmp::Reverse(advertiser.rssi_dbm));
    Ok(found)
}
