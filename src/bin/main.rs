#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]

use esp_hal::Blocking;
use esp_hal::clock::CpuClock;
use esp_hal::main;
use esp_hal::rmt::Rmt;
use esp_hal::time::{Duration, Instant, Rate};
use esp_hal_smartled::{RmtSmartLeds, WS2812_TIMING, buffer_size, color_order};
use log::{error, info};
use smart_leds::colors::{BLUE, GREEN, RED};
use smart_leds::{RGB8, SmartLedsWrite, brightness};

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

    let rmt = Rmt::new(peripherals.RMT, Rate::from_mhz(80)).unwrap();
    const LED_BUFFER_SIZE: usize = buffer_size::<RGB8>(1);
    let mut led = RmtSmartLeds::<LED_BUFFER_SIZE, Blocking, RGB8, color_order::Rgb>::new(
        WS2812_TIMING,
        rmt.channel0,
        peripherals.GPIO27,
        Rate::from_mhz(80),
    )
    .unwrap();

    let colors = [RED, GREEN, BLUE];
    let mut next = 0;

    loop {
        led.write(brightness([colors[next]].into_iter(), 10))
            .unwrap();
        next = (next + 1) % colors.len();
        let delay_start = Instant::now();
        while delay_start.elapsed() < Duration::from_millis(100) {}
    }

    // for inspiration have a look at the examples at https://github.com/esp-rs/esp-hal/tree/esp-hal-v1.2.2/examples
}
