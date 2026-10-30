//! Where the broker is and how to log in: build-time environment variables, kept in the secret
//! store, the same way as the Wi-Fi credentials.
//!
//! [`Settings::load_or_provision`] reads these **build-time** variables of the shell that runs
//! `cargo`:
//!
//! - `PULSAR_MQTT_URL`: `mqtt://host[:port]` (TCP, port 1883), `mqtts://host[:port]` (TLS, 8883),
//!   `ws://host[:port]/path` or `wss://host[:port]/path` (WebSocket, plain or over TLS). The URL
//!   holds no user name or password.
//! - `PULSAR_MQTT_USERNAME` and `PULSAR_MQTT_PASSWORD`: optional. A password needs a user name.
//!
//! If the URL is set, the three values are written to the [`SecretStore`] (nothing is written
//! when they are unchanged) and replace what was stored. If none is set, the stored settings are
//! used, so you can rebuild without the password in your shell; if there are none, MQTT is simply
//! off. Setting a user name or password without a URL is an error. Empty values count as unset.
//!
//! `PULSAR_MQTT_URL=off` turns MQTT off again: the stored settings are erased and nothing connects.
//!
//! The password is never logged. Keep it out of your shell history, for example with
//! `read -rs PULSAR_MQTT_PASSWORD; export PULSAR_MQTT_PASSWORD`.

use core::fmt;

use zeroize::Zeroizing;

use super::Error;
use crate::config::MQTT_SECRET_KEY;
use crate::secrets::SecretStore;

/// Longest URL in bytes.
const MAX_URL_LEN: usize = 256;
/// Longest user name or password in bytes.
const MAX_CREDENTIAL_LEN: usize = 256;
/// The value of `PULSAR_MQTT_URL` that erases the stored settings and turns MQTT off.
const OFF: &str = "off";
/// The first byte of the stored record: the format version.
const RECORD_VERSION: u8 = 1;

/// Schemes the client can speak, and whether they use TLS.
const SCHEMES: [(&str, bool); 4] = [
    ("mqtt://", false),
    ("mqtts://", true),
    ("ws://", false),
    ("wss://", true),
];

/// The broker URL and the login. `Debug` never prints the password, and the password is wiped from
/// memory when this is dropped.
#[derive(Clone)]
pub struct Settings {
    url: String,
    username: Option<String>,
    password: Option<Zeroizing<String>>,
}

impl Settings {
    /// Checks and copies the values. Empty strings count as absent.
    ///
    /// # Errors
    ///
    /// Fails if the URL is not valid (see [`Error::InvalidUrl`]), or a credential is too long, has
    /// a NUL byte, or there is a password without a user name.
    pub fn new(url: &str, username: Option<&str>, password: Option<&str>) -> Result<Self, Error> {
        let (username, password) = (non_empty(username), non_empty(password));

        let credential_ok =
            |value: &str| value.len() <= MAX_CREDENTIAL_LEN && !value.contains('\0');
        if !username.into_iter().chain(password).all(credential_ok)
            || (password.is_some() && username.is_none())
        {
            return Err(Error::InvalidCredentials);
        }
        if !is_valid_url(url) {
            return Err(Error::InvalidUrl);
        }
        Ok(Self {
            url: url.to_owned(),
            username: username.map(str::to_owned),
            password: password.map(|password| Zeroizing::new(password.to_owned())),
        })
    }

    /// The broker URL.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// The user name, if the broker needs one.
    #[must_use]
    pub fn username(&self) -> Option<&str> {
        self.username.as_deref()
    }

    /// The password, if the broker needs one.
    #[must_use]
    pub fn password(&self) -> Option<&str> {
        self.password.as_ref().map(|password| password.as_str())
    }

    /// Whether the connection is encrypted (`mqtts://` or `wss://`).
    #[must_use]
    pub fn uses_tls(&self) -> bool {
        scheme(&self.url).is_some_and(|(_, tls)| tls)
    }

    /// The `host:port` part of the URL, for logging.
    #[must_use]
    pub fn host(&self) -> &str {
        authority(&self.url)
    }

