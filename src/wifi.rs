//! Join a WPA3 Wi-Fi network (2.4 GHz or 5 GHz, up to Wi-Fi 6) and get an IPv4 address by DHCP.
//!
//! ```ignore
//! let credentials = wifi::load_or_provision(&secrets)?;
//! wifi::start(board.modem, sysloop, &credentials, status.clone())?;
//! ```
//!
//! # Where the credentials come from
//!
//! [`load_or_provision`] reads two **build-time** environment variables of the shell that runs
//! `cargo`: `PULSAR_WIFI_SSID` and `PULSAR_WIFI_PASSWORD`. If both are set they are written to
//! the [`SecretStore`] (nothing is written when they are unchanged). If neither is set, the
//! credentials already in the store are used, so you can rebuild and reflash without the password
//! in your shell. Setting only one of them is an error. Empty values count as unset.
//!
//! The password is never logged. Keep it out of your shell history, for example with
//! `read -rs PULSAR_WIFI_PASSWORD; export PULSAR_WIFI_PASSWORD`.
//!
//! # Behaviour
//!
//! - **WPA3-Personal only.** The minimum security level is WPA3, so an access point offering
//!   WPA2 or weaker is refused and cannot talk the board into a downgrade. A router in
//!   WPA2/WPA3 transition mode works (the board joins with WPA3).
//! - **Both bands.** The board scans 2.4 GHz and 5 GHz and joins the strongest access point
//!   that has the name; Wi-Fi 6 (802.11ax) is allowed on both bands.
//! - **Persistent retries.** After a failed attempt or a lost connection it retries forever with
//!   a doubling delay (see [`config`](crate::config)). After a few failures in a row the status
//!   shows [`LinkState::Failed`], but retrying goes on, so the board reconnects when the router
//!   comes back.
//! - **RAM only.** The Wi-Fi driver is started without an NVS partition, so the password is
//!   never copied into the plain `nvs` partition by ESP-IDF.
//!
//! # What this does not protect
//!
//! The Wi-Fi driver keeps its own copy of the password in RAM while it runs, and a build made
//! with the environment variables contains the password in the firmware image (and in `target/`
//! on your computer).

use std::io;
use std::net::Ipv4Addr;
use std::thread;
use std::time::Duration;

use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::modem::Modem;
use esp_idf_svc::ipv4::{
    ClientConfiguration as IpClientConfiguration, Configuration as IpConfiguration,
    DHCPClientSettings,
};
use esp_idf_svc::netif::{EspNetif, NetifConfiguration, NetifStack};
use esp_idf_svc::sys::EspError;
use esp_idf_svc::wifi::{
    AuthMethod, BlockingWifi, ClientConfiguration, Configuration, EspWifi, ScanMethod,
    ScanSortMethod, WifiDriver,
};
use zeroize::Zeroizing;

use crate::config::{
    HOSTNAME, WIFI_BACKOFF_MAX, WIFI_BACKOFF_START, WIFI_CONNECT_TIMEOUT, WIFI_FAILED_AFTER,
    WIFI_POLL_PERIOD, WIFI_PREFER_5GHZ_MARGIN_DB, WIFI_SECRET_KEY, WIFI_STACK_BYTES,
};
use crate::platform;
use crate::secrets::{self, SecretStore};
use crate::status::{Band, LinkState, MAX_SSID_LEN, SharedStatus};

/// Shortest password accepted (WPA3 itself allows less, but that is too weak to be sensible).
pub const MIN_PASSWORD_LEN: usize = 8;
/// Longest password the Wi-Fi driver accepts.
pub const MAX_PASSWORD_LEN: usize = 63;
/// Size of the driver's password buffer (the limit plus room for a terminator).
const PASSWORD_CAPACITY: usize = 64;

