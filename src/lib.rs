//! Firmware for the XIAO ESP32-C5 on the Seeed expansion board.
//!
//! `main.rs` only wires the modules together; each module owns one concern:
//!
//! - [`config`]: every tunable constant.
//! - [`board`]: which pin does what.
//! - [`platform`]: the few raw ESP-IDF calls (the only `unsafe` code).
//! - [`secrets`]: encrypted storage for passwords and keys.
//! - [`status`]: the Wi-Fi and Bluetooth status types shared between threads (no hardware).
//! - [`led`]: the blinking user LED.
//! - [`ui`]: the OLED (status bar and scrolling message).
//! - [`bluetooth`]: Bluetooth LE (pairing, services, advertising, scanning).
//! - [`wifi`]: joining a WPA3 network.
//! - [`clock`]: the wall clock, set with SNTP (TLS needs it).
//! - [`http`]: an HTTP/HTTPS client (HTTP/1.1, TLS 1.3).
//! - [`message`]: the scrolling text, shared between the thread that fetches it and the screen.
//! - [`weather`]: the Taipei forecast, fetched over HTTPS.
//! - [`mqtt`]: an MQTT client (3.1.1 and 5.0, TLS and WebSocket).
//! - [`telemetry`]: what the board does over MQTT: state, commands, availability.
//! - [`home_assistant`]: the discovery message that lets Home Assistant find the board.

pub mod bluetooth;
pub mod board;
pub mod clock;
pub mod config;
pub mod home_assistant;
pub mod http;
pub mod led;
pub mod message;
pub mod mqtt;
pub mod platform;
pub mod secrets;
pub mod status;
pub mod telemetry;
pub mod ui;
pub mod weather;
pub mod wifi;
