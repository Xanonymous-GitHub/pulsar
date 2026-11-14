//! Coordinates and place names: the checks and clean-up applied to what a location service says.
//!
//! This file imports nothing from the crate, so it is tested on the host:
//!
//! ```sh
//! rustc --edition 2024 --test src/location/place.rs -o /tmp/t && /tmp/t
//! ```

/// The longest place name kept, in characters. The name scrolls with the weather text, and a
/// service must not be able to fill the screen with it.
pub const MAX_PLACE_CHARS: usize = 24;

/// A point on the globe, rounded to a hundredth of a degree (about one kilometre).
///
/// The address of a network only tells the city, so more digits would be false precision, and
/// the rounded value is all that is sent on to the weather service.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Coordinates {
    /// Hundredths of a degree north of the equator (negative: south), -9000 to 9000.
    latitude_centi: i16,
    /// Hundredths of a degree east of Greenwich (negative: west), -18000 to 18000.
    longitude_centi: i16,
}

impl Coordinates {
    /// Checks and rounds a pair of degrees. Returns `None` for values that are not on the globe
    /// (or not numbers), and for exactly 0, 0: no network is there (it is in the Atlantic), and
    /// services use it to say "unknown".
    #[must_use]
    pub fn new(latitude: f64, longitude: f64) -> Option<Self> {
        let on_globe = (-90.0..=90.0).contains(&latitude) && (-180.0..=180.0).contains(&longitude);
        if !on_globe || (latitude == 0.0 && longitude == 0.0) {
            return None;
        }
        Some(Self {
            latitude_centi: centi(latitude)?,
            longitude_centi: centi(longitude)?,
        })
    }

    /// Degrees north, as text with two decimals (for example `25.05` or `-33.87`).
    #[must_use]
    pub fn latitude(self) -> String {
        degrees(self.latitude_centi)
    }

    /// Degrees east, as text with two decimals (for example `121.53` or `-0.12`).
    #[must_use]
    pub fn longitude(self) -> String {
        degrees(self.longitude_centi)
    }
}

/// `value` degrees as hundredths of a degree, rounded to the nearest.
fn centi(value: f64) -> Option<i16> {
    let hundredths = (value * 100.0).round();
    // The range check keeps the conversion exact: the value is a whole number that fits.
    (f64::from(i16::MIN)..=f64::from(i16::MAX))
        .contains(&hundredths)
        .then(|| {
            #[allow(
                clippy::cast_possible_truncation,
                reason = "checked to be a whole number inside the range of i16"
            )]
            let hundredths = hundredths as i16;
            hundredths
        })
}

/// Hundredths of a degree as decimal text, without going through floating point.
fn degrees(centi: i16) -> String {
    let sign = if centi < 0 { "-" } else { "" };
    let magnitude = centi.unsigned_abs();
    format!("{sign}{}.{:02}", magnitude / 100, magnitude % 100)
}

/// Turns a place name from a service into text the display font can show: ASCII only, at most
/// [`MAX_PLACE_CHARS`] characters, no control characters. Common accented Latin letters lose
/// their accent (`São Paulo` becomes `Sao Paulo`); anything else outside ASCII becomes `?`.
/// Returns `None` when nothing readable is left.
#[must_use]
pub fn ascii_place(name: &str) -> Option<String> {
    let mut place = String::new();
    for character in name.trim().chars() {
        if place.chars().count() >= MAX_PLACE_CHARS {
            break;
        }
        match character {
            ' '..='~' => place.push(character),
            _ if character.is_control() => {}
            _ => place.push_str(fold(character).unwrap_or("?")),
        }
    }
    let place = place.trim();
    let readable = place
        .chars()
        .any(|character| character.is_ascii_alphanumeric());
    readable.then(|| place.chars().take(MAX_PLACE_CHARS).collect())
}

