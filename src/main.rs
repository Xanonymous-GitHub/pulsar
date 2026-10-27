//! Entry point: initialise the platform, then start every subsystem.

use anyhow::Result;
use esp_idf_svc::eventloop::EspSystemEventLoop;
use pulsar::{
    board::Board,
    clock, config, led,
    message::SharedMessage,
    secrets::SecretStore,
    status::{LinkState, SharedStatus},
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

fn main() -> Result<()> {
    // Required once so the linker keeps the ESP-IDF runtime patches.
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    let status = SharedStatus::default();
    let message = SharedMessage::new(config::SCROLL_PLACEHOLDER);
    let board = Board::take()?;
    let sysloop = EspSystemEventLoop::take()?;

    led::spawn(board.led)?;
    Screen::new(board.display_i2c, status.clone(), message.clone())?.spawn()?;

    // Wi-Fi needs its credentials from the store. If they are missing, the rest of the firmware
    // keeps running and the screen says why.
    let started = open_secrets()
        .ok_or(wifi::Error::NoCredentials)
        .and_then(|secrets| wifi::load_or_provision(&secrets))
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
        }
        Err(error) => {
            log::error!("Wi-Fi not started: {error}");
            status.update(|status| status.state = LinkState::NoCredentials);
        }
    }

    log::info!("pulsar started");
    Ok(())
}