/// What can go wrong when setting up Wi-Fi.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Only one of `PULSAR_WIFI_SSID` and `PULSAR_WIFI_PASSWORD` was set at build time.
    #[error("set both PULSAR_WIFI_SSID and PULSAR_WIFI_PASSWORD at build time, or neither")]
    IncompleteBuildEnv,
    /// Nothing in the build environment and nothing in the secret store.
    #[error("no Wi-Fi credentials: build once with PULSAR_WIFI_SSID and PULSAR_WIFI_PASSWORD set")]
    NoCredentials,
    /// The SSID is empty or longer than 32 bytes, or the stored record is damaged.
    #[error("invalid SSID (1 to {MAX_SSID_LEN} bytes)")]
    InvalidSsid,
    /// The configured hostname is longer than 30 bytes.
    #[error("invalid hostname in config.rs (at most 30 bytes)")]
    InvalidHostname,
    /// The password length is outside the accepted range.
    #[error("invalid password ({MIN_PASSWORD_LEN} to {MAX_PASSWORD_LEN} bytes)")]
    InvalidPassword,
    /// The secret store failed.
    #[error("secret store: {0}")]
    Secrets(#[from] secrets::Error),
    /// The Wi-Fi driver failed.
    #[error("Wi-Fi driver: {0}")]
    Driver(#[from] EspError),
    /// The supervisor thread could not be created.
    #[error("cannot start the Wi-Fi thread: {0}")]
    Thread(#[from] io::Error),
}

/// The SSID and password of a WPA3 network. `Debug` never prints the password, and the password
/// is wiped from memory when this is dropped.
pub struct Credentials {
    ssid: heapless::String<MAX_SSID_LEN>,
    password: Zeroizing<heapless::String<PASSWORD_CAPACITY>>,
}

impl Credentials {
    /// Checks the lengths and copies both values.
    ///
    /// # Errors
    ///
    /// Fails if the SSID or the password has an unusable length.
    pub fn new(ssid: &str, password: &str) -> Result<Self, Error> {
        if ssid.is_empty() {
            return Err(Error::InvalidSsid);
        }
        if !(MIN_PASSWORD_LEN..=MAX_PASSWORD_LEN).contains(&password.len()) {
            return Err(Error::InvalidPassword);
        }
        Ok(Self {
            ssid: ssid.try_into().map_err(|_| Error::InvalidSsid)?,
            password: Zeroizing::new(password.try_into().map_err(|_| Error::InvalidPassword)?),
        })
    }

    /// The network name.
    #[must_use]
    pub fn ssid(&self) -> &str {
        &self.ssid
    }

    /// One byte for the SSID length, the SSID, then the password: the format kept in the store.
    fn encode(&self) -> Zeroizing<Vec<u8>> {
        let mut record = Zeroizing::new(Vec::with_capacity(
            1 + self.ssid.len() + self.password.len(),
        ));
        // An SSID is at most 32 bytes, so its length always fits in one byte.
        record.push(u8::try_from(self.ssid.len()).unwrap_or(u8::MAX));
        record.extend_from_slice(self.ssid.as_bytes());
        record.extend_from_slice(self.password.as_bytes());
        record
    }

    fn decode(record: &[u8]) -> Result<Self, Error> {
        let (&ssid_len, rest) = record.split_first().ok_or(Error::InvalidSsid)?;
        let (ssid, password) = rest
            .split_at_checked(usize::from(ssid_len))
            .ok_or(Error::InvalidSsid)?;
        let ssid = core::str::from_utf8(ssid).map_err(|_| Error::InvalidSsid)?;
        let password = core::str::from_utf8(password).map_err(|_| Error::InvalidPassword)?;
        Self::new(ssid, password)
    }
}

impl core::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Credentials")
            .field("ssid", &self.ssid)
            .field("password", &"<redacted>")
            .finish()
    }
}

/// Gets the credentials: from the build environment (and stores them), or else from `secrets`.
///
/// See the [module documentation](self) for the environment variables.
///
/// # Errors
///
/// Fails if there are no credentials, they are invalid, or the store fails.
pub fn load_or_provision(secrets: &SecretStore) -> Result<Credentials, Error> {
    // Empty values count as unset.
    let from_env = |value: Option<&'static str>| value.filter(|value| !value.is_empty());

    match (
        from_env(option_env!("PULSAR_WIFI_SSID")),
        from_env(option_env!("PULSAR_WIFI_PASSWORD")),
    ) {
        (Some(ssid), Some(password)) => {
            let credentials = Credentials::new(ssid, password)?;
            secrets.put(WIFI_SECRET_KEY, &credentials.encode())?;
            log::info!(
                "credentials for '{}' taken from the build environment and stored",
                credentials.ssid()
            );
            Ok(credentials)
        }
        (None, None) => {
            let record = secrets.get(WIFI_SECRET_KEY)?.ok_or(Error::NoCredentials)?;
            let credentials = Credentials::decode(&record)?;
            log::info!("using the stored credentials for '{}'", credentials.ssid());
            Ok(credentials)
        }
        _ => Err(Error::IncompleteBuildEnv),
    }
}

