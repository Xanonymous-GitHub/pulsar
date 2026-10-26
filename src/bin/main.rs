#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]

use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::mono_font::ascii::FONT_10X20;
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;
use esp_hal::clock::CpuClock;
use esp_hal::gpio::{Level, Output, OutputConfig};
use esp_hal::i2c::master::{Config as I2cConfig, I2c};
use esp_hal::main;
use esp_hal::time::{Duration, Instant, Rate};
use log::{error, info};
use pulsar::scroll::ScrollingText;
use ssd1306::mode::DisplayConfig;
use ssd1306::prelude::*;
use ssd1306::{I2CDisplayInterface, Ssd1306};

/// The text shown on the OLED. Only ASCII is supported by the built-in font.
const SCROLL_TEXT: &str = "Hello from ESP32-C5!";
/// How often the LED toggles.
const LED_PERIOD: Duration = Duration::from_millis(100);
/// How often the text moves (and the screen is redrawn).
const SCROLL_PERIOD: Duration = Duration::from_millis(40);
/// Pixels the text moves on every scroll tick.
const SCROLL_STEP: i32 = 2;

#[panic_handler]
fn panic(panic_info: &core::panic::PanicInfo) -> ! {
    error!("{}", panic_info);
    loop {}
}

// This creates a default app-descriptor required by the esp-idf bootloader.
// For more information see: <https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-reference/system/app_image_format.html#application-description>
esp_bootloader_esp_idf::esp_app_desc!();

#[allow(
    clippy::large_stack_frames,
    reason = "it's not unusual to allocate larger buffers etc. in main"
)]
#[main]
fn main() -> ! {
    // generator version: 1.4.0
    // generator parameters: -o esp32c5 -o stable-aarch64-apple-darwin -o vscode -o ci -o agents -o log

    esp_println::logger::init_logger_from_env();

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    let count = 10;

    for i in 0..count {
        info!("Hello world! (iteration {})", i);
        let delay_start = Instant::now();
        while delay_start.elapsed() < Duration::from_millis(500) {}
    }

    info!("Done, idling.");

    let mut led = Output::new(peripherals.GPIO27, Level::Low, OutputConfig::default());

    // The expansion board's OLED sits on the XIAO's I2C pins: D4 = SDA = GPIO23, D5 = SCL = GPIO24.
    let i2c = I2c::new(
        peripherals.I2C0,
        I2cConfig::default().with_frequency(Rate::from_khz(400)),
    )
    .unwrap()
    .with_sda(peripherals.GPIO23)
    .with_scl(peripherals.GPIO24);

    let interface = I2CDisplayInterface::new(i2c);
    let mut display = Ssd1306::new(interface, DisplaySize128x64, DisplayRotation::Rotate0)
        .into_buffered_graphics_mode();
    display.init().unwrap();
    info!("OLED initialized");

    let mut scroller = ScrollingText::new(
        SCROLL_TEXT,
        MonoTextStyle::new(&FONT_10X20, BinaryColor::On),
    )
    .with_step(SCROLL_STEP);

    // Two independent deadlines instead of a blocking delay, so the LED keeps
    // blinking at its own pace while the screen scrolls at another.
    let mut next_led = Instant::now();
    let mut next_scroll = Instant::now();

    loop {
        let now = Instant::now();

        if now >= next_led {
            led.toggle();
            next_led += LED_PERIOD;
        }

        if now >= next_scroll {
            display.clear(BinaryColor::Off).ok();
            scroller.draw(&mut display).ok();
            if let Err(e) = display.flush() {
                error!("display flush failed: {:?}", e);
            }
            scroller.advance();
            next_scroll += SCROLL_PERIOD;
        }
    }

    // for inspiration have a look at the examples at https://github.com/esp-rs/esp-hal/tree/esp-hal-v1.2.2/examples
}
