//! Bluetooth Low Energy: the board is discoverable, phones can pair with a passkey shown on the
//! screen, and a paired phone can read the Wi-Fi status and the weather text.
//!
//! ```ignore
//! bluetooth::start(secrets, wifi_status.clone(), message.clone(), bluetooth_status.clone())?;
//! ```
//!
//! The stack is NimBLE (through the `esp32-nimble` crate). `docs/bluetooth.md` lists the
//! Bluetooth features the chip has and which of them are turned on. In short:
//!
//! - **Security:** LE Secure Connections only, with a random six-digit passkey on the screen
//!   (Security Mode 1 Level 4 once a phone has paired with the passkey). The bonds are kept in the encrypted secret store (`bonds.rs`).
//! - **Radio:** 2M and Coded PHY, 251-byte packets (Data Length Extension), MTU 512.
//! - **Advertising:** two sets, one every phone sees and one on the LE Coded PHY, see
//!   [`advertising`].
//! - **Roles:** peripheral and broadcaster, plus the observer role in [`scan`].
//!
//! [`start`] returns once the stack runs. Callbacks from the stack publish the link state on a
//! [`SharedBluetooth`], which the screen draws as an icon; a thread keeps the values of the
//! services up to date and saves changed bonds.
//!
//! **Advertising starts late on purpose.** The thread waits until Wi-Fi's first connection
//! attempt is over ([`BLE_WIFI_WAIT`](BLE_WIFI_WAIT) at most), because with an
//! advertising set on, Wi-Fi's scan takes about twice as long. Until then the status is
//! [`BtState::Off`] and the icon is not shown.

pub mod advertising;
mod bonds;
pub mod scan;
mod services;

use std::io;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::Instant;

use esp_idf_svc::sys::EspError;
use esp32_nimble::enums::{AuthReq, PairKeyDist, SecurityIOCap};
use esp32_nimble::{BLEConnDesc, BLEDevice, BLEError};

use crate::config::{
    BLE_BOOT_SCAN, BLE_POLL_PERIOD, BLE_PREFERRED_MTU, BLE_STACK_BYTES, BLE_WIFI_POLL,
    BLE_WIFI_WAIT, HOSTNAME,
};
use crate::message::SharedMessage;
use crate::platform;
use crate::secrets::SecretStore;
use crate::status::{BtState, LinkState, SharedBluetooth, SharedStatus};
use bonds::BondStore;
use services::PulsarService;

/// Passkeys are six digits: 0 to 999 999.
const PASSKEY_LIMIT: u32 = 1_000_000;

