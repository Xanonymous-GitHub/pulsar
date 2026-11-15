//! Where the board is: the city its network is in, found from its public address.
//!
//! ```ignore
//! let mut locator = Locator::new();
//! if locator.refresh_if_due() {
//!     // the location changed
//! }
//! let location = locator.current(); // "Taipei", 25.05 N 121.53 E
//! ```
//!
//! The location decides which forecast [`crate::weather`] asks for, and through the forecast the
//! time zone of the clock. It is as exact as a city, which is all a network address tells.
//!
//! # Where it comes from
//!
//! With [`LocationSource::Network`] the board asks a location service over HTTPS. The request
//! says nothing about the board: the service sees the public address the request comes from, as
//! every server does, and answers with the city and coordinates registered for it. The services
//! in [`PROVIDERS`] are tried in order until one answers. Until then, and when all of them fail,
//! the place in [`config`](crate::config) is used (`LOCATION_FALLBACK_*`).
//!
//! With [`LocationSource::Fixed`] no service is asked and the place in the configuration is used
//! always. Choose it when the address of the network is registered in the wrong city (a VPN, a
//! mobile provider), or to keep the board from asking.
//!
//! # When it is asked
//!
//! After every new Wi-Fi connection ([`Locator::invalidate`]), since that may be another network
//! in another place, and then every [`LOCATION_REFRESH`]. After a failure the last known location
//! stays, and the services are asked again after [`LOCATION_RETRY`]. In no case are they asked
//! more often than every [`LOCATION_MIN_INTERVAL`]: a connection that keeps dropping must not
//! become a stream of requests.
//!
//! The location is kept in RAM only and is never written to flash.

mod place;

use std::time::Instant;

use serde::Deserialize;

use crate::config::{
    LOCATION_FALLBACK_LATITUDE, LOCATION_FALLBACK_LONGITUDE, LOCATION_FALLBACK_PLACE,
    LOCATION_MIN_INTERVAL, LOCATION_REFRESH, LOCATION_RETRY, LOCATION_SOURCE, LOCATION_TIMEOUT,
    LocationSource,
};
use crate::http::{self, Client};

pub use place::{Coordinates, MAX_PLACE_CHARS, ascii_place};

/// What can go wrong when asking one location service.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The request failed.
    #[error("request: {0}")]
    Http(#[from] http::Error),
    /// The service answered with an error status (429 means it is asked too often).
    #[error("the service answered with status {0}")]
    Status(u16),
    /// The answer is not the JSON that was expected.
    #[error("unexpected answer: {0}")]
    Json(#[from] serde_json::Error),
    /// The service does not know where the address is, or says somewhere that is not on the globe.
    #[error("the service does not know the location")]
    Unknown,
}

/// A place on the globe with a name for the screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Location {
    /// The name of the place: ASCII, at most [`MAX_PLACE_CHARS`] characters.
    pub place: String,
    /// Where it is.
    pub coordinates: Coordinates,
}

impl Location {
    /// A location from what a service said. The name falls back to `country` (a two-letter
    /// code) when the city is missing or cannot be shown.
    fn from_service(
        city: Option<&str>,
        country: Option<&str>,
        latitude: f64,
        longitude: f64,
    ) -> Result<Self, Error> {
        let coordinates = Coordinates::new(latitude, longitude).ok_or(Error::Unknown)?;
        let place = city
            .and_then(ascii_place)
            .or_else(|| country.and_then(ascii_place))
            .ok_or(Error::Unknown)?;
        Ok(Self { place, coordinates })
    }

    /// The place in the configuration, or `None` if the coordinates there are not on the globe.
    fn configured() -> Option<Self> {
        Some(Self {
            place: ascii_place(LOCATION_FALLBACK_PLACE)?,
            coordinates: Coordinates::new(LOCATION_FALLBACK_LATITUDE, LOCATION_FALLBACK_LONGITUDE)?,
        })
    }
}

/// A location service: where to ask and how to read the answer.
struct Provider {
    /// The host, for the log.
    name: &'static str,
    /// The address to `GET`. The answer describes the address the request came from.
    url: &'static str,
    /// Reads the answer.
    parse: fn(&[u8]) -> Result<Location, Error>,
}

/// The services, in the order they are tried. Both are free without a key for this kind of use,
/// answer in a few hundred bytes, and speak TLS 1.3.
const PROVIDERS: [Provider; 2] = [
    Provider {
        name: "ipwho.is",
        url: "https://ipwho.is/?fields=success,city,country_code,latitude,longitude",
        parse: parse_ipwhois,
    },
    Provider {
        name: "get.geojs.io",
        url: "https://get.geojs.io/v1/ip/geo.json",
        parse: parse_geojs,
    },
];

/// The answer of `ipwho.is`, for example
/// `{"success":true,"country_code":"TW","city":"Taipei","latitude":25.05,"longitude":121.52}`.
/// With `"success":false` the other fields are missing.
#[derive(Deserialize)]
struct IpWhois<'a> {
    success: bool,
    #[serde(borrow, default)]
    city: Option<std::borrow::Cow<'a, str>>,
    #[serde(borrow, default)]
    country_code: Option<std::borrow::Cow<'a, str>>,
    #[serde(default)]
    latitude: Option<f64>,
    #[serde(default)]
    longitude: Option<f64>,
}

