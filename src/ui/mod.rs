//! The OLED: a status bar (Wi-Fi and the local time) on top and a scrolling message below.
//!
//! [`Screen`] owns the display and a thread that redraws it at a fixed rate. It reads the Wi-Fi
//! state from a [`SharedStatus`], the scrolling text from a [`SharedMessage`] and the time from
//! [`clock`]; it never talks to the network code directly.

pub mod scroll;
pub mod status_bar;

use std::io;
use std::thread::{self, JoinHandle};
use std::time::Instant;

use display_interface::DisplayError;
use embedded_graphics::geometry::AnchorY;
use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::mono_font::ascii::FONT_10X20;
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::Rectangle;
use esp_idf_svc::hal::i2c::I2cDriver;
use ssd1306::mode::{BufferedGraphicsMode, DisplayConfig};
use ssd1306::prelude::*;
use ssd1306::{I2CDisplayInterface, Ssd1306};

use crate::clock;
use crate::config::{FRAME_PERIOD, SCREEN_STACK_BYTES, SCROLL_STEP, STATUS_BAR_HEIGHT};
use crate::message::SharedMessage;
use crate::status::SharedStatus;
use scroll::ScrollingText;
use status_bar::StatusBar;

/// The 128x64 SSD1306 OLED of the expansion board, driven over I2C.
type Display = Ssd1306<
    I2CInterface<I2cDriver<'static>>,
    DisplaySize128x64,
    BufferedGraphicsMode<DisplaySize128x64>,
>;

/// What can go wrong with the screen.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The display did not accept its start-up commands (wrong wiring or address?).
    #[error("display error: {0:?}")]
    Display(DisplayError),
    /// The screen thread could not be created.
    #[error("cannot start the screen thread: {0}")]
    Thread(#[from] io::Error),
}

impl From<DisplayError> for Error {
    fn from(error: DisplayError) -> Self {
        Self::Display(error)
    }
}

/// The OLED and everything drawn on it.
pub struct Screen {
    display: Display,
    bar: StatusBar,
    scroller: ScrollingText,
    status: SharedStatus,
    message: SharedMessage,
    /// The generation of the message that is on the screen, see [`SharedMessage::take_update`].
    message_generation: u64,
}

impl Screen {
    /// Initialises the display on `i2c`.
    ///
    /// # Errors
    ///
    /// Fails if the display does not answer.
    pub fn new(
        i2c: I2cDriver<'static>,
        status: SharedStatus,
        message: SharedMessage,
    ) -> Result<Self, Error> {
        let mut display = Ssd1306::new(
            I2CDisplayInterface::new(i2c),
            DisplaySize128x64,
            DisplayRotation::Rotate0,
        )
        .into_buffered_graphics_mode();
        display.init()?;

        Ok(Self {
            display,
            bar: StatusBar::new(),
            // The text itself arrives with the first frame.
            scroller: ScrollingText::new("", MonoTextStyle::new(&FONT_10X20, BinaryColor::On))
                .with_step(SCROLL_STEP),
            status,
            message,
            message_generation: 0,
        })
    }

    /// Moves the screen to its own thread, which redraws it forever.
    ///
    /// # Errors
    ///
    /// Fails if the thread cannot be created.
    pub fn spawn(self) -> Result<JoinHandle<()>, Error> {
        Ok(thread::Builder::new()
            .name("screen".into())
            .stack_size(SCREEN_STACK_BYTES)
            .spawn(move || self.run())?)
    }

    /// Draws a frame every [`FRAME_PERIOD`] until the display fails.
    fn run(mut self) {
        let mut next_frame = Instant::now();
        loop {
            if let Err(error) = self.draw_frame() {
                log::error!("display failed, stopping: {error:?}");
                return;
            }
            // Counting from the planned time (not from now) keeps the speed steady.
            next_frame += FRAME_PERIOD;
            thread::sleep(next_frame.saturating_duration_since(Instant::now()));
        }
    }

    fn draw_frame(&mut self) -> Result<(), DisplayError> {
        let area: Rectangle = self.display.bounding_box();
        let bar_area = area.resized_height(STATUS_BAR_HEIGHT, AnchorY::Top);
        let scroll_area = area.resized_height(
            area.size.height.saturating_sub(STATUS_BAR_HEIGHT),
            AnchorY::Bottom,
        );

        if let Some(text) = self.message.take_update(&mut self.message_generation) {
            self.scroller.set_text(text);
        }
        let time = clock::local_time().map(|time| time.to_string());

        self.display.clear(BinaryColor::Off)?;
        self.bar.draw(
            &self.status.snapshot(),
            time.as_deref(),
            &mut self.display.cropped(&bar_area),
        )?;
        self.scroller
            .draw(&mut self.display.cropped(&scroll_area))?;
        self.display.flush()?;

        self.scroller.advance();
        Ok(())
    }
}