/// Starts Wi-Fi and returns at once. A thread keeps the connection alive and publishes progress
/// on `status`.
///
/// # Errors
///
/// Fails if the driver cannot be set up or the thread cannot be created. A wrong password or a
/// missing router is **not** an error here: it shows up as [`LinkState::Failed`] on `status`.
pub fn start(
    modem: Modem<'static>,
    sysloop: EspSystemEventLoop,
    credentials: &Credentials,
    status: SharedStatus,
) -> Result<(), Error> {
    // No NVS partition: the driver then keeps its configuration (and the password) in RAM only.
    let station = EspNetif::new_with_conf(&NetifConfiguration {
        ip_configuration: Some(IpConfiguration::Client(IpClientConfiguration::DHCP(
            DHCPClientSettings {
                hostname: Some(HOSTNAME.try_into().map_err(|_| Error::InvalidHostname)?),
            },
        ))),
        ..NetifConfiguration::wifi_default_client()
    })?;
    let driver = EspWifi::wrap_all(
        WifiDriver::new(modem, sysloop.clone(), None)?,
        station,
        EspNetif::new(NetifStack::Ap)?,
    )?;
    let mut wifi = BlockingWifi::wrap(driver, sysloop)?;

    wifi.set_configuration(&Configuration::Client(ClientConfiguration {
        ssid: credentials.ssid.clone(),
        password: (*credentials.password).clone(),
        // The minimum security level: weaker access points are not considered at all.
        auth_method: AuthMethod::WPA3Personal,
        // Look on every channel of both bands and pick the strongest access point (5 GHz gets a
        // head start, see `prefer_5ghz` below).
        scan_method: ScanMethod::CompleteScan(ScanSortMethod::Signal),
        ..ClientConfiguration::default()
    }))?;
    platform::prefer_5ghz(WIFI_PREFER_5GHZ_MARGIN_DB)?;
    wifi.start()?;
    platform::enable_dual_band_wifi6()?;
    if let Ok(name) = wifi.wifi().sta_netif().get_hostname() {
        log::info!("hostname: {name}");
    }

    status.update(|status| {
        status.ssid = credentials.ssid.clone();
        status.state = LinkState::Connecting;
    });

    thread::Builder::new()
        .name("wifi".into())
        .stack_size(WIFI_STACK_BYTES)
        .spawn(move || supervise(&mut wifi, &status))?;
    Ok(())
}

/// Keeps the connection alive forever.
fn supervise(wifi: &mut BlockingWifi<EspWifi<'static>>, status: &SharedStatus) {
    let mut failures: u32 = 0;
    loop {
        let state = if failures >= WIFI_FAILED_AFTER {
            LinkState::Failed
        } else {
            LinkState::Connecting
        };
        status.update(|status| {
            status.state = state;
            status.rssi_dbm = None;
            status.band = None;
            status.ip = None;
        });

        match join(wifi) {
            Ok(()) => {
                failures = 0;
                monitor(wifi, status);
                log::warn!("connection lost");
            }
            Err(error) => {
                failures = failures.saturating_add(1);
                log::warn!("connect failed (attempt {failures}): {error}");
            }
        }

        // Reset the driver's state before the next attempt.
        if let Err(error) = wifi.disconnect() {
            log::debug!("disconnect: {error}");
        }
        thread::sleep(backoff(failures));
    }
}

/// Connects to the access point and waits for an IP address.
///
/// `BlockingWifi::connect` and `wait_netif_up` give up after a fixed 15 seconds each. That is not
/// enough here: the scan of both bands takes about 11 seconds on its own, and Bluetooth shares the
/// 2.4 GHz radio and slows it further. So the board waits for the events itself, with the
/// budget in [`WIFI_CONNECT_TIMEOUT`].
fn join(wifi: &mut BlockingWifi<EspWifi<'static>>) -> Result<(), EspError> {
    wifi.wifi_mut().connect()?;
    wifi.wifi_wait_while(
        || wifi.wifi().is_connected().map(|connected| !connected),
        Some(WIFI_CONNECT_TIMEOUT),
    )?;
    wifi.ip_wait_while(
        || wifi.wifi().is_up().map(|up| !up),
        Some(WIFI_CONNECT_TIMEOUT),
    )
}

/// Publishes signal, band and address every [`WIFI_POLL_PERIOD`] until the connection is lost.
fn monitor(wifi: &BlockingWifi<EspWifi<'static>>, status: &SharedStatus) {
    loop {
        // `get_ap_info` fails as soon as the station is no longer associated.
        let Ok(info) = wifi.wifi().get_ap_info() else {
            return;
        };
        let ip = wifi
            .wifi()
            .sta_netif()
            .get_ip_info()
            .ok()
            .map(|info| info.ip)
            .filter(|ip| *ip != Ipv4Addr::UNSPECIFIED);

        status.update(|status| {
            status.state = LinkState::Connected;
            status.rssi_dbm = Some(i32::from(info.signal_strength));
            status.band = Some(Band::from_channel(info.channel));
            status.ip = ip;
        });
        thread::sleep(WIFI_POLL_PERIOD);
    }
}

/// The wait before retry number `failures`: doubles every time, up to [`WIFI_BACKOFF_MAX`].
fn backoff(failures: u32) -> Duration {
    let doublings = failures.saturating_sub(1).min(5);
    WIFI_BACKOFF_START
        .saturating_mul(1 << doublings)
        .min(WIFI_BACKOFF_MAX)
}
