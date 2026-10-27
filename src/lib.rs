//! Firmware for the XIAO ESP32-C5 on the Seeed expansion board.
//!
//! `main.rs` only wires the modules together; each module owns one concern:
//!
//! - [`config`]: every tunable constant.
//! - [`board`]: which pin does what.
//! - [`platform`]: the few raw ESP-IDF calls (the only `unsafe` code).
//! - [`secrets`]: encrypted storage for passwords and keys.
//! - [`status`]: the Wi-Fi status type shared between threads (no hardware).
//! - [`led`]: the blinking user LED.
//! - [`ui`]: the OLED (status bar and scrolling message).
//! - [`wifi`]: joining a WPA3 network.
//! - [`clock`]: the wall clock, set with SNTP (TLS needs it).
//! - [`http`]: an HTTP/HTTPS client (HTTP/1.1, TLS 1.3).

pub mod board;
pub mod clock;
pub mod config;
pub mod http;
pub mod led;
pub mod platform;
pub mod secrets;
pub mod status;
pub mod ui;
pub mod wifi;
