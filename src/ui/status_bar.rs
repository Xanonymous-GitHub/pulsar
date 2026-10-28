//! The status bar: two lines. The first has the network name (or what the link is doing) and,
//! while connected, a band badge and signal bars at its right. The second has the local time;
//! its right side is free for more icons.
//!
//! Like [`ScrollingText`](super::scroll::ScrollingText) it draws onto any [`DrawTarget`], so it
//! knows nothing about the display hardware.

use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::mono_font::ascii::{FONT_4X6, FONT_6X10};
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{PrimitiveStyle, Rectangle};
use embedded_graphics::text::{Baseline, Text};

use crate::status::{Band, LinkState, SIGNAL_LEVELS, WifiStatus, signal_bars};

/// Vertical distance between the two text lines.
const LINE_HEIGHT: i32 = 12;
/// Space between the network name, the band badge and the signal bars.
const ICON_GAP: i32 = 3;

const BAR_WIDTH: u32 = 3;
const BAR_GAP: u32 = 1;
/// Height of the smallest bar; each further bar is this much taller.
const BAR_STEP: u32 = 2;
/// Width of the whole signal icon.
const BARS_WIDTH: u32 = SIGNAL_LEVELS as u32 * (BAR_WIDTH + BAR_GAP) - BAR_GAP;
/// Row of the lowest pixel of the first line's icons. The tallest bar then spans the 10 px of
/// the text next to it.
const ICON_BOTTOM: i32 = LINE_HEIGHT - 3;

/// Characters of the longest band label (`2.4G`); every badge is as wide as this one so the
/// room for the network name does not change with the band.
const BADGE_CHARS: u32 = 4;
/// Blank pixels around the label inside the badge.
const BADGE_PADDING: u32 = 1;
const BADGE_WIDTH: u32 = BADGE_CHARS * FONT_4X6.character_size.width + 2 * BADGE_PADDING;
const BADGE_HEIGHT: u32 = FONT_4X6.character_size.height + 2 * BADGE_PADDING;

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
    /// of day (`HH:MM:SS`) shown at the left of the second line, or `None` until the clock is set.
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
        // Line 1: what the link is doing, or, once connected, the network name with the icons.
        if let Some(text) = idle_text(status.state) {
            self.print(text, 0, target)?;
        } else {
            let area = target.bounding_box();
            let right = area.top_left.x + area.size.width.cast_signed();
            let bars_left = draw_bars(status.rssi_dbm.map_or(0, signal_bars), right, target)?;
            let badge_left = draw_badge(status.band, bars_left - ICON_GAP, target)?;

            // The name gets what the icons leave, cut by characters (not bytes).
            let room = badge_left - ICON_GAP;
            let name_chars = usize::try_from(room.max(0).cast_unsigned()).unwrap_or(0)
                / usize::try_from(self.text.font.character_size.width).unwrap_or(1);
            let name = if status.ssid.is_empty() {
                "Wi-Fi"
            } else {
                &status.ssid
            };
            self.print(cut(name, name_chars), 0, target)?;
        }

        // Line 2: the time.
        if let Some(time) = time {
            self.print(time, LINE_HEIGHT, target)?;
        }
        Ok(())
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

/// What to show in place of the network name while there is no link, or `None` when connected.
const fn idle_text(state: LinkState) -> Option<&'static str> {
    match state {
        LinkState::Idle => Some("Off"),
        LinkState::NoCredentials => Some("No credentials"),
        LinkState::Connecting => Some("Connecting..."),
        LinkState::Failed => Some("Wi-Fi failed"),
        LinkState::Connected => None,
    }
}

/// The first `chars` characters of `text`.
fn cut(text: &str, chars: usize) -> &str {
    let end = text
        .char_indices()
        .nth(chars)
        .map_or(text.len(), |(index, _)| index);
    &text[..end]
}

/// Draws `lit` of [`SIGNAL_LEVELS`] growing bars with their right end at `right`. Bars that are
/// not lit are shown as a single pixel row so the scale stays visible.
///
/// Returns the x position of the left end of the icon.
fn draw_bars<D>(lit: u8, right: i32, target: &mut D) -> Result<i32, D::Error>
where
    D: DrawTarget<Color = BinaryColor>,
{
    let icon_left = right - BARS_WIDTH.cast_signed();
    let mut left = icon_left;

    for index in 0..SIGNAL_LEVELS {
        let full_height = BAR_STEP * (u32::from(index) + 1);
        let height = if index < lit { full_height } else { 1 };
        Rectangle::new(
            Point::new(left, ICON_BOTTOM - height.cast_signed() + 1),
            Size::new(BAR_WIDTH, height),
        )
        .into_styled(PrimitiveStyle::with_fill(BinaryColor::On))
        .draw(target)?;
        left += (BAR_WIDTH + BAR_GAP).cast_signed();
    }
    Ok(icon_left)
}

/// Draws the band as a light badge with dark text, ending at `right`. Nothing is drawn while the
/// band is unknown, but the room is still used.
///
/// Returns the x position of the left end of the badge.
fn draw_badge<D>(band: Option<Band>, right: i32, target: &mut D) -> Result<i32, D::Error>
where
    D: DrawTarget<Color = BinaryColor>,
{
    let left = right - BADGE_WIDTH.cast_signed();
    let Some(band) = band else {
        return Ok(left);
    };
    let label = match band {
        Band::Ghz2_4 => "2.4G",
        Band::Ghz5 => "5G",
    };
    let top = ICON_BOTTOM - BADGE_HEIGHT.cast_signed() + 1;

    Rectangle::new(Point::new(left, top), Size::new(BADGE_WIDTH, BADGE_HEIGHT))
        .into_styled(PrimitiveStyle::with_fill(BinaryColor::On))
        .draw(target)?;

    // Centre the label (shorter than `BADGE_CHARS` for 5 GHz) in the badge.
    let label_width = i32::try_from(label.len())
        .unwrap_or(0)
        .saturating_mul(FONT_4X6.character_size.width.cast_signed());
    let label_left = left + (BADGE_WIDTH.cast_signed() - label_width) / 2;
    Text::with_baseline(
        label,
        Point::new(label_left, top + BADGE_PADDING.cast_signed()),
        MonoTextStyle::new(&FONT_4X6, BinaryColor::Off),
        Baseline::Top,
    )
    .draw(target)?;
    Ok(left)
}
