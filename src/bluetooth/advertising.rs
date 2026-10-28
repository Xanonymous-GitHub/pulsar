//! The two advertising sets.
//!
//! - **Set 0** is a legacy advertisement (connectable and scannable) on the 1M PHY. Every phone
//!   with Bluetooth 4.0 or later can see it.
//! - **Set 1** is an extended advertisement on the LE Coded PHY (Bluetooth 5). It reaches about
//!   four times as far, but only phones with Coded PHY support see it. The board is fully usable
//!   without it, so if it cannot be set up the problem is logged and set 0 goes on alone.
//!
//! Both sets carry the Flags field (LE General Discoverable, no Classic Bluetooth). Without it,
//! the Bluetooth page in a phone's Settings does not list the device, though a BLE scanner app
//! does. The field comes from the patched `esp32-nimble` in `vendor/`.

use std::sync::atomic::{AtomicBool, Ordering};

use esp32_nimble::enums::{PrimPhy, SecPhy};
use esp32_nimble::{BLEDevice, BLEError, BLEExtAdvertisement, BLEExtAdvertising};

use crate::config::{BLE_ADV_INTERVAL_MS, HOSTNAME};

/// Flags field: LE General Discoverable Mode (bit 1) and BR/EDR Not Supported (bit 2).
const FLAGS_GENERAL_DISCOVERABLE: u8 = 0b0000_0110;

/// Instance number of the legacy set.
const LEGACY: u8 = 0;
/// Instance number of the LE Coded set.
const LONG_RANGE: u8 = 1;

/// Whether the LE Coded set was set up, so [`restart`] knows if it has to bring it back.
static LONG_RANGE_READY: AtomicBool = AtomicBool::new(false);

/// Converts milliseconds to the 0.625 ms units the controller uses.
const fn interval_units(milliseconds: u32) -> u32 {
    milliseconds * 8 / 5
}

/// A connectable set with the board's name, on the given PHYs.
fn connectable_set(primary: PrimPhy, secondary: SecPhy, legacy: bool) -> BLEExtAdvertisement {
    let mut set = BLEExtAdvertisement::new(primary, secondary);
    set.legacy_advertising(legacy);
    set.connectable(true);
    set.flags(FLAGS_GENERAL_DISCOVERABLE);
    set.min_interval(interval_units(BLE_ADV_INTERVAL_MS));
    set.max_interval(interval_units(BLE_ADV_INTERVAL_MS));
    set.name(HOSTNAME);
    set
}

/// Configures `set` as instance `instance` and starts it.
fn start_set(
    advertising: &mut BLEExtAdvertising,
    instance: u8,
    mut set: BLEExtAdvertisement,
) -> Result<(), BLEError> {
    advertising.set_instance_data(instance, &mut set)?;
    advertising.start(instance)
}

/// Sets up the sets and starts them. Call it after the services have been installed.
///
/// # Errors
///
/// Fails if the Bluetooth stack rejects the legacy set. A problem with the LE Coded set is only
/// logged.
pub fn start() -> Result<(), BLEError> {
    let mut advertising = BLEDevice::take().get_advertising().lock();

    let legacy = connectable_set(PrimPhy::Phy1M, SecPhy::Phy1M, true);
    start_set(&mut advertising, LEGACY, legacy)?;

    let long_range = connectable_set(PrimPhy::Coded, SecPhy::Coded, false);
    match start_set(&mut advertising, LONG_RANGE, long_range) {
        Ok(()) => LONG_RANGE_READY.store(true, Ordering::Release),
        Err(error) => log::warn!("LE Coded advertising not started: {error}"),
    }
    Ok(())
}

/// Starts the sets again. A connectable set ends when a phone connects to it, and with extended
/// advertising nothing starts it again by itself. A set that is still running is enabled again,
/// which NimBLE allows: it only restarts that set's duration and event counters.
pub fn restart() {
    let mut advertising = BLEDevice::take().get_advertising().lock();
    let long_range = LONG_RANGE_READY
        .load(Ordering::Acquire)
        .then_some(LONG_RANGE);
    for instance in std::iter::once(LEGACY).chain(long_range) {
        if let Err(error) = advertising.start(instance) {
            log::warn!("cannot restart advertising set {instance}: {error}");
        }
    }
}
