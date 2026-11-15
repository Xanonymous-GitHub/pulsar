//! Boots a Linux kernel on a desktop computer in the same emulated machine the firmware has
//! (`src/linux/`), to try a kernel image before it is flashed, or to work on the emulator.
//!
//! ```sh
//! rustc -O --edition 2024 tools/linux-host.rs -o target/linux-host
//! stty raw -echo; target/linux-host target/linux/Image; stty sane
//! ```
//!
//! Options, after the image:
//!
//! - `--ram <MiB>`: the size of the machine's RAM (default 8, what the board's PSRAM gives).
//! - `--less <KiB>`: take this much off the RAM again, to try sizes that are not whole MiB.
//! - `--until <text>`: stop with success as soon as the console has printed `text`.
//! - `--limit <millions>`: give up after this many million instructions.
//! - `--mips <n>`: do not follow the wall clock; the machine's clock advances as if it ran `n`
//!   million instructions per second. Runs as fast as it can and is repeatable (for tests).
//!
//! In a normal terminal the console is line-buffered and echoes twice; `stty raw -echo` gives the
//! guest the keys as they are typed. Power the guest off with `poweroff`, or kill the program.

use std::io::{Read, Write};
use std::process::ExitCode;
use std::sync::mpsc;
use std::time::{Duration, Instant};

// The emulator itself: the same three files the firmware compiles.
#[allow(dead_code, reason = "the firmware uses parts of these files that this tool does not")]
#[path = "../src/linux"]
mod linux {
    pub mod cpu;
    pub mod fdt;
    pub mod machine;
}

use linux::cpu::Exit;
use linux::machine::{self, Devices};

/// Instructions per call of `Cpu::run`: the timer is looked at this often.
const BATCH: u32 = 4096;

struct Options {
    image: String,
    ram_mib: usize,
    less_kib: usize,
    until: Option<String>,
    limit: Option<u64>,
    mips: Option<u64>,
}

fn options() -> Result<Options, String> {
    let mut arguments = std::env::args().skip(1);
    let mut options = Options {
        image: arguments.next().ok_or("usage: linux-host <Image> [--ram MiB] [--less KiB] [--until text] [--limit millions] [--mips n]")?,
        ram_mib: 8,
        less_kib: 0,
        until: None,
        limit: None,
        mips: None,
    };
    while let Some(name) = arguments.next() {
        let value = arguments.next().ok_or_else(|| format!("{name} needs a value"))?;
        let number = || value.parse::<u64>().map_err(|_| format!("{name}: {value} is not a number"));
        match name.as_str() {
            "--ram" => options.ram_mib = usize::try_from(number()?).map_err(|error| error.to_string())?,
            "--less" => options.less_kib = usize::try_from(number()?).map_err(|error| error.to_string())?,
            "--until" => options.until = Some(value),
            "--limit" => options.limit = Some(number()? * 1_000_000),
            "--mips" => options.mips = Some(number()?.max(1)),
            _ => return Err(format!("unknown option {name}")),
        }
    }
    Ok(options)
}

fn run(options: &Options) -> Result<bool, String> {
    let image = std::fs::read(&options.image).map_err(|error| format!("{}: {error}", options.image))?;
    let mut ram = vec![0_u8; (options.ram_mib * 1024).saturating_sub(options.less_kib) * 1024];
    ram.get_mut(..image.len())
        .ok_or("the image is larger than the RAM")?
        .copy_from_slice(&image);
    let mut cpu = machine::boot(&mut ram, machine::BOOT_ARGS).map_err(|error| error.to_string())?;
    let mut devices = Devices::new();

    // The keyboard: a thread hands over what it reads, so the machine never waits for it.
    let (keys, typed) = mpsc::channel::<u8>();
    std::thread::spawn(move || {
        for byte in std::io::stdin().lock().bytes().map_while(Result::ok) {
            if keys.send(byte).is_err() {
                break;
            }
        }
    });

    let mut stdout = std::io::stdout().lock();
    let mut seen = Vec::new();
    let mut pending_key = None;
    let mut last = Instant::now();
    let started = Instant::now();
    let mut found = options.until.is_none();
    // With --mips: the instructions that the machine's clock has not been advanced for yet.
    let mut unaccounted = 0_u64;

    let exit = loop {
        // Console input, as much as the guest has room for.
        while devices.input_space() > 0 {
            let Some(byte) = pending_key.take().or_else(|| typed.try_recv().ok()) else {
                break;
            };
            if devices.push_input(&[byte]) == 0 {
                pending_key = Some(byte);
                break;
            }
        }

        let before = cpu.cycles();
        let elapsed = match options.mips {
            // `mips` instructions take one microsecond of machine time.
            Some(mips) => {
                let micros = unaccounted / mips;
                unaccounted %= mips;
                u32::try_from(micros).unwrap_or(u32::MAX)
            }
            None => {
                let now = Instant::now();
                let elapsed = u32::try_from((now - last).as_micros()).unwrap_or(u32::MAX);
                last = now;
                elapsed
            }
        };
        let mut exit = cpu.run(&mut ram, &mut devices, elapsed, BATCH);
        unaccounted += cpu.cycles() - before;

        devices.take_output(|bytes| {
            let _ = stdout.write_all(bytes);
            let _ = stdout.flush();
            if let Some(text) = &options.until {
                seen.extend_from_slice(bytes);
                found |= seen.windows(text.len()).any(|window| window == text.as_bytes());
                let keep = seen.len().saturating_sub(text.len());
                seen.drain(..keep);
            }
        });
        if found && options.until.is_some() {
            break Exit::Budget;
        }

        if exit == Exit::Idle {
            let wait = cpu.micros_until_timer();
            if options.mips.is_some() {
                // Skip the idle time: the next call moves the clock to the timer at once.
                let skip = u32::try_from(wait.unwrap_or(1000)).unwrap_or(u32::MAX);
                exit = cpu.run(&mut ram, &mut devices, skip, 0);
            } else {
                std::thread::sleep(Duration::from_micros(wait.unwrap_or(1000).min(10_000)));
            }
        }
        match exit {
            Exit::PowerOff | Exit::Reboot => break exit,
            Exit::Budget | Exit::Idle => {}
        }
        if options.limit.is_some_and(|limit| cpu.cycles() >= limit) {
            break Exit::Budget;
        }
    };

    let seconds = started.elapsed().as_secs_f64();
    #[allow(clippy::cast_precision_loss, reason = "a rate for people to read")]
    let millions = cpu.cycles() as f64 / 1e6;
    eprintln!(
        "\r\n[linux-host] {exit:?} after {millions:.1} million instructions in {seconds:.1} s ({:.0} MIPS)",
        millions / seconds.max(1e-9)
    );
    Ok(found)
}

fn main() -> ExitCode {
    match options().and_then(|options| run(&options)) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => {
            eprintln!("[linux-host] the expected text did not appear");
            ExitCode::FAILURE
        }
        Err(error) => {
            eprintln!("linux-host: {error}");
            ExitCode::from(2)
        }
    }
}