fn parse_ipwhois(body: &[u8]) -> Result<Location, Error> {
    let answer: IpWhois = serde_json::from_slice(body)?;
    let (true, Some(latitude), Some(longitude)) =
        (answer.success, answer.latitude, answer.longitude)
    else {
        return Err(Error::Unknown);
    };
    Location::from_service(
        answer.city.as_deref(),
        answer.country_code.as_deref(),
        latitude,
        longitude,
    )
}

/// The answer of `get.geojs.io`, which gives the coordinates as text, for example
/// `{"city":"Taipei","country_code":"TW","latitude":"25.0504","longitude":"121.5324"}`. The city
/// is missing for addresses it only knows the country of.
#[derive(Deserialize)]
struct GeoJs<'a> {
    #[serde(borrow, default)]
    city: Option<std::borrow::Cow<'a, str>>,
    #[serde(borrow, default)]
    country_code: Option<std::borrow::Cow<'a, str>>,
    #[serde(borrow)]
    latitude: std::borrow::Cow<'a, str>,
    #[serde(borrow)]
    longitude: std::borrow::Cow<'a, str>,
}

fn parse_geojs(body: &[u8]) -> Result<Location, Error> {
    let answer: GeoJs = serde_json::from_slice(body)?;
    let degrees = |text: &str| text.trim().parse::<f64>().map_err(|_| Error::Unknown);
    Location::from_service(
        answer.city.as_deref(),
        answer.country_code.as_deref(),
        degrees(&answer.latitude)?,
        degrees(&answer.longitude)?,
    )
}

/// Asks one service.
fn ask(client: &Client, provider: &Provider) -> Result<Location, Error> {
    let response = client.get(provider.url)?;
    if !response.is_success() {
        return Err(Error::Status(response.status()));
    }
    (provider.parse)(&response.bytes()?)
}

/// Asks the services in order and returns the first location one of them gives.
///
/// # Errors
///
/// Fails with the error of the last service if none of them gives a location.
pub fn lookup(client: &Client) -> Result<Location, Error> {
    let mut last_error = Error::Unknown;
    for provider in &PROVIDERS {
        match ask(client, provider) {
            Ok(location) => {
                log::info!("location: {} (from {})", location.place, provider.name);
                return Ok(location);
            }
            Err(error) => {
                log::warn!("location service {} failed: {error}", provider.name);
                last_error = error;
            }
        }
    }
    Err(last_error)
}

/// Keeps the current location and decides when to ask for it again, see the
/// [module documentation](self).
pub struct Locator {
    client: Client,
    /// `None` only if the place in the configuration is invalid and no service has answered.
    current: Option<Location>,
    /// When to ask the services next. `None`: never ([`LocationSource::Fixed`]).
    next_lookup: Option<Instant>,
    /// When the services were last asked, whether they answered or not.
    last_lookup: Option<Instant>,
}

impl Locator {
    /// Starts with the place from the configuration. Nothing is asked until
    /// [`refresh_if_due`](Self::refresh_if_due).
    #[must_use]
    pub fn new() -> Self {
        let current = Location::configured();
        if current.is_none() {
            log::error!("the LOCATION_FALLBACK_* values in config.rs are not a place on the globe");
        }
        Self {
            client: Client::new().with_timeout(LOCATION_TIMEOUT),
            current,
            next_lookup: (LOCATION_SOURCE == LocationSource::Network).then(Instant::now),
            last_lookup: None,
        }
    }

    /// The location to use now. `None` only if the place in the configuration is invalid and no
    /// service has answered yet.
    #[must_use]
    pub const fn current(&self) -> Option<&Location> {
        self.current.as_ref()
    }

    /// Makes [`refresh_if_due`](Self::refresh_if_due) ask the services as soon as
    /// [`LOCATION_MIN_INTERVAL`] has passed since they were last asked (at once if they never
    /// were). Call it when Wi-Fi has connected: it may be another network, in another place.
    pub fn invalidate(&mut self) {
        let Some(scheduled) = self.next_lookup else {
            return;
        };
        let soonest = self
            .last_lookup
            .map_or_else(Instant::now, |last| last + LOCATION_MIN_INTERVAL);
        self.next_lookup = Some(scheduled.min(soonest));
    }

    /// Asks the services if it is time to, which takes a few seconds, and returns whether the
    /// location is another one now. Call it only while the board is online. A failure is logged
    /// and keeps the location as it is.
    pub fn refresh_if_due(&mut self) -> bool {
        if self.next_lookup.is_none_or(|due| Instant::now() < due) {
            return false;
        }
        self.last_lookup = Some(Instant::now());
        let Ok(location) = lookup(&self.client) else {
            // Each service has logged why. The last known place stays in use.
            self.next_lookup = Some(Instant::now() + LOCATION_RETRY);
            return false;
        };
        self.next_lookup = Some(Instant::now() + LOCATION_REFRESH);
        let changed = self.current.as_ref() != Some(&location);
        self.current = Some(location);
        changed
    }
}

impl Default for Locator {
    fn default() -> Self {
        Self::new()
    }
}
