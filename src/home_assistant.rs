//! The MQTT discovery message that makes [Home Assistant](https://www.home-assistant.io) list the
//! board as a device with its sensors, a restart button and a text box for the screen.
//!
//! It is one message in the "device" format (Home Assistant 2024.11 or newer), published retained
//! on [`config_topic`], so Home Assistant finds it whenever it starts. Nothing here talks to the
//! network: [`config`] only builds the JSON, [`telemetry`](crate::telemetry) publishes it.
//!
//! The sensors read their values from fields of the JSON state message, see
//! [`telemetry`](crate::telemetry). Each sensor names the state topic itself: a state topic at the
//! device level would also be inherited by the text box, which would then take the whole JSON as
//! its text. The device is marked unavailable while the availability topic does not say `online`
//! (the Last Will sets it to `offline`).

use serde_json::{Map, Value, json};

use crate::config::{BLE_MANUFACTURER, BLE_MODEL, HA_DISCOVERY_PREFIX, NOTICE_MAX_LEN};

/// Where to find the board's topics.
#[derive(Clone, Copy, Debug)]
pub struct Device<'a> {
    /// The short id of the board (from its MAC address), unique on the broker.
    pub id: &'a str,
    /// Says `online` or `offline`.
    pub availability_topic: &'a str,
    /// The JSON state message.
    pub state_topic: &'a str,
    /// Any message here restarts the board.
    pub restart_topic: &'a str,
    /// A message here shows its text on the screen for a while.
    pub notice_topic: &'a str,
}

/// A sensor, read from one field of the state message.
struct Sensor {
    /// The field in the state message, and the end of the unique id.
    key: &'static str,
    name: &'static str,
    device_class: Option<&'static str>,
    unit: Option<&'static str>,
    icon: Option<&'static str>,
    /// A number that changes over time (Home Assistant keeps statistics for it).
    measurement: bool,
    /// Shown under "Diagnostic" instead of with the main readings.
    diagnostic: bool,
}

impl Sensor {
    const fn new(key: &'static str, name: &'static str) -> Self {
        Self {
            key,
            name,
            device_class: None,
            unit: None,
            icon: None,
            measurement: false,
            diagnostic: true,
        }
    }

    const fn class(mut self, device_class: &'static str, unit: &'static str) -> Self {
        self.device_class = Some(device_class);
        self.unit = Some(unit);
        self
    }

    const fn icon(mut self, icon: &'static str) -> Self {
        self.icon = Some(icon);
        self
    }

    const fn measurement(mut self) -> Self {
        self.measurement = true;
        self
    }

    const fn main(mut self) -> Self {
        self.diagnostic = false;
        self
    }
}

const SENSORS: [Sensor; 7] = [
    Sensor::new("rssi", "Signal strength")
        .class("signal_strength", "dBm")
        .measurement(),
    Sensor::new("band", "Wi-Fi band").icon("mdi:wifi"),
    Sensor::new("ssid", "Wi-Fi network").icon("mdi:wifi-marker"),
    Sensor::new("ip", "IP address").icon("mdi:ip-network"),
    Sensor::new("uptime_s", "Uptime").class("duration", "s"),
    Sensor::new("free_heap", "Free memory")
        .class("data_size", "B")
        .measurement(),
    Sensor::new("weather", "Weather")
        .icon("mdi:weather-partly-cloudy")
        .main(),
];

/// The topic the discovery message goes to.
#[must_use]
pub fn config_topic(id: &str) -> String {
    format!("{HA_DISCOVERY_PREFIX}/device/pulsar-{id}/config")
}

/// The discovery message for `device`.
#[must_use]
pub fn config(device: &Device<'_>) -> Value {
    let unique = format!("pulsar_{}", device.id);
    let version = env!("CARGO_PKG_VERSION");

    let mut components = Map::new();
    for sensor in &SENSORS {
        let mut component = json!({
            "p": "sensor",
            "name": sensor.name,
            "uniq_id": format!("{unique}_{}", sensor.key),
            "stat_t": device.state_topic,
            "val_tpl": format!("{{{{ value_json.{} }}}}", sensor.key),
        });
        let optional = [
            ("dev_cla", sensor.device_class),
            ("unit_of_meas", sensor.unit),
            ("ic", sensor.icon),
            ("stat_cla", sensor.measurement.then_some("measurement")),
            ("ent_cat", sensor.diagnostic.then_some("diagnostic")),
        ];
        if let Some(fields) = component.as_object_mut() {
            fields.extend(
                optional
                    .into_iter()
                    .filter_map(|(field, value)| Some((field.to_owned(), Value::from(value?)))),
            );
        }
        components.insert(sensor.key.to_owned(), component);
    }
    components.insert(
        "restart".to_owned(),
        json!({
            "p": "button",
            "name": "Restart",
            "uniq_id": format!("{unique}_restart"),
            "dev_cla": "restart",
            "ent_cat": "config",
            "cmd_t": device.restart_topic,
        }),
    );
    components.insert(
        "notice".to_owned(),
        json!({
            "p": "text",
            "name": "Notice",
            "uniq_id": format!("{unique}_notice"),
            "icon": "mdi:message-text",
            "max": NOTICE_MAX_LEN,
            "cmd_t": device.notice_topic,
        }),
    );

    json!({
        "dev": {
            "ids": [unique],
            "name": format!("Pulsar {}", device.id),
            "mf": BLE_MANUFACTURER,
            "mdl": BLE_MODEL,
            "sw": version,
        },
        "o": { "name": "pulsar", "sw": version },
        "avty_t": device.availability_topic,
        "qos": 1,
        "cmps": components,
    })
}
