//! The status bar: two lines. The first has the network name (or what the link is doing) and,
//! while connected, a band badge and signal bars at its right. The second has the local time and,
//! at its right, the Bluetooth icon (with the passkey next to it while a phone is pairing).
//!
//! The Bluetooth icon follows two rules. **Filled** means a phone is connected, **hollow** means
//! none is. **Steady** means the phone is paired (bonded and encrypted), **blinking** means it is
//! not, yet. A hollow frame with a cross means Bluetooth failed to start, and no icon means it is
//! off.
//!
//! Like [`ScrollingText`](super::scroll::ScrollingText) it draws onto any [`DrawTarget`], so it
//! knows nothing about the display hardware.

use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::mono_font::ascii::{FONT_4X6, FONT_6X10};
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{PrimitiveStyle, Rectangle};
use embedded_graphics::text::{Baseline, Text};

use crate::status::{Band, BtState, BtStatus, LinkState, SIGNAL_LEVELS, WifiStatus, signal_bars};

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

/// A passkey has six digits (leading zeros included).
const PASSKEY_DIGITS: u32 = 6;
/// Width of the passkey in the status bar's font.
const PASSKEY_WIDTH: i32 = (PASSKEY_DIGITS * FONT_6X10.character_size.width).cast_signed();

const BT_ICON_WIDTH: u32 = 9;
const BT_ICON_HEIGHT: u32 = 11;
/// The Bluetooth rune, 5 by 9 pixels (`#` is a lit pixel).
const BT_RUNE: [&str; 9] = [
    "..#..", "..##.", "#.#.#", ".###.", "..#..", ".###.", "#.#.#", "..##.", "..#..",
];
/// A cross, 5 by 5 pixels, shown instead of the rune when Bluetooth failed.
const BT_CROSS: [&str; 5] = ["#...#", ".#.#.", "..#..", ".#.#.", "#...#"];

/// How the Bluetooth icon looks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BtLook {
    /// A filled box with a dark rune: a phone is connected.
    Filled,
    /// A frame with a light rune: no phone is connected.
    Hollow,
    /// A frame with a cross: Bluetooth could not start.
    Failed,
}

/// The look of the icon for `state`, and whether it blinks. `None` means no icon.
const fn bluetooth_icon(state: BtState) -> Option<(BtLook, bool)> {
    match state {
        BtState::Off => None,
        BtState::Failed => Some((BtLook::Failed, false)),
        BtState::Waiting { bonded } => Some((BtLook::Hollow, !bonded)),
        BtState::Connected { secured } => Some((BtLook::Filled, !secured)),
    }
}

/// Draws a [`WifiStatus`], a [`BtStatus`] and the time into the area it is given.
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

    /// Draws `status` and `bluetooth` onto `target`, starting at its top left corner. `time` is
    /// the local time of day (`HH:MM:SS`) shown at the left of the second line, or `None` until
    /// the clock is set. `blink_on` is the phase of the blinking Bluetooth icon: it is drawn
    /// while true and skipped while false (a steady icon ignores it).
    ///
    /// # Errors
    ///
    /// Returns the error of `target` if drawing fails.
    pub fn draw<D>(
        &self,
        status: &WifiStatus,
        bluetooth: &BtStatus,
        time: Option<&str>,
        blink_on: bool,
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

        // Line 2: the time at the left, the Bluetooth icon (and a passkey) at the right.
        if let Some(time) = time {
            self.print(time, LINE_HEIGHT, target)?;
        }
        let right = {
            let area = target.bounding_box();
            area.top_left.x + area.size.width.cast_signed()
        };
        let icon_left = right - BT_ICON_WIDTH.cast_signed();
        if let Some((look, blinking)) = bluetooth_icon(bluetooth.state)
            && (blink_on || !blinking)
        {
            draw_bluetooth(look, icon_left, target)?;
        }
        if let Some(passkey) = bluetooth.passkey {
            self.print_at(
                &format!("{passkey:06}"),
                Point::new(icon_left - ICON_GAP - PASSKEY_WIDTH, LINE_HEIGHT),
                target,
            )?;
        }
        Ok(())
    }

    fn print<D>(&self, text: &str, y: i32, target: &mut D) -> Result<(), D::Error>
    where
        D: DrawTarget<Color = BinaryColor>,
    {
        self.print_at(text, Point::new(0, y), target)
    }

    fn print_at<D>(&self, text: &str, at: Point, target: &mut D) -> Result<(), D::Error>
    where
        D: DrawTarget<Color = BinaryColor>,
    {
        Text::with_baseline(text, at, self.text, Baseline::Top)
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

/// Draws the Bluetooth icon with its left edge at `left`, on the second line.
fn draw_bluetooth<D>(look: BtLook, left: i32, target: &mut D) -> Result<(), D::Error>
where
    D: DrawTarget<Color = BinaryColor>,
{
    let top_left = Point::new(left, LINE_HEIGHT);
    let size = Size::new(BT_ICON_WIDTH, BT_ICON_HEIGHT);
    let (box_style, glyph_color) = match look {
        BtLook::Filled => (PrimitiveStyle::with_fill(BinaryColor::On), BinaryColor::Off),
        BtLook::Hollow | BtLook::Failed => (
            PrimitiveStyle::with_stroke(BinaryColor::On, 1),
            BinaryColor::On,
        ),
    };
    Rectangle::new(top_left, size)
        .into_styled(box_style)
        .draw(target)?;

    // Take the four corner pixels away, which rounds the box.
    let last = Point::new(size.width.cast_signed() - 1, size.height.cast_signed() - 1);
    target.draw_iter(
        [
            Point::zero(),
            Point::new(last.x, 0),
            Point::new(0, last.y),
            last,
        ]
        .map(|corner| Pixel(top_left + corner, BinaryColor::Off)),
    )?;

    let (glyph, glyph_width): (&[&str], i32) = match look {
        BtLook::Failed => (&BT_CROSS, 5),
        BtLook::Filled | BtLook::Hollow => (&BT_RUNE, 5),
    };
    let glyph_height = i32::try_from(glyph.len()).unwrap_or(0);
    // Centred in the box.
    let origin = top_left
        + Point::new(
            (size.width.cast_signed() - glyph_width) / 2,
            (size.height.cast_signed() - glyph_height) / 2,
        );
    target.draw_iter(
        glyph
            .iter()
            .zip(0..)
            .flat_map(|(row, y)| row.bytes().zip(0..).map(move |(pixel, x)| (pixel, x, y)))
            .filter(|&(pixel, ..)| pixel == b'#')
            .map(|(_, x, y)| Pixel(origin + Point::new(x, y), glyph_color)),
    )
}
