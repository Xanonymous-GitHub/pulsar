//! Runs the Linux guest on the board: gives it the PSRAM as its RAM, loads the kernel from
//! flash, and connects its console to the USB serial port. See the [parent module](super).

use std::io;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use esp_idf_svc::hal::task::thread::ThreadSpawnConfiguration;
use esp_idf_svc::sys::EspError;

use super::cpu::{Cpu, Exit};
use super::machine::{self, BOOT_ARGS, BootError, Devices, IMAGE_HEADER_BYTES};
use crate::config::{
    LINUX_BATCH_INSTRUCTIONS, LINUX_BREATHER, LINUX_BREATHER_EVERY, LINUX_CONSOLE_POLL,
    LINUX_IDLE_SLEEP_MAX, LINUX_PARTITION, LINUX_STACK_BYTES, LINUX_THREAD_PRIORITY,
};
use crate::platform;

/// Bytes copied from flash at a time. The buffer is in internal RAM: the flash driver cannot
/// write straight into PSRAM while it has the flash and PSRAM bus to itself.
const FLASH_CHUNK_BYTES: usize = 4096;

/// What can keep the guest from running.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The board's PSRAM is not there, or something else took it.
    #[error("no PSRAM for the guest's memory")]
    NoPsram,
    /// The flash has no partition for the kernel image.
    #[error("the flash has no 'linux' partition: flash with the partitions.csv of this build")]
    NoPartition,
    /// The partition does not hold a kernel, or the kernel does not fit in the memory.
    #[error("cannot start the kernel in the 'linux' partition: {0} (see docs/linux.md)")]
    Boot(#[from] BootError),
    /// An ESP-IDF call failed.
    #[error("driver: {0}")]
    Driver(#[from] EspError),
    /// The thread could not be created.
    #[error("cannot start the Linux thread: {0}")]
    Thread(#[from] io::Error),
}

/// Starts the guest on a thread of its own and returns at once.
///
/// The thread runs below every other thread of the firmware, so Linux gets the processor time
/// that is left over and the radio, the screen and the rest go on as before.
///
/// # Errors
///
/// Fails if the thread cannot be created. Problems with the PSRAM or the kernel image are
/// logged by the thread.
pub fn spawn() -> Result<JoinHandle<()>, Error> {
    // The priority of a new thread comes from a setting that applies to every thread created
    // after it, so it is put back right away.
    let before = ThreadSpawnConfiguration::get().unwrap_or_default();
    ThreadSpawnConfiguration {
        priority: LINUX_THREAD_PRIORITY,
        ..ThreadSpawnConfiguration::default()
    }
    .set()?;
    let thread = thread::Builder::new()
        .name("linux".into())
        .stack_size(LINUX_STACK_BYTES)
        .spawn(|| {
            if let Err(error) = run() {
                log::error!("Linux not started: {error}");
            }
        });
    before.set()?;
    Ok(thread?)
}

/// Boots the guest, and boots it again whenever it restarts itself, until it powers off.
fn run() -> Result<(), Error> {
    let ram = platform::psram_take().ok_or(Error::NoPsram)?;
    platform::console_claim()?;
    loop {
        let kernel_bytes = load_kernel(ram)?;
        let mut cpu = machine::boot(ram, BOOT_ARGS)?;
        log::warn!(
            "Linux: starting a {} KiB kernel in {} KiB of RAM; the console is this port",
            kernel_bytes / 1024,
            ram.len() / 1024
        );
        // From here on this port is the guest's console: only warnings and errors of the
        // firmware still appear on it.
        log::set_max_level(log::LevelFilter::Warn);
        platform::quiet_system_log();

        let started = Instant::now();
        let exit = execute(&mut cpu, ram);
        let seconds = started.elapsed().as_secs().max(1);
        log::warn!(
            "Linux: {exit:?} after {seconds} s and {} million instructions ({} thousand per second)",
            cpu.cycles() / 1_000_000,
            cpu.cycles() / seconds / 1000
        );
        if exit != Exit::Reboot {
            return Ok(());
        }
    }
}

/// Copies the kernel image from its flash partition to the start of `ram` and returns how many
/// bytes it occupies there.
fn load_kernel(ram: &mut [u8]) -> Result<usize, Error> {
    let partition = platform::Partition::find(LINUX_PARTITION).ok_or(Error::NoPartition)?;
    let mut chunk = vec![0_u8; FLASH_CHUNK_BYTES];

    partition.read(0, &mut chunk[..IMAGE_HEADER_BYTES])?;
    let kernel_bytes = machine::kernel_size(&chunk[..IMAGE_HEADER_BYTES])?;
    // The image file is shorter than this (the kernel zeroes the rest itself), but its length is
    // not recorded anywhere. Whatever follows it in the partition is copied along and ignored.
    let too_large = BootError::TooLittleRam {
        needed: kernel_bytes,
        available: ram.len() as u64,
    };
    let bytes = usize::try_from(kernel_bytes)
        .map_err(|_| too_large)?
        .min(partition.size());
    let target = ram.get_mut(..bytes).ok_or(too_large)?;

    for (index, part) in target.chunks_mut(FLASH_CHUNK_BYTES).enumerate() {
        let buffer = &mut chunk[..part.len()];
        partition.read(index * FLASH_CHUNK_BYTES, buffer)?;
        part.copy_from_slice(buffer);
    }
    Ok(bytes)
}

/// Runs the guest until it powers off or restarts.
fn execute(cpu: &mut Cpu, ram: &mut [u8]) -> Exit {
    let mut devices = Devices::new();
    let mut input = [0_u8; 16];
    let mut last = Instant::now();
    let mut last_poll = last;
    let mut last_breather = last;

    loop {
        let now = Instant::now();
        let elapsed = u32::try_from((now - last).as_micros()).unwrap_or(u32::MAX);
        last = now;
        let exit = cpu.run(ram, &mut devices, elapsed, LINUX_BATCH_INSTRUCTIONS);
        devices.take_output(platform::console_write);

        // Keys from the host, as many as the guest's serial port has room for.
        if now - last_poll >= LINUX_CONSOLE_POLL {
            last_poll = now;
            let room = devices.input_space().min(input.len());
            let received = platform::console_read(&mut input[..room]);
            devices.push_input(&input[..received]);
        }

        match exit {
            Exit::Budget => {
                // This thread never blocks while the guest is busy. The idle task below it,
                // which feeds the watchdog and frees finished threads, needs a turn sometimes.
                if now - last_breather >= LINUX_BREATHER_EVERY {
                    thread::sleep(LINUX_BREATHER);
                    last_breather = Instant::now();
                }
            }
            Exit::Idle => {
                // The guest waits for its timer. Sleep until then, but not for long: a key may
                // arrive.
                let until_timer = cpu
                    .micros_until_timer()
                    .map_or(LINUX_IDLE_SLEEP_MAX, Duration::from_micros);
                thread::sleep(until_timer.min(LINUX_IDLE_SLEEP_MAX));
                last_breather = Instant::now();
            }
            Exit::PowerOff | Exit::Reboot => return exit,
        }
    }
}
