//! Join a WPA2 Wi-Fi network and get an IPv4 address (DHCP), with the password kept in the
//! [`secret_store`](crate::secret_store).
//!
//! # Using it from `main`
//!
//! ```ignore
//! let mut store = EncryptedFlashStore::open(FlashStorage::new(peripherals.FLASH))?;
//! let credentials = wifi::load_or_provision(&mut store)?;
//! let link = WifiLink::start(peripherals.WIFI, &credentials, spawner)?;
//! let ip = link.wait_for_ip().await;
//! ```
//!
//! `esp_rtos::start` must have run before [`WifiLink::start`], and the heap must exist.
//!
//! # Where the credentials come from
//!
//! [`load_or_provision`] reads two **build-time** environment variables from the shell that runs
//! `cargo`: `PULSAR_WIFI_SSID` and `PULSAR_WIFI_PASSWORD`. If both are set they are written to
//! the secret store (nothing is written when they are unchanged). If neither is set, the
//! credentials already in the store are used, so you can rebuild and reflash without the
//! password in your shell. Set only one of them and you get [`Error::IncompleteBuildEnv`].
//!
//! The password is never logged. Keep it out of shell history, for example with
//! `read -rs PULSAR_WIFI_PASSWORD; export PULSAR_WIFI_PASSWORD`.
//!
//! # Behaviour and limits
//!
//! - Only **WPA2-Personal** is accepted, so an access point cannot talk the chip into a weaker
//!   mode. WPA3-only networks are not supported by this version of `esp-radio`; a network in
//!   WPA2/WPA3 transition mode works.
//! - After a lost connection or a failed attempt the link retries forever with a growing delay
//!   (1 s up to 30 s). After [`FAILED_AFTER`] failures in a row the status shows
//!   [`LinkState::Failed`] but the retries go on, so the board reconnects when the router
//!   comes back.
//! - Progress is published as a [`WifiStatus`] on [`WIFI_STATUS`] for a display or anything else.
//!
//! # What this does not protect
//!
//! `esp-radio`'s `Password` is `Copy` and lives in RAM while the radio runs; it cannot be
//! wiped. The password of a build that used the environment variable is also part of that
//! firmware image (and of `target/` on your computer).

use core::fmt;
use core::net::Ipv4Addr;

use embassy_executor::Spawner;
use embassy_net::{Config as NetConfig, Runner, Stack, StackResources};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::watch::Watch;
use embassy_time::{Duration, Timer};
use esp_hal::peripherals::WIFI;
use esp_hal::rng::Rng;
use esp_radio::wifi::sta::StationConfig;
use esp_radio::wifi::{
    AuthenticationMethodConfig, Config, ConnectionError, ControllerConfig, Interface, Password,
    Ssid, WifiController, WifiError,
};
use log::{info, warn};
use static_cell::StaticCell;

use crate::secret_store::{self, MAX_SECRET_LEN, Secret, SecretStore};

/// The secret-store slot that holds the Wi-Fi credentials (SSID and password together).
pub const CREDENTIALS_SLOT: u8 = 0;
/// Longest SSID in bytes (802.11 limit).
pub const MAX_SSID_LEN: usize = 32;
/// Shortest WPA2 passphrase in bytes.
pub const MIN_PASSWORD_LEN: usize = 8;
/// Longest WPA2 passphrase in bytes (63 characters, or 64 hex digits for a raw key).
pub const MAX_PASSWORD_LEN: usize = 64;
/// Consecutive failed connection attempts before the status becomes [`LinkState::Failed`].
pub const FAILED_AFTER: u32 = 3;

const STATUS_POLL: Duration = Duration::from_secs(2);
const BACKOFF_START: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(30);
/// Sockets the network stack can hold. DHCP needs one; the rest are free for later use.
const STACK_SOCKETS: usize = 3;

/// The latest [`WifiStatus`]. Read it with `WIFI_STATUS.receiver()` (up to 2 receivers).
pub static WIFI_STATUS: Watch<CriticalSectionRawMutex, WifiStatus, 2> =
    Watch::new_with(WifiStatus::idle());

