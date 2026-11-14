//! Fetches the weather forecast for where the board is, over HTTPS, and publishes it as the
//! scrolling text. The same answer sets the time zone of the clock.
//!
//! ```ignore
//! weather::spawn(status.clone(), message.clone())?;
//! ```
//!
//! The data comes from the free [Open-Meteo](https://open-meteo.com) API (see
//! [`config::WEATHER_API`](crate::config::WEATHER_API)), through [`crate::http`]. A report only
//! arrives if the whole chain works: Wi-Fi and DHCP, DNS, the TLS handshake and the certificate
//! chain, the certificate dates (so the SNTP clock), and the HTTP exchange.
//!
//! The text looks like `Taipei 27C Overcast | Today 24-29C, rain 40% | Wind 12 km/h`. It is plain
//! ASCII because the display font has no other characters.
//!
//! # Place and time zone
//!
//! The place comes from [`crate::location`]: the city the network is in, or the one in the
//! configuration. The forecast is asked for its coordinates with `timezone=auto`, so the service
//! works out the time zone of the place and reports its offset from UTC as it is now, daylight
//! saving time included. That offset goes to [`clock::set_utc_offset`] with every report, which
//! is how the clock on the screen is right without a time zone database on the board.
//!
//! This thread is also the one that notices Wi-Fi coming up: it then makes the clock ask the
//! time servers at once and the location be looked up again.
//!
//! # When the internet is gone
//!
//! The Wi-Fi may be down, or up with no internet behind it (requests then fail at DNS or time
//! out). Either way the thread keeps going and the screen never lies about the data:
//!
//! - Updates are only tried while Wi-Fi has an address. One that falls due during an outage runs
//!   as soon as the connection is back, so the screen catches up within seconds.
//! - Failed updates are retried with a doubling wait, from [`WEATHER_RETRY`] up to
//!   [`WEATHER_RETRY_MAX`], so a dead link is not hammered. The first retry comes quickly: right
//!   after Wi-Fi connects a request sometimes stalls although the network is fine.
//! - The last report stays on the screen, and gets `(old)` in front once it is older than
//!   [`WEATHER_STALE_AFTER`]. Before the first report, a failed update shows `Weather
//!   unavailable`.
//! - The clock keeps running without SNTP, so the time on the screen goes on, in the zone it
//!   last knew.

use std::io;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::clock;
use crate::config::{
    WEATHER_API, WEATHER_REFRESH, WEATHER_RETRY, WEATHER_RETRY_MAX, WEATHER_STACK_BYTES,
    WEATHER_STALE_AFTER,
};
use crate::http::{self, Client};
use crate::location::{Location, Locator};
use crate::message::SharedMessage;
use crate::status::SharedStatus;

/// How often the thread looks at the network, the clock and what the screen should say.
const TICK: Duration = Duration::from_secs(1);

