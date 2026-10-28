//! The GATT services the board offers: the standard Device Information service, and the Pulsar
//! service with the Wi-Fi status and the weather text.
//!
//! The Pulsar characteristics need an **authenticated** link to be read and to be subscribed to
//! (a pairing with the passkey on the screen). Protecting the read alone would not be enough:
//! a notification does not check the read permission, so the subscription has to be protected too.

use std::fmt::Write as _;
use std::sync::Arc;

use esp32_nimble::utilities::{BleUuid, mutex::Mutex};
use esp32_nimble::{BLECharacteristic, BLEServer, DescriptorProperties, NimbleProperties, uuid128};

use crate::config::{BLE_MANUFACTURER, BLE_MODEL};
use crate::status::{Band, LinkState, WifiStatus};

/// Device Information service (Bluetooth SIG assigned number).
const DEVICE_INFORMATION: BleUuid = BleUuid::from_uuid16(0x180A);
const MANUFACTURER_NAME: BleUuid = BleUuid::from_uuid16(0x2A29);
const MODEL_NUMBER: BleUuid = BleUuid::from_uuid16(0x2A24);
const FIRMWARE_REVISION: BleUuid = BleUuid::from_uuid16(0x2A26);
/// Characteristic Presentation Format descriptor (assigned number).
const PRESENTATION_FORMAT: BleUuid = BleUuid::from_uuid16(0x2904);

/// The Pulsar service. The UUIDs are random 128-bit numbers made for this project.
const PULSAR_SERVICE: BleUuid = uuid128!("5d1a0001-7e3f-4c53-9a6e-9f1c2b8d4a10");
const WIFI_STATUS: BleUuid = uuid128!("5d1a0002-7e3f-4c53-9a6e-9f1c2b8d4a10");
const WEATHER_TEXT: BleUuid = uuid128!("5d1a0003-7e3f-4c53-9a6e-9f1c2b8d4a10");

/// The longest value of an attribute, in bytes (a limit of the protocol).
const MAX_VALUE_LEN: usize = 512;

/// Presentation format: UTF-8 string, exponent 0, unit "unitless" (0x2700), Bluetooth SIG
/// namespace, no description. It makes apps such as nRF Connect show the value as text.
const UTF8_FORMAT: [u8; 7] = [0x19, 0x00, 0x00, 0x27, 0x01, 0x00, 0x00];

type Characteristic = Arc<Mutex<BLECharacteristic>>;

/// The Pulsar service, whose values the Bluetooth thread keeps up to date.
pub struct PulsarService {
    wifi: Characteristic,
    weather: Characteristic,
}

impl PulsarService {
    /// Adds the Device Information service and the Pulsar service to `server`. Do this before the
    /// first advertising set is configured: configuring it starts the server, and a service added
    /// after that is not visible.
    pub fn install(server: &mut BLEServer) -> Self {
        let information = server.create_service(DEVICE_INFORMATION);
        for (uuid, value) in [
            (MANUFACTURER_NAME, BLE_MANUFACTURER),
            (MODEL_NUMBER, BLE_MODEL),
            (FIRMWARE_REVISION, env!("CARGO_PKG_VERSION")),
        ] {
            information
                .lock()
                .create_characteristic(uuid, NimbleProperties::READ)
                .lock()
                .set_value(value.as_bytes());
        }

        let pulsar = server.create_service(PULSAR_SERVICE);
        let text_characteristic = |uuid| {
            let characteristic = pulsar.lock().create_characteristic(
                uuid,
                NimbleProperties::READ
                    | NimbleProperties::READ_AUTHEN
                    | NimbleProperties::NOTIFY
                    | NimbleProperties::NOTIFY_INDICATE_AUTHEN,
            );
            characteristic
                .lock()
                .create_descriptor(PRESENTATION_FORMAT, DescriptorProperties::READ)
                .lock()
                .set_value(&UTF8_FORMAT);
            characteristic
        };
        Self {
            wifi: text_characteristic(WIFI_STATUS),
            weather: text_characteristic(WEATHER_TEXT),
        }
    }

    /// Sets the Wi-Fi status text and notifies the phones that subscribed.
    pub fn publish_wifi(&self, text: &str) {
        publish(&self.wifi, text);
    }

    /// Sets the weather text and notifies the phones that subscribed.
    pub fn publish_weather(&self, text: &str) {
        publish(&self.weather, text);
    }
}

fn publish(characteristic: &Characteristic, text: &str) {
    characteristic
        .lock()
        .set_value(clip(text).as_bytes())
        .notify();
}

/// `text` cut to [`MAX_VALUE_LEN`] bytes, at a character boundary.
fn clip(text: &str) -> &str {
    let mut end = text.len().min(MAX_VALUE_LEN);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// The Wi-Fi status as one line, for example `connected 5G -61dBm 192.168.1.20`. The network name
/// is left out on purpose: the value is for the phone's owner, but there is no need to send more
/// than the status needs.
#[must_use]
pub fn wifi_text(status: &WifiStatus) -> String {
    let mut text = String::from(match status.state {
        LinkState::Idle => "off",
        LinkState::NoCredentials => "no credentials",
        LinkState::Connecting => "connecting",
        LinkState::Connected => "connected",
        LinkState::Failed => "failed",
    });
    if let Some(band) = status.band {
        text.push_str(match band {
            Band::Ghz2_4 => " 2.4G",
            Band::Ghz5 => " 5G",
        });
    }
    // Writing to a `String` cannot fail.
    if let Some(rssi) = status.rssi_dbm {
        let _ = write!(text, " {rssi}dBm");
    }
    if let Some(ip) = status.ip {
        let _ = write!(text, " {ip}");
    }
    text
}