static NET_RESOURCES: StaticCell<StackResources<STACK_SOCKETS>> = StaticCell::new();

/// What can go wrong when setting up Wi-Fi.
#[derive(Debug)]
pub enum Error {
    /// Only one of `PULSAR_WIFI_SSID` and `PULSAR_WIFI_PASSWORD` was set at build time.
    IncompleteBuildEnv,
    /// Nothing in the build environment and nothing in the secret store.
    NoCredentials,
    /// The SSID is empty, longer than [`MAX_SSID_LEN`] bytes, or the stored data is damaged.
    InvalidSsid,
    /// The password is shorter than [`MIN_PASSWORD_LEN`] or longer than [`MAX_PASSWORD_LEN`].
    InvalidPassword,
    /// [`WifiLink::start`] was called a second time.
    AlreadyStarted,
    /// The secret store failed.
    Store(secret_store::Error),
    /// The Wi-Fi driver failed.
    Radio(WifiError),
}

impl From<secret_store::Error> for Error {
    fn from(error: secret_store::Error) -> Self {
        Self::Store(error)
    }
}

impl From<WifiError> for Error {
    fn from(error: WifiError) -> Self {
        Self::Radio(error)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IncompleteBuildEnv => write!(
                f,
                "set both PULSAR_WIFI_SSID and PULSAR_WIFI_PASSWORD at build time, or neither"
            ),
            Self::NoCredentials => write!(
                f,
                "no Wi-Fi credentials: build once with PULSAR_WIFI_SSID and PULSAR_WIFI_PASSWORD set"
            ),
            Self::InvalidSsid => write!(f, "invalid SSID (1 to {MAX_SSID_LEN} bytes of UTF-8)"),
            Self::InvalidPassword => write!(
                f,
                "invalid password ({MIN_PASSWORD_LEN} to {MAX_PASSWORD_LEN} bytes)"
            ),
            Self::AlreadyStarted => write!(f, "Wi-Fi was already started"),
            Self::Store(error) => write!(f, "secret store: {error}"),
            Self::Radio(error) => write!(f, "Wi-Fi driver: {error:?}"),
        }
    }
}

impl core::error::Error for Error {}

/// The SSID and password of a WPA2 network. `Debug` never prints the password.
pub struct WifiCredentials {
    ssid: [u8; MAX_SSID_LEN],
    ssid_len: usize,
    password: Secret<MAX_PASSWORD_LEN>,
}

impl WifiCredentials {
    /// Checks the lengths and copies both values.
    pub fn new(ssid: &str, password: &str) -> Result<Self, Error> {
        Self::from_bytes(ssid.as_bytes(), password.as_bytes())
    }

    fn from_bytes(ssid: &[u8], password: &[u8]) -> Result<Self, Error> {
        if ssid.is_empty() || ssid.len() > MAX_SSID_LEN || core::str::from_utf8(ssid).is_err() {
            return Err(Error::InvalidSsid);
        }
        if !(MIN_PASSWORD_LEN..=MAX_PASSWORD_LEN).contains(&password.len()) {
            return Err(Error::InvalidPassword);
        }
        let password = Secret::from_slice(password).ok_or(Error::InvalidPassword)?;
        let mut buffer = [0; MAX_SSID_LEN];
        buffer[..ssid.len()].copy_from_slice(ssid);
        Ok(Self {
            ssid: buffer,
            ssid_len: ssid.len(),
            password,
        })
    }

    /// The network name.
    pub fn ssid(&self) -> &str {
        // Checked to be valid UTF-8 in `from_bytes`.
        core::str::from_utf8(&self.ssid[..self.ssid_len]).unwrap_or("")
    }

    /// One byte for the SSID length, the SSID, then the password: the format kept in the store.
    fn encode(&self) -> Result<Secret<MAX_SECRET_LEN>, Error> {
        Secret::concat(&[
            &[self.ssid_len as u8],
            &self.ssid[..self.ssid_len],
            self.password.as_bytes(),
        ])
        .ok_or(Error::InvalidPassword)
    }

