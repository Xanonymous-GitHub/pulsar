#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]

use embassy_executor::Spawner;
use embassy_time::{Duration, Ticker, Timer};
use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::mono_font::ascii::FONT_10X20;
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;
use esp_hal::Blocking;
use esp_hal::clock::CpuClock;
use esp_hal::gpio::{Level, Output, OutputConfig};
use esp_hal::i2c::master::{Config as I2cConfig, I2c};
use esp_hal::main;
use esp_hal::peripherals::WIFI;
use esp_hal::time::Rate;
use esp_hal::timer::timg::TimerGroup;
use esp_storage::FlashStorage;
use log::{error, info};
use pulsar::scroll::ScrollingText;
use pulsar::secret_store::EncryptedFlashStore;
use pulsar::wifi::{self, LinkState, WIFI_STATUS, WifiLink, WifiStatus};
use ssd1306::mode::{BufferedGraphicsMode, DisplayConfig};
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
/// Size of the heap that Wi-Fi (and anything else that allocates) can use.
const HEAP_SIZE: usize = 72 * 1024;

/// The 128x64 SSD1306 OLED on the expansion board, driven over I2C.
type Display = Ssd1306<
    I2CInterface<I2c<'static, Blocking>>,
    DisplaySize128x64,
    BufferedGraphicsMode<DisplaySize128x64>,
>;

#[panic_handler]
fn panic(panic_info: &core::panic::PanicInfo) -> ! {
    error!("{}", panic_info);
    loop {}
}

// This creates a default app-descriptor required by the esp-idf bootloader.
// For more information see: <https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-reference/system/app_image_format.html#application-description>
esp_bootloader_esp_idf::esp_app_desc!();

/// Opens the secret store, gets the credentials from it (or from the build environment) and
/// starts joining the network. Returns as soon as the connection attempt is under way.
fn start_wifi(
    flash: FlashStorage<'static>,
    wifi_peripheral: WIFI<'static>,
    spawner: Spawner,
) -> Result<WifiLink, wifi::Error> {
    let mut store = EncryptedFlashStore::open(flash)?;
    let credentials = wifi::load_or_provision(&mut store)?;
    WifiLink::start(wifi_peripheral, &credentials, spawner)
}

/// Blinks the user LED. The ticker keeps a fixed rhythm even if one tick runs late.
#[embassy_executor::task]
async fn led_task(mut led: Output<'static>) {
    let mut ticker = Ticker::every(LED_PERIOD);
    loop {
        led.toggle();
        ticker.next().await;
    }
}

/// Redraws the OLED and moves the scrolling text.
#[embassy_executor::task]
async fn display_task(mut display: Display) {
    let mut scroller = ScrollingText::new(
        SCROLL_TEXT,
        MonoTextStyle::new(&FONT_10X20, BinaryColor::On),
    )
    .with_step(SCROLL_STEP);

    let mut ticker = Ticker::every(SCROLL_PERIOD);
    loop {
        display.clear(BinaryColor::Off).ok();
        scroller.draw(&mut display).ok();
        if let Err(e) = display.flush() {
            error!("display flush failed: {:?}", e);
        }
        scroller.advance();
        ticker.next().await;
    }
}

#[allow(
    clippy::large_stack_frames,
    reason = "it's not unusual to allocate larger buffers etc. in main"
)]
#[main]
async fn main(spawner: Spawner) {
    // generator version: 1.4.0
    // generator parameters: -o esp32c5 -o stable-aarch64-apple-darwin -o vscode -o ci -o agents -o log

    esp_println::logger::init_logger_from_env();

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    // Wi-Fi needs a heap and the preemptive scheduler. The scheduler must be
    // running before any radio is created.
    esp_alloc::heap_allocator!(size: HEAP_SIZE);
    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

    let wifi_link = match start_wifi(
        FlashStorage::new(peripherals.FLASH),
        peripherals.WIFI,
        spawner,
    ) {
        Ok(link) => Some(link),
        Err(e) => {
            error!("Wi-Fi not started: {}", e);
            WIFI_STATUS
                .sender()
                .send(WifiStatus::with_state(LinkState::NoCredentials));
            None
        }
    };

    let count = 10;

    for i in 0..count {
        info!("Hello world! (iteration {})", i);
        Timer::after(Duration::from_millis(500)).await;
    }

    info!("Done, idling.");

    let led = Output::new(peripherals.GPIO27, Level::Low, OutputConfig::default());

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

    spawner.spawn(led_task(led).unwrap());
    spawner.spawn(display_task(display).unwrap());

    if let Some(link) = wifi_link {
        let ip = link.wait_for_ip().await;
        info!("Wi-Fi connected, IP address {}", ip);
    }

    // for inspiration have a look at the examples at https://github.com/esp-rs/esp-hal/tree/esp-hal-v1.2.2/examples
}