/// What can go wrong with one update.
#[derive(Debug, thiserror::Error)]
enum Error {
    /// The request failed.
    #[error("request: {0}")]
    Http(#[from] http::Error),
    /// The server answered with an error status.
    #[error("the server answered with status {0}")]
    Status(u16),
    /// The answer is not the JSON that was expected.
    #[error("unexpected answer: {0}")]
    Json(#[from] serde_json::Error),
    /// The answer holds no value for today.
    #[error("the forecast for today is missing")]
    Missing,
    /// There is no place to ask the forecast for (the one in the configuration is invalid).
    #[error("no location")]
    NoLocation,
}

/// The parts of the Open-Meteo answer that are used.
#[derive(Debug, Deserialize)]
struct Forecast {
    /// The offset from UTC of the time zone at the place, as it is now, in seconds.
    utc_offset_seconds: i32,
    current: Current,
    daily: Daily,
}

#[derive(Debug, Deserialize)]
struct Current {
    temperature_2m: f32,
    /// A WMO weather code, see [`describe`].
    weather_code: u16,
    /// In km/h.
    wind_speed_10m: f32,
}

/// One entry per forecast day; only today is asked for.
#[derive(Debug, Deserialize)]
struct Daily {
    temperature_2m_max: Vec<f32>,
    temperature_2m_min: Vec<f32>,
    /// `null` when the model has no value.
    precipitation_probability_max: Vec<Option<f32>>,
}

impl Forecast {
    /// The one-line report for `place`.
    fn summary(&self, place: &str) -> Result<String, Error> {
        let (Some(high), Some(low)) = (
            self.daily.temperature_2m_max.first(),
            self.daily.temperature_2m_min.first(),
        ) else {
            return Err(Error::Missing);
        };
        let rain = self
            .daily
            .precipitation_probability_max
            .first()
            .copied()
            .flatten()
            .map_or_else(String::new, |chance| format!(", rain {chance:.0}%"));

        Ok(format!(
            "{place} {:.0}C {} | Today {low:.0}-{high:.0}C{rain} | Wind {:.0} km/h",
            self.current.temperature_2m,
            describe(self.current.weather_code),
            self.current.wind_speed_10m,
        ))
    }
}

/// A short English name for a WMO weather code (the codes Open-Meteo uses).
fn describe(code: u16) -> &'static str {
    match code {
        0 => "Clear",
        1 => "Mostly clear",
        2 => "Partly cloudy",
        3 => "Overcast",
        45 | 48 => "Fog",
        51 | 53 | 55 => "Drizzle",
        56 | 57 => "Freezing drizzle",
        61 => "Light rain",
        63 => "Rain",
        65 => "Heavy rain",
        66 | 67 => "Freezing rain",
        71 | 73 | 75 | 77 => "Snow",
        80..=82 => "Showers",
        85 | 86 => "Snow showers",
        95 => "Thunderstorm",
        96 | 99 => "Thunderstorm, hail",
        _ => "Unknown",
    }
}

/// The address of the forecast for `location`.
fn forecast_url(location: &Location) -> String {
    format!(
        "{WEATHER_API}&latitude={}&longitude={}",
        location.coordinates.latitude(),
        location.coordinates.longitude()
    )
}

/// Downloads the forecast for `location`, sets the clock's time zone from it and returns the
/// report.
fn fetch(client: &Client, location: &Location) -> Result<String, Error> {
    let response = client.get(&forecast_url(location))?;
    if !response.is_success() {
        return Err(Error::Status(response.status()));
    }
    let forecast: Forecast = serde_json::from_slice(&response.bytes()?)?;
    if !clock::set_utc_offset(forecast.utc_offset_seconds) {
        log::warn!(
            "the weather service gave an impossible UTC offset ({} s), keeping the old one",
            forecast.utc_offset_seconds
        );
    }
    forecast.summary(&location.place)
}

/// Starts the thread that keeps `message` up to date, see the [module documentation](self).
///
/// # Errors
///
/// Fails if the thread cannot be created.
pub fn spawn(status: SharedStatus, message: SharedMessage) -> io::Result<JoinHandle<()>> {
    thread::Builder::new()
        .name("weather".into())
        .stack_size(WEATHER_STACK_BYTES)
        .spawn(move || run(&status, &message))
}

/// The newest report that arrived, and when.
struct Latest {
    report: String,
    fetched: Instant,
}

/// What the screen should say now, or `None` to leave the text as it is (nothing was tried yet).
fn display_text(latest: Option<&Latest>, failures: u32) -> Option<String> {
    match latest {
        Some(latest) if latest.fetched.elapsed() > WEATHER_STALE_AFTER => {
            Some(format!("(old) {}", latest.report))
        }
        Some(latest) => Some(latest.report.clone()),
        None if failures > 0 => Some("Weather unavailable".into()),
        None => None,
    }
}

/// The wait after the `failures`-th failure in a row: doubles every time up to
/// [`WEATHER_RETRY_MAX`].
fn retry_delay(failures: u32) -> Duration {
    let doublings = failures.saturating_sub(1).min(8);
    WEATHER_RETRY
        .saturating_mul(1 << doublings)
        .min(WEATHER_RETRY_MAX)
}

fn run(status: &SharedStatus, message: &SharedMessage) {
    let client = Client::new();
    let mut locator = Locator::new();
    let mut latest: Option<Latest> = None;
    let mut failures: u32 = 0;
    let mut next_attempt = Instant::now();
    let mut was_online = false;

    loop {
        let online = status.snapshot().is_online();
        if online && !was_online {
            // The network is up: ask for the time now, and for the place again, because this
            // may be another network.
            clock::resync();
            locator.invalidate();
        }
        was_online = online;

        if online {
            // A new place needs its own forecast now, not at the next refresh.
            if locator.refresh_if_due() {
                next_attempt = Instant::now();
            }
            // An update that came due while the network was down runs as soon as it is back.
            if Instant::now() >= next_attempt {
                let fetched = locator
                    .current()
                    .ok_or(Error::NoLocation)
                    .and_then(|location| fetch(&client, location));
                match fetched {
                    Ok(report) => {
                        log::info!("weather: {report}");
                        failures = 0;
                        latest = Some(Latest {
                            report,
                            fetched: Instant::now(),
                        });
                        next_attempt = Instant::now() + WEATHER_REFRESH;
                    }
                    Err(error) => {
                        failures = failures.saturating_add(1);
                        let wait = retry_delay(failures);
                        log::warn!(
                            "weather update failed (attempt {failures}): {error}; retrying in {}s",
                            wait.as_secs()
                        );
                        next_attempt = Instant::now() + wait;
                    }
                }
            }
        }

        // Every tick, so a report turns "old" on time even while no update is possible.
        if let Some(text) = display_text(latest.as_ref(), failures) {
            message.set(text);
        }
        thread::sleep(TICK);
    }
}