    fn decode(bytes: &[u8]) -> Result<Self, Error> {
        let (&ssid_len, rest) = bytes.split_first().ok_or(Error::InvalidSsid)?;
        let ssid_len = usize::from(ssid_len);
        if rest.len() < ssid_len {
            return Err(Error::InvalidSsid);
        }
        let (ssid, password) = rest.split_at(ssid_len);
        Self::from_bytes(ssid, password)
    }
}

impl fmt::Debug for WifiCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "WifiCredentials {{ ssid: {:?}, password: <redacted> }}",
            self.ssid()
        )
    }
}

/// Gets the credentials: from the build environment (and stores them), or else from `store`.
///
/// See the [module documentation](self) for the environment variables. Never logs the password.
pub fn load_or_provision(store: &mut impl SecretStore) -> Result<WifiCredentials, Error> {
    let from_env = |value: Option<&'static str>| value.filter(|value| !value.is_empty());

    match (
        from_env(option_env!("PULSAR_WIFI_SSID")),
        from_env(option_env!("PULSAR_WIFI_PASSWORD")),
    ) {
        (Some(ssid), Some(password)) => {
            let credentials = WifiCredentials::new(ssid, password)?;
            store.put(CREDENTIALS_SLOT, credentials.encode()?.as_bytes())?;
            info!(
                "wifi: credentials for '{}' taken from the build environment and stored",
                credentials.ssid()
            );
            Ok(credentials)
        }
        (None, None) => {
            let stored = store
                .get_secret::<MAX_SECRET_LEN>(CREDENTIALS_SLOT)?
                .ok_or(Error::NoCredentials)?;
            let credentials = WifiCredentials::decode(stored.as_bytes())?;
            info!(
                "wifi: using stored credentials for '{}'",
                credentials.ssid()
            );
            Ok(credentials)
        }
        _ => Err(Error::IncompleteBuildEnv),
    }
}

/// How the link is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkState {
    /// Wi-Fi has not been started yet.
    Idle,
    /// No credentials were available, so Wi-Fi was not started.
    NoCredentials,
    /// Trying to join the network.
    Connecting,
    /// Associated with the access point (an IP address may still be pending).
    Connected,
    /// [`FAILED_AFTER`] or more attempts in a row failed; still retrying.
    Failed,
}

/// A snapshot of the Wi-Fi link, published on [`WIFI_STATUS`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WifiStatus {
    pub state: LinkState,
    ssid: [u8; MAX_SSID_LEN],
    ssid_len: u8,
    /// Signal strength of the access point in dBm (about -30 is excellent, -90 is unusable).
    pub rssi_dbm: Option<i32>,
    /// The address from DHCP, once there is one.
    pub ip: Option<Ipv4Addr>,
}

impl WifiStatus {
    const fn idle() -> Self {
        Self {
            state: LinkState::Idle,
            ssid: [0; MAX_SSID_LEN],
            ssid_len: 0,
            rssi_dbm: None,
            ip: None,
        }
    }

    /// A status with no SSID, signal or address. For states that need no more than the state.
    pub const fn with_state(state: LinkState) -> Self {
        Self {
            state,
            ..Self::idle()
        }
    }

    fn for_network(state: LinkState, ssid: &str) -> Self {
        let mut status = Self::with_state(state);
        let len = ssid.len().min(MAX_SSID_LEN);
        status.ssid[..len].copy_from_slice(&ssid.as_bytes()[..len]);
        status.ssid_len = len as u8;
        status
    }

    /// The network name, empty when unknown.
    pub fn ssid(&self) -> &str {
        core::str::from_utf8(&self.ssid[..usize::from(self.ssid_len)]).unwrap_or("")
    }
}

/// A running Wi-Fi station with an IPv4 network stack.
pub struct WifiLink {
    stack: Stack<'static>,
}

