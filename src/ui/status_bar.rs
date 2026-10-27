//! The status bar: two lines of text (network and clock, signal) and signal bars.
//!
//! Like [`ScrollingText`](super::scroll::ScrollingText) it draws onto any [`DrawTarget`], so it
//! knows nothing about the display hardware.

use core::fmt::Write as _;

use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::mono_font::ascii::FONT_6X10;
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{PrimitiveStyle, Rectangle};
use embedded_graphics::text::{Baseline, Text};

use crate::status::{Band, LinkState, WifiStatus, signal_bars};

/// Vertical distance between the two text lines.
const LINE_HEIGHT: i32 = 12;
/// Characters that fit on one line (128 px wide screen, 6 px per character).
const LINE_CHARS: usize = 21;
const BAR_COUNT: u8 = 4;
const BAR_WIDTH: u32 = 3;
const BAR_GAP: u32 = 2;
/// Height of the smallest bar; each further bar is this much taller.
const BAR_STEP: u32 = 2;

/// Characters of the clock text (`HH:MM:SS`).
const CLOCK_CHARS: usize = 8;

/// Draws a [`WifiStatus`] and the time into the area it is given.
pub struct StatusBar {
    text: MonoTextStyle<'static, BinaryColor>,
}

impl StatusBar {
    /// A status bar in the small 6x10 font.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            text: MonoTextStyle::new(&FONT_6X10, BinaryColor::On),
        }
    }

    /// Draws `status` onto `target`, starting at its top left corner. `time` is the local time
    /// of day (`HH:MM:SS`) shown at the right of the first line, or `None` until the clock is set.
    ///
    /// # Errors
    ///
    /// Returns the error of `target` if drawing fails.
    pub fn draw<D>(
        &self,
        status: &WifiStatus,
        time: Option<&str>,
        target: &mut D,
    ) -> Result<(), D::Error>
    where
        D: DrawTarget<Color = BinaryColor>,
    {
        let mut line = heapless::String::<LINE_CHARS>::new();

        // Line 1: the network name, or a placeholder until it is known. With a clock the name
        // gets less room (the clock plus one space is left out).
        let name_chars = if time.is_some() {
            LINE_CHARS - CLOCK_CHARS - 1
        } else {
            LINE_CHARS
        };
        if status.ssid.is_empty() {
            let _ = line.push_str("Wi-Fi");
        } else {
            // Names longer than the room are cut (by characters, not bytes).
            for character in status.ssid.chars().take(name_chars) {
                let _ = line.push(character);
            }
        }
        self.print(&line, 0, target)?;
        if let Some(time) = time {
            self.print_right(time, 0, target)?;
        }

        // Line 2: signal and band, or what the link is doing.
        line.clear();
        match status.state {
            LinkState::Idle => {
                let _ = line.push_str("Off");
            }
            LinkState::NoCredentials => {
                let _ = line.push_str("No credentials");
            }
            LinkState::Connecting => {
                let _ = line.push_str("Connecting...");
            }
            LinkState::Failed => {
                let _ = line.push_str("Wi-Fi failed");
            }
            LinkState::Connected => {
                let _ = match status.rssi_dbm {
                    Some(rssi) => write!(line, "{rssi} dBm"),
                    None => write!(line, "-- dBm"),
                };
                match status.band {
                    Some(Band::Ghz2_4) => {
                        let _ = line.push_str(" 2.4G");
                    }
                    Some(Band::Ghz5) => {
                        let _ = line.push_str(" 5G");
                    }
                    None => {}
                }
            }
        }
        self.print(&line, LINE_HEIGHT, target)?;

        if status.state == LinkState::Connected {
            draw_bars(status.rssi_dbm.map_or(0, signal_bars), target)?;
        }
        Ok(())
    }

    /// Prints `text` so that it ends at the right edge of `target`.
    fn print_right<D>(&self, text: &str, y: i32, target: &mut D) -> Result<(), D::Error>
    where
        D: DrawTarget<Color = BinaryColor>,
    {
        let width = i32::try_from(text.chars().count())
            .unwrap_or(0)
            .saturating_mul(self.text.font.character_size.width.cast_signed());
        let right = target.bounding_box().size.width.cast_signed();
        Text::with_baseline(text, Point::new(right - width, y), self.text, Baseline::Top)
            .draw(target)
            .map(drop)
    }

    fn print<D>(&self, text: &str, y: i32, target: &mut D) -> Result<(), D::Error>
    where
        D: DrawTarget<Color = BinaryColor>,
    {
        Text::with_baseline(text, Point::new(0, y), self.text, Baseline::Top)
            .draw(target)
            .map(drop)
    }
}

impl Default for StatusBar {
    fn default() -> Self {
        Self::new()
    }
}

/// Draws `lit` of [`BAR_COUNT`] growing bars at the right edge of the second line.
/// Bars that are not lit are shown as a single pixel row so the scale stays visible.
fn draw_bars<D>(lit: u8, target: &mut D) -> Result<(), D::Error>
where
    D: DrawTarget<Color = BinaryColor>,
{
    let area = target.bounding_box();
    let total_width = u32::from(BAR_COUNT) * (BAR_WIDTH + BAR_GAP) - BAR_GAP;
    let bottom = 2 * LINE_HEIGHT - 3;
    let mut left = area.top_left.x + (area.size.width - total_width).cast_signed();

    for index in 0..BAR_COUNT {
        let full_height = BAR_STEP * (u32::from(index) + 2);
        let height = if index < lit { full_height } else { 1 };
        Rectangle::new(
            Point::new(left, bottom - height.cast_signed() + 1),
            Size::new(BAR_WIDTH, height),
        )
        .into_styled(PrimitiveStyle::with_fill(BinaryColor::On))
        .draw(target)?;
        left += (BAR_WIDTH + BAR_GAP).cast_signed();
    }
    Ok(())
}
