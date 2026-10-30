//! Entry point: initialise the platform, then start every subsystem.

use anyhow::Result;
use esp_idf_svc::eventloop::EspSystemEventLoop;
use pulsar::{
    bluetooth,
    board::Board,
    clock, config, led,
    message::SharedMessage,
    mqtt,
    secrets::SecretStore,
    status::{BtState, LinkState, SharedBluetooth, SharedStatus},
    telemetry,
    ui::Screen,
    weather, wifi,
};

/// Opens the secret store. It may be unavailable (for example while flash encryption is off).
/// That must not stop the rest of the firmware, so the problem is logged and `None` returned.
fn open_secrets() -> Option<SecretStore> {
    match SecretStore::open() {
        Ok(store) => {
            #[cfg(feature = "insecure-dev-store")]
            if let Err(error) = store.self_test() {
                log::error!("self-test failed: {error}");
            }
            Some(store)
        }
        Err(error) => {
            log::error!("secrets unavailable: {error}");
            None
        }
    }
}

/// Loads the MQTT broker settings. MQTT is optional: no settings, or settings that cannot be read,
/// leave the rest of the firmware running.
fn load_mqtt_settings(secrets: Option<&SecretStore>) -> Option<mqtt::Settings> {
    match mqtt::Settings::load_or_provision(secrets?) {
        Ok(None) => {
            log::info!("MQTT is not configured (set PULSAR_MQTT_URL at build time)");
            None
        }
        Ok(settings) => settings,
        Err(error) => {
            log::error!("MQTT settings unavailable: {error}");
            None
        }
    }
}

fn main() -> Result<()> {
    // Required once so the linker keeps the ESP-IDF runtime patches.
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    let status = SharedStatus::default();
    let bluetooth_status = SharedBluetooth::default();
    let message = SharedMessage::new(config::SCROLL_PLACEHOLDER);
    let board = Board::take()?;
    let sysloop = EspSystemEventLoop::take()?;

    led::spawn(board.led)?;
    Screen::new(
        board.display_i2c,
        status.clone(),
        bluetooth_status.clone(),
        message.clone(),
    )?
    .spawn()?;

    // Wi-Fi needs its credentials from the store. If they are missing, the rest of the firmware
    // keeps running and the screen says why.
    let secrets = open_secrets();
    let mqtt_settings = load_mqtt_settings(secrets.as_ref());
    let started = secrets
        .as_ref()
        .ok_or(wifi::Error::NoCredentials)
        .and_then(wifi::load_or_provision)
        .and_then(|credentials| wifi::start(board.modem, sysloop, &credentials, status.clone()));
    match started {
        // The network stack exists now, so SNTP can start. It waits for the connection by itself.
        // The weather thread waits for an address, then fetches the forecast over HTTPS.
        Ok(()) => {
            if let Err(error) = clock::start() {
                log::error!("clock not started: {error}");
            }
            if let Err(error) = weather::spawn(status.clone(), message.clone()) {
                log::error!("weather not started: {error}");
            }
            // The telemetry thread waits for an address, then connects to the MQTT broker.
            if let Some(settings) = mqtt_settings
                && let Err(error) = telemetry::spawn(settings, status.clone(), message.clone())
            {
                log::error!("MQTT not started: {error}");
            }
        }
        Err(error) => {
            log::error!("Wi-Fi not started: {error}");
            status.update(|status| status.state = LinkState::NoCredentials);
        }
    }

    // Bluetooth does not depend on Wi-Fi. It keeps the bonds of paired phones in the secret store
    // (which it now owns), and offers the Wi-Fi status and the weather text to them.
    if let Err(error) = bluetooth::start(
        secrets,
        status.clone(),
        message.clone(),
        bluetooth_status.clone(),
    ) {
        log::error!("Bluetooth not started: {error}");
        bluetooth_status.update(|status| status.state = BtState::Failed);
    }

    log::info!("pulsar started");
    Ok(())
}