/// The ASCII spelling of an accented Latin letter, if it has an obvious one.
const fn fold(character: char) -> Option<&'static str> {
    Some(match character {
        'À'..='Å' | 'Ā' | 'Ă' | 'Ą' => "A",
        'à'..='å' | 'ā' | 'ă' | 'ą' => "a",
        'Æ' => "AE",
        'æ' => "ae",
        'Ç' | 'Ć' | 'Č' => "C",
        'ç' | 'ć' | 'č' => "c",
        'Ď' | 'Đ' => "D",
        'ď' | 'đ' => "d",
        'È'..='Ë' | 'Ē' | 'Ę' | 'Ě' => "E",
        'è'..='ë' | 'ē' | 'ę' | 'ě' => "e",
        'Ğ' => "G",
        'ğ' => "g",
        'Ì'..='Ï' | 'Ī' | 'İ' => "I",
        'ì'..='ï' | 'ī' | 'ı' => "i",
        'Ł' => "L",
        'ł' => "l",
        'Ñ' | 'Ń' | 'Ň' => "N",
        'ñ' | 'ń' | 'ň' => "n",
        'Ò'..='Ö' | 'Ø' | 'Ō' | 'Ő' => "O",
        'ò'..='ö' | 'ø' | 'ō' | 'ő' => "o",
        'Œ' => "OE",
        'œ' => "oe",
        'Ř' => "R",
        'ř' => "r",
        'Ś' | 'Ş' | 'Š' => "S",
        'ś' | 'ş' | 'š' => "s",
        'ß' => "ss",
        'Ť' | 'Ţ' => "T",
        'ť' | 'ţ' => "t",
        'Ù'..='Ü' | 'Ū' | 'Ů' | 'Ű' => "U",
        'ù'..='ü' | 'ū' | 'ů' | 'ű' => "u",
        'Ý' | 'Ÿ' => "Y",
        'ý' | 'ÿ' => "y",
        'Ź' | 'Ż' | 'Ž' => "Z",
        'ź' | 'ż' | 'ž' => "z",
        '’' | '‘' => "'",
        '–' | '—' => "-",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(latitude: f64, longitude: f64) -> Option<(String, String)> {
        Coordinates::new(latitude, longitude).map(|place| (place.latitude(), place.longitude()))
    }

    #[test]
    fn coordinates_are_rounded_to_two_decimals() {
        assert_eq!(
            text(25.053_056_5, 121.526_394_6),
            Some(("25.05".into(), "121.53".into()))
        );
        assert_eq!(
            text(-33.868_8, 151.209_3),
            Some(("-33.87".into(), "151.21".into()))
        );
        assert_eq!(text(51.5, -0.12), Some(("51.50".into(), "-0.12".into())));
        assert_eq!(text(-0.004, -0.004), Some(("0.00".into(), "0.00".into())));
    }

    #[test]
    fn the_edges_of_the_globe_are_accepted() {
        assert_eq!(text(90.0, 180.0), Some(("90.00".into(), "180.00".into())));
        assert_eq!(
            text(-90.0, -180.0),
            Some(("-90.00".into(), "-180.00".into()))
        );
    }

    #[test]
    fn values_off_the_globe_are_refused() {
        assert_eq!(text(90.01, 0.0), None);
        assert_eq!(text(0.0, 180.01), None);
        assert_eq!(text(-91.0, 10.0), None);
        assert_eq!(text(f64::NAN, 10.0), None);
        assert_eq!(text(10.0, f64::INFINITY), None);
    }

    #[test]
    fn null_island_means_unknown() {
        assert_eq!(text(0.0, 0.0), None);
        assert!(text(0.0, 0.01).is_some());
    }

    #[test]
    fn plain_names_pass() {
        assert_eq!(ascii_place("Taipei").as_deref(), Some("Taipei"));
        assert_eq!(ascii_place("  New York  ").as_deref(), Some("New York"));
        assert_eq!(ascii_place("Saint-Denis").as_deref(), Some("Saint-Denis"));
    }

    #[test]
    fn accents_are_folded() {
        assert_eq!(ascii_place("São Paulo").as_deref(), Some("Sao Paulo"));
        assert_eq!(ascii_place("Zürich").as_deref(), Some("Zurich"));
        assert_eq!(ascii_place("Kraków").as_deref(), Some("Krakow"));
        assert_eq!(ascii_place("Besançon").as_deref(), Some("Besancon"));
        assert_eq!(ascii_place("Straße").as_deref(), Some("Strasse"));
    }

    #[test]
    fn other_scripts_become_question_marks_or_nothing() {
        assert_eq!(ascii_place("台北 Taipei").as_deref(), Some("?? Taipei"));
        assert_eq!(ascii_place("台北"), None);
        assert_eq!(ascii_place(""), None);
        assert_eq!(ascii_place("   "), None);
        assert_eq!(ascii_place("?-?"), None);
    }

    #[test]
    fn control_characters_are_dropped_and_long_names_cut() {
        assert_eq!(ascii_place("Tai\npei\u{7}").as_deref(), Some("Taipei"));
        let long = ascii_place(&"Llanfairpwllgwyngyll".repeat(3));
        assert_eq!(long.as_deref(), Some("LlanfairpwllgwyngyllLlan"));
        assert_eq!(
            ascii_place(&"ß".repeat(40)).map(|place| place.len()),
            Some(MAX_PLACE_CHARS)
        );
    }
}