/// What can go wrong with Bluetooth.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The Bluetooth stack refused a setting.
    #[error("Bluetooth stack: {0}")]
    Stack(BLEError),
    /// An ESP-IDF call failed.
    #[error("Bluetooth driver: {0}")]
    Driver(#[from] EspError),
    /// The Bluetooth thread could not be created.
    #[error("cannot start the Bluetooth thread: {0}")]
    Thread(#[from] io::Error),
}

impl From<BLEError> for Error {
    fn from(error: BLEError) -> Self {
        Self::Stack(error)
    }
}

/// Starts Bluetooth and returns at once.
///
/// `secrets` is where the bonds are saved. Without it Bluetooth works, but bonds are lost at
/// reboot. `wifi` and `message` are the values offered to a paired phone. The link state is
/// published on `status`.
///
/// # Errors
///
/// Fails if the stack cannot be set up or the thread cannot be created. A phone that cannot pair
/// is **not** an error here.
pub fn start(
    secrets: Option<SecretStore>,
    wifi: SharedStatus,
    message: SharedMessage,
    status: SharedBluetooth,
) -> Result<(), Error> {
    // This brings up the controller and the host, and returns when they are ready.
    let device = BLEDevice::take();
    BLEDevice::set_device_name(HOSTNAME)?;
    if let Ok(address) = device.get_addr() {
        log::info!("Bluetooth address: {address}");
    }

    // Bonds go in before anything can connect, and the change tracking starts after they are in.
    let bonds = BondStore::new(secrets);
    bonds.restore();
    platform::ble_bonds_watch();

    // Display only: the board shows a passkey and the phone types it. With Secure Connections
    // that is an authenticated pairing with a 128-bit key. Both sides give their encryption key
    // and identity key (the phone's identity key lets the board recognise its private addresses).
    device
        .security()
        .set_auth(AuthReq::Bond | AuthReq::Mitm | AuthReq::Sc)
        .set_io_cap(SecurityIOCap::DisplayOnly)
        .set_security_init_key(PairKeyDist::ENC | PairKeyDist::ID)
        .set_security_resp_key(PairKeyDist::ENC | PairKeyDist::ID)
        .resolve_rpa();
    platform::ble_allow_all_phys()?;
    platform::ble_use_long_packets()?;
    device.set_preferred_mtu(BLE_PREFERRED_MTU)?;

    let link = Arc::new(Link {
        status: status.clone(),
        connections: Mutex::new(Vec::new()),
    });
    let server = device.get_server();
    {
        let link = Arc::clone(&link);
        server.on_connect(move |_server, connection| link.connected(connection));
    }
    {
        let link = Arc::clone(&link);
        server.on_authentication_complete(move |_server, connection, result| {
            link.authenticated(connection, result.is_ok());
        });
    }
    {
        let link = Arc::clone(&link);
        server.on_disconnect(move |connection, _reason| link.disconnected(connection));
    }
    {
        let status = status.clone();
        server.on_passkey_request(move || {
            let passkey = random_passkey();
            status.update(|status| status.passkey = Some(passkey));
            passkey
        });
    }

    // The services first: configuring the first advertising set starts the server.
    let pulsar = PulsarService::install(server);
    log::info!("Bluetooth stack ready");

    thread::Builder::new()
        .name("ble".into())
        .stack_size(BLE_STACK_BYTES)
        .spawn(move || run(&pulsar, &bonds, &wifi, &message, &status))?;
    Ok(())
}

/// Keeps the link state up to date from the stack's callbacks. They run on the stack's own task,
/// so they stay short: they update a list, publish and, after a disconnect, restart advertising.
struct Link {
    status: SharedBluetooth,
    /// The phones connected right now.
    connections: Mutex<Vec<Connection>>,
}

/// One connected phone.
struct Connection {
    handle: u16,
    /// Whether the link is encrypted with a bonded, authenticated key (a passkey was typed).
    secured: bool,
}

/// Whether the link is encrypted and bonded with an authenticated key. A phone without a keyboard
/// or screen can bond with "Just Works", which is encrypted but not authenticated: it is not
/// counted, and it cannot read the Pulsar values either.
fn is_secured(connection: &BLEConnDesc) -> bool {
    connection.encrypted() && connection.bonded() && connection.authenticated()
}

impl Link {
    fn connected(&self, connection: &BLEConnDesc) {
        self.update(|connections| {
            connections.push(Connection {
                handle: connection.conn_handle(),
                secured: is_secured(connection),
            });
        });
    }

    fn authenticated(&self, connection: &BLEConnDesc, success: bool) {
        // Whether it worked or not, the pairing is over and the passkey is of no use any more.
        // (Pairing is one phone at a time: the stack does not say whose passkey this is.)
        self.status.update(|status| status.passkey = None);
        let handle = connection.conn_handle();
        let secured = success && is_secured(connection);
        self.update(|connections| {
            if let Some(known) = connections.iter_mut().find(|known| known.handle == handle) {
                known.secured = secured;
            }
        });
    }

    fn disconnected(&self, connection: &BLEConnDesc) {
        let handle = connection.conn_handle();
        self.status.update(|status| status.passkey = None);
        self.update(|connections| connections.retain(|known| known.handle != handle));
        // The set the phone connected to has ended: bring it back so the next phone can connect.
        advertising::restart();
    }

    /// Changes the list of connections and publishes what the screen shows for the result: the
    /// board is "secured" when every phone connected is.
    fn update(&self, change: impl FnOnce(&mut Vec<Connection>)) {
        let mut connections = self
            .connections
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        change(&mut connections);
        let state = if connections.is_empty() {
            BtState::Waiting {
                bonded: has_bonds(),
            }
        } else {
            BtState::Connected {
                secured: connections.iter().all(|known| known.secured),
            }
        };
        self.status.update(|status| status.state = state);
    }
}

/// Whether at least one phone is bonded.
fn has_bonds() -> bool {
    BLEDevice::take()
        .bonded_addresses()
        .is_ok_and(|bonded| !bonded.is_empty())
}

/// A random passkey from 000000 to 999999, without the bias a plain remainder would have.
fn random_passkey() -> u32 {
    // The largest multiple of `PASSKEY_LIMIT` in 32 bits: numbers above it are drawn again.
    const LIMIT: u32 = u32::MAX / PASSKEY_LIMIT * PASSKEY_LIMIT;
    loop {
        let number = platform::random_u32();
        if number < LIMIT {
            return number % PASSKEY_LIMIT;
        }
    }
}

/// Waits until Wi-Fi is no longer busy with its first connection attempt, for at most
/// [`BLE_WIFI_WAIT`]. See the module documentation for why.
fn wait_for_wifi(wifi: &SharedStatus) {
    let deadline = Instant::now() + BLE_WIFI_WAIT;
    while wifi.snapshot().state == LinkState::Connecting && Instant::now() < deadline {
        thread::sleep(BLE_WIFI_POLL);
    }
}

/// The body of the Bluetooth thread.
fn run(
    pulsar: &PulsarService,
    bonds: &BondStore,
    wifi: &SharedStatus,
    message: &SharedMessage,
    status: &SharedBluetooth,
) {
    wait_for_wifi(wifi);
    if let Err(error) = advertise(status) {
        log::error!("Bluetooth advertising not started: {error}");
        status.update(|status| status.state = BtState::Failed);
        return;
    }
    scan_once();

    let mut published = Published::default();
    let mut bonds_unsaved = false;
    loop {
        save_changed_bonds(bonds, status, &mut bonds_unsaved);
        published.refresh(pulsar, wifi, message);
        thread::sleep(BLE_POLL_PERIOD);
    }
}

/// Starts advertising and tells the screen that the board can be found now.
fn advertise(status: &SharedBluetooth) -> Result<(), BLEError> {
    advertising::start()?;
    let bonded = has_bonds();
    // A phone can connect before this point: its state stays.
    status.update(|status| {
        if status.state == BtState::Off {
            status.state = BtState::Waiting { bonded };
        }
    });
    log::info!("Bluetooth advertising (bonded phones known: {bonded})");
    Ok(())
}

/// One scan to show in the log that the Observer works, unless [`BLE_BOOT_SCAN`] is zero.
fn scan_once() {
    if BLE_BOOT_SCAN.is_zero() {
        return;
    }
    match scan::scan(BLE_BOOT_SCAN) {
        Ok(found) => log::info!(
            "scan: {} devices nearby ({} on LE Coded PHY)",
            found.len(),
            found.iter().filter(|device| device.coded_phy).count()
        ),
        Err(error) => log::warn!("scan failed: {error}"),
    }
}

/// Saves the bonds if NimBLE changed them since they were last saved. `unsaved` stays set when
/// saving fails, so the next call tries again.
fn save_changed_bonds(bonds: &BondStore, status: &SharedBluetooth, unsaved: &mut bool) {
    *unsaved |= platform::ble_bonds_take_changed();
    if !*unsaved {
        return;
    }
    match bonds.save() {
        Ok(()) => {
            *unsaved = false;
            log::info!("Bluetooth bonds saved");
        }
        Err(error) => log::error!("cannot save the Bluetooth bonds, will retry: {error}"),
    }
    // Bonds can also change while nobody is connected, so the icon follows.
    let bonded = has_bonds();
    status.update(|status| {
        if matches!(status.state, BtState::Waiting { .. }) {
            status.state = BtState::Waiting { bonded };
        }
    });
}

/// What the phones were last told, so that only changes are sent.
#[derive(Default)]
struct Published {
    wifi_text: String,
    /// The generation of the weather text, see [`SharedMessage::take_update`].
    weather_seen: u64,
}

impl Published {
    /// Sends the Wi-Fi status and the weather text to the services if they changed.
    fn refresh(&mut self, pulsar: &PulsarService, wifi: &SharedStatus, message: &SharedMessage) {
        let text = services::wifi_text(&wifi.snapshot());
        if text != self.wifi_text {
            pulsar.publish_wifi(&text);
            self.wifi_text = text;
        }
        if let Some(text) = message.take_update(&mut self.weather_seen) {
            pulsar.publish_weather(&text);
        }
    }
}