    /// Gets the settings: from the build environment (and stores them), or else from `secrets`.
    /// `None` means MQTT is not configured, which is not an error.
    ///
    /// See the [module documentation](self) for the environment variables.
    ///
    /// # Errors
    ///
    /// Fails if the values are invalid, only some of them are set, or the store fails.
    pub fn load_or_provision(secrets: &SecretStore) -> Result<Option<Self>, Error> {
        let from_env = |value: Option<&'static str>| value.filter(|value| !value.is_empty());
        let url = from_env(option_env!("PULSAR_MQTT_URL"));
        let username = from_env(option_env!("PULSAR_MQTT_USERNAME"));
        let password = from_env(option_env!("PULSAR_MQTT_PASSWORD"));

        match url {
            Some(OFF) => {
                secrets.erase(MQTT_SECRET_KEY)?;
                log::info!("MQTT turned off: the stored settings are erased");
                Ok(None)
            }
            Some(url) => {
                let settings = Self::new(url, username, password)?;
                secrets.put(MQTT_SECRET_KEY, &settings.encode())?;
                log::info!(
                    "MQTT settings for '{}' taken from the build environment and stored",
                    settings.host()
                );
                Ok(Some(settings))
            }
            None if username.is_some() || password.is_some() => Err(Error::IncompleteBuildEnv),
            None => {
                let Some(record) = secrets.get(MQTT_SECRET_KEY)? else {
                    return Ok(None);
                };
                let settings = Self::decode(&record)?;
                log::info!("using the stored MQTT settings for '{}'", settings.host());
                Ok(Some(settings))
            }
        }
    }

    /// The format kept in the store: the version byte, then the URL, the user name and the
    /// password, each as a 16-bit little-endian length and the bytes (length 0 is absent).
    fn encode(&self) -> Zeroizing<Vec<u8>> {
        let fields = [
            self.url.as_str(),
            self.username.as_deref().unwrap_or_default(),
            self.password().unwrap_or_default(),
        ];
        let mut record = Zeroizing::new(Vec::with_capacity(
            1 + fields.iter().map(|field| 2 + field.len()).sum::<usize>(),
        ));
        record.push(RECORD_VERSION);
        for field in fields {
            // Every field is at most 256 bytes, so the length always fits in 16 bits.
            record.extend_from_slice(&u16::try_from(field.len()).unwrap_or(u16::MAX).to_le_bytes());
            record.extend_from_slice(field.as_bytes());
        }
        record
    }

    fn decode(record: &[u8]) -> Result<Self, Error> {
        let (&version, mut rest) = record.split_first().ok_or(Error::InvalidUrl)?;
        if version != RECORD_VERSION {
            return Err(Error::InvalidUrl);
        }

        let mut next_field = || -> Option<&str> {
            let (length, tail) = rest.split_first_chunk::<2>()?;
            let (field, tail) = tail.split_at_checked(usize::from(u16::from_le_bytes(*length)))?;
            rest = tail;
            core::str::from_utf8(field).ok()
        };
        let (Some(url), Some(username), Some(password)) =
            (next_field(), next_field(), next_field())
        else {
            return Err(Error::InvalidUrl);
        };
        Self::new(url, Some(username), Some(password))
    }
}

impl fmt::Debug for Settings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Settings")
            .field("host", &self.host())
            .field("username", &self.username)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .finish_non_exhaustive()
    }
}

/// `value` unless it is empty: empty settings count as unset.
fn non_empty(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.is_empty())
}

/// The scheme of `url` and whether it uses TLS, if the client supports it.
fn scheme(url: &str) -> Option<(&'static str, bool)> {
    SCHEMES.into_iter().find(|(scheme, _)| {
        url.get(..scheme.len())
            .is_some_and(|start| start.eq_ignore_ascii_case(scheme))
    })
}

/// The `host[:port]` part: after the scheme, up to the first `/`, `?` or `#`.
fn authority(url: &str) -> &str {
    let rest = scheme(url).map_or(url, |(scheme, _)| &url[scheme.len()..]);
    rest.split(['/', '?', '#']).next().unwrap_or_default()
}

/// Whether the client can use `url`: a supported scheme, a host, no whitespace, no control
/// characters, and no user name or password inside (those have their own settings, so that they
/// are kept out of logs).
fn is_valid_url(url: &str) -> bool {
    url.len() <= MAX_URL_LEN
        && scheme(url).is_some()
        && !url.chars().any(|c| c.is_whitespace() || c.is_control())
        && !authority(url).is_empty()
        && !authority(url).contains('@')
}
