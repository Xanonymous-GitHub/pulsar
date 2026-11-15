//! Linux on the board: an emulated RISC-V computer that runs a real Linux kernel, with a shell
//! on the USB serial port.
//!
//! The ESP32-C5 cannot run Linux itself. It has no memory management unit, and Linux does not
//! know its interrupt controller, timers or boot process. What it does have is a fast processor
//! and, on this board, 8 MiB of PSRAM. So the firmware contains a small computer in software that
//! Linux does know: an RV32IMA processor without MMU ([`cpu`]), a timer, a serial port and a
//! power switch ([`machine`]), described to the kernel by a device tree ([`fdt`]). The guest's
//! RAM is the PSRAM, the kernel image comes from the `linux` flash partition, and the guest's
//! serial port is the board's USB port ([`guest`], only with the `linux` feature).
//!
//! ```ignore
//! #[cfg(feature = "linux")]
//! linux::spawn()?;
//! ```
//!
//! `docs/linux.md` has the whole story: how to build, what image to flash, how fast it is.
//!
//! [`cpu`], [`fdt`] and [`machine`] import nothing from the rest of the crate and are compiled
//! and tested on the host as well; `tools/linux-host.rs` boots a kernel with them on a desktop.

pub mod cpu;
pub mod fdt;
#[cfg(feature = "linux")]
mod guest;
pub mod machine;

#[cfg(feature = "linux")]
pub use guest::{Error, spawn};
