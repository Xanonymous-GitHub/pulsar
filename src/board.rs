//! The XIAO ESP32-C5 and its expansion board: which pin does what.
//!
//! This is the only module that knows pin numbers, so moving to another board means editing
//! this file alone.

use esp_idf_svc::hal::gpio::{Output, PinDriver};
use esp_idf_svc::hal::i2c::I2cDriver;
use esp_idf_svc::hal::i2c::config::Config as I2cConfig;
use esp_idf_svc::hal::modem::Modem;
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::hal::units::Hertz;
use esp_idf_svc::sys::EspError;

use crate::config::I2C_BAUDRATE_HZ;

/// The user LED (GPIO27).
pub type Led = PinDriver<'static, Output>;

/// The peripherals the firmware uses, already configured.
pub struct Board {
    /// The user LED, initially off.
    pub led: Led,
    /// The I2C bus of the OLED: D4 = SDA = GPIO23, D5 = SCL = GPIO24.
    pub display_i2c: I2cDriver<'static>,
    /// The radio, for Wi-Fi.
    pub modem: Modem<'static>,
}

impl Board {
    /// Takes ownership of the chip's peripherals. Call this once.
    ///
    /// # Errors
    ///
    /// Fails if the peripherals were already taken or a pin cannot be configured.
    pub fn take() -> Result<Self, EspError> {
        let peripherals = Peripherals::take()?;
        let pins = peripherals.pins;

        let led = PinDriver::output(pins.gpio27)?;
        let display_i2c = I2cDriver::new(
            peripherals.i2c0,
            pins.gpio23,
            pins.gpio24,
            &I2cConfig::new().baudrate(Hertz(I2C_BAUDRATE_HZ)),
        )?;
        Ok(Self {
            led,
            display_i2c,
            modem: peripherals.modem,
        })
    }
}