impl WifiLink {
    /// Starts the radio, begins joining the network and returns at once. Use
    /// [`wait_for_ip`](Self::wait_for_ip) to wait for the connection.
    ///
    /// Spawns two tasks (the connection manager and the network stack runner).
    pub fn start(
        wifi: WIFI<'static>,
        credentials: &WifiCredentials,
        spawner: Spawner,
    ) -> Result<Self, Error> {
        let resources = NET_RESOURCES
            .try_init(StackResources::new())
            .ok_or(Error::AlreadyStarted)?;

        let mut controller = WifiController::new(wifi, ControllerConfig::default())?;
        let station = StationConfig::default()
            .with_ssid(Ssid::try_from(credentials.ssid())?)
            .with_authentication(AuthenticationMethodConfig::Wpa2Personal(
                Password::try_from(credentials.password.as_bytes())?,
            ));
        controller.set_config(&Config::Station(station))?;

        let rng = Rng::new();
        let seed = (u64::from(rng.random()) << 32) | u64::from(rng.random());
        let (stack, runner) = embassy_net::new(
            Interface::station(),
            NetConfig::dhcpv4(Default::default()),
            resources,
            seed,
        );

        let network = WifiStatus::for_network(LinkState::Connecting, credentials.ssid());
        WIFI_STATUS.sender().send(network);
        spawner.spawn(net_task(runner).unwrap());
        spawner.spawn(connection_task(controller, stack, network).unwrap());
        Ok(Self { stack })
    }

    /// Waits until DHCP has given the board an address and returns it.
    pub async fn wait_for_ip(&self) -> Ipv4Addr {
        loop {
            self.stack.wait_config_up().await;
            if let Some(ip) = self.ip() {
                return ip;
            }
        }
    }

    /// The current IPv4 address, if there is one.
    pub fn ip(&self) -> Option<Ipv4Addr> {
        ip_of(self.stack)
    }

    /// The network stack, for opening sockets.
    pub fn stack(&self) -> Stack<'static> {
        self.stack
    }
}

fn ip_of(stack: Stack<'static>) -> Option<Ipv4Addr> {
    stack.config_v4().map(|config| config.address.address())
}

fn backoff(failures: u32) -> Duration {
    let shift = failures.saturating_sub(1).min(5);
    (BACKOFF_START * (1 << shift)).min(BACKOFF_MAX)
}

#[embassy_executor::task]
async fn net_task(mut runner: Runner<'static, Interface>) -> ! {
    runner.run().await
}

/// Joins the network, publishes the status while connected, and rejoins after any loss.
#[embassy_executor::task]
async fn connection_task(
    mut controller: WifiController<'static>,
    stack: Stack<'static>,
    network: WifiStatus,
) -> ! {
    let status = WIFI_STATUS.sender();
    let mut failures: u32 = 0;

    loop {
        let state = if failures >= FAILED_AFTER {
            LinkState::Failed
        } else {
            LinkState::Connecting
        };
        status.send(WifiStatus { state, ..network });

        match controller.connect_async().await {
            Ok(info) => {
                failures = 0;
                info!("wifi: connected on channel {}", info.channel);
                // `rssi()` fails with `NotConnected` once the access point is gone.
                loop {
                    let rssi_dbm = match controller.rssi() {
                        Ok(rssi) => Some(rssi),
                        Err(WifiError::NotConnected) => break,
                        Err(_) => None,
                    };
                    status.send(WifiStatus {
                        state: LinkState::Connected,
                        rssi_dbm,
                        ip: ip_of(stack),
                        ..network
                    });
                    Timer::after(STATUS_POLL).await;
                }
                warn!("wifi: connection lost");
                Timer::after(BACKOFF_START).await;
            }
            Err(error) => {
                failures = failures.saturating_add(1);
                // Log the reason only: the full error carries the SSID and BSSID.
                match error {
                    ConnectionError::Failed(info) => {
                        warn!(
                            "wifi: connect failed (attempt {failures}): {:?}",
                            info.reason
                        );
                    }
                    ConnectionError::WifiError(error) => {
                        warn!("wifi: connect failed (attempt {failures}): {error:?}");
                    }
                    _ => warn!("wifi: connect failed (attempt {failures})"),
                }
                Timer::after(backoff(failures)).await;
            }
        }
    }
}
