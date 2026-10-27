//! A horizontal "marquee": a string that scrolls from right to left and loops.
//!
//! This module knows nothing about the hardware. It draws onto anything that
//! implements [`DrawTarget`] with [`BinaryColor`] (an SSD1306 OLED, an
//! in-memory buffer, ...). It also does not keep time: call [`advance`] whenever
//! you want the text to move by one step, then [`draw`] to render it.
//!
//! The text is repeated on an endless "tape", `gap` pixels apart, so there is
//! never a jump when it wraps around, even if the text is shorter than the screen.
//!
//! Only monospace fonts are supported, and the built-in `embedded-graphics`
//! ASCII fonts can only draw ASCII (other characters show up as `?`).
//!
//! [`advance`]: ScrollingText::advance
//! [`draw`]: ScrollingText::draw

use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;
use embedded_graphics::text::{Baseline, Text};

/// Default number of blank pixels between two repeats of the text.
const DEFAULT_GAP: i32 = 16;

/// A string that scrolls horizontally, vertically centered on the draw target.
pub struct ScrollingText<'a> {
    text: &'a str,
    style: MonoTextStyle<'static, BinaryColor>,
    /// How far the tape has moved to the left, always in `0..period`.
    offset: i32,
    /// Pixels moved per call to `advance`.
    step: i32,
    /// Blank pixels between two repeats of the text.
    gap: i32,
}

impl<'a> ScrollingText<'a> {
    /// Creates a scroller that moves 1 pixel per `advance` with a 16 pixel gap.
    #[must_use]
    pub const fn new(text: &'a str, style: MonoTextStyle<'static, BinaryColor>) -> Self {
        Self {
            text,
            style,
            offset: 0,
            step: 1,
            gap: DEFAULT_GAP,
        }
    }

    /// Sets how many pixels the text moves on each `advance`.
    /// A negative value scrolls the other way.
    #[must_use]
    pub const fn with_step(mut self, step: i32) -> Self {
        self.step = step;
        self
    }

    /// Sets the number of blank pixels between two repeats of the text.
    #[must_use]
    pub const fn with_gap(mut self, gap: i32) -> Self {
        self.gap = gap;
        self
    }

    /// Replaces the text and restarts the scroll from the beginning.
    pub fn set_text(&mut self, text: &'a str) {
        self.text = text;
        self.offset = 0;
    }

    /// Width of one copy of the text, in pixels.
    #[must_use]
    pub fn text_width(&self) -> i32 {
        let font = self.style.font;
        let glyph_width = (font.character_size.width + font.character_spacing).cast_signed();
        let glyphs = i32::try_from(self.text.chars().count()).unwrap_or(i32::MAX);
        glyphs.saturating_mul(glyph_width)
    }

    /// Distance between the start of one copy of the text and the next.
    /// At least 1, so the arithmetic below never divides by zero.
    fn period(&self) -> i32 {
        (self.text_width() + self.gap).max(1)
    }

    /// Moves the text by one step and wraps around when a full period has passed.
    pub fn advance(&mut self) {
        self.offset = (self.offset + self.step).rem_euclid(self.period());
    }

    /// Draws the text (as many repeats as fit) onto `target`.
    ///
    /// The caller is responsible for clearing the target first and for
    /// flushing it afterwards if it is buffered.
    ///
    /// # Errors
    ///
    /// Returns the error of `target` if drawing fails.
    pub fn draw<D>(&self, target: &mut D) -> Result<(), D::Error>
    where
        D: DrawTarget<Color = BinaryColor>,
    {
        if self.text.is_empty() {
            return Ok(());
        }

        let bounds = target.bounding_box();
        let left = bounds.top_left.x;
        let right = left + bounds.size.width.cast_signed();
        let y = bounds.center().y;
        let period = self.period();

        // The first copy starts `offset` pixels to the left of the screen edge.
        // The copy before it ends before the edge, so it never needs drawing.
        let mut x = left - self.offset;
        while x < right {
            Text::with_baseline(self.text, Point::new(x, y), self.style, Baseline::Middle)
                .draw(target)?;
            x += period;
        }

        Ok(())
    }
}
