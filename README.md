# pulsar

Pulsar is firmware for the ESP32-C5 microcontroller. It uses Rust and the `esp-hal` crate.

The program prints a log message 10 times, 500 ms apart. Then it idles. Use it as a base for new ESP32-C5 projects.

## Requirements

- An ESP32-C5 board with a USB cable that carries data.
- Rust 1.99 or later, installed with `rustup`.
- The `espflash` tool. Install it with `cargo install espflash`.

The file `rust-toolchain.toml` installs the `riscv32imac-unknown-none-elf` target automatically.

## Build and flash

1. Connect the board to your computer.
2. Run `cargo run` in the project directory.

`cargo run` builds the firmware, flashes it to the board, and opens the serial monitor.

The monitor shows these log lines:

```
INFO - Hello world! (iteration 0)
...
INFO - Hello world! (iteration 9)
INFO - Done, idling.
```

For a release build, run `cargo run --release`.

## Serial monitor

| Key      | Action                                     |
|----------|--------------------------------------------|
| `Ctrl+R` | Reset the chip. The program starts again.  |
| `Ctrl+C` | Close the monitor. The chip keeps running. |

To open the monitor without flashing, run `espflash monitor --chip esp32c5`.

## Troubleshooting

**The monitor shows `Broken pipe` after flashing.**
The USB connection restarted when the program started. Run `espflash monitor --chip esp32c5`. Then press `Ctrl+R`.

**Flashing fails.**
1. Hold the BOOT button and press RESET.
2. Release BOOT.
3. Run `cargo run` again.

If you connect more than one board, add `--port <PORT>` to the `espflash` command. Use the port name from `ls /dev/cu.usb*`.

**The build fails with `invalid Mach-O section specifier`.**
Cargo builds for your computer and not for the chip. Make sure `.cargo/config.toml` exists. It must set `target = "riscv32imac-unknown-none-elf"` in the `[build]` section.

## Project layout

| Path                  | Contents                                                        |
|-----------------------|-----------------------------------------------------------------|
| `src/bin/main.rs`     | The firmware entry point.                                       |
| `.cargo/config.toml`  | The build target, the flash command, and the default log level. |
| `build.rs`            | The linker script setup.                                        |
| `rust-toolchain.toml` | The Rust toolchain and target.                                  |
| `AGENTS.md`           | Instructions for AI coding agents.                              |

## Configuration

Set the log level with the `ESP_LOG` environment variable. The default is `info`. Example: `ESP_LOG=debug cargo run`.

## Limits

- The code uses `no_std`. It cannot use the Rust standard library.
- The project has no heap allocator. Types such as `Vec` and `String` are not available.

## References

- [esp-hal documentation](https://docs.espressif.com/projects/rust/esp-hal/1.2.2/index.html)
- [esp-hal examples](https://github.com/esp-rs/esp-hal/tree/esp-hal-v1.2.2/examples)
- [esp-generate](https://github.com/esp-rs/esp-generate), the template this project started from
