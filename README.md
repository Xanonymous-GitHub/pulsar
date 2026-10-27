# pulsar

Pulsar is firmware for the Seeed XIAO ESP32-C5 on the Seeeduino XIAO Expansion Board. It is written in Rust on top of ESP-IDF v6.1 (`std`, via `esp-idf-svc`).

What it does:

- Joins a **WPA3-Personal** Wi-Fi network on **2.4 GHz or 5 GHz** (Wi-Fi 6 is allowed on both bands). It prefers 5 GHz when that signal is within 10 dB of the best 2.4 GHz one (`WIFI_PREFER_5GHZ_MARGIN_DB` in `src/config.rs`), and uses 2.4 GHz otherwise. The device name on the network is `pulsar`.
- Shows the SSID, signal strength, band and a signal-bar icon on the 0.96" OLED, with the local time (Asia/Taipei, from SNTP) in the corner and the Taipei weather forecast scrolling under it. The forecast comes over HTTPS from Open-Meteo every 15 minutes, so a report on the screen shows that Wi-Fi, DNS, TLS 1.3, the certificate chain and the clock all work. Without internet the last report stays, marked `(old)` after 35 minutes, and updates are retried with a growing delay until the connection is back.
- Has a general HTTP/HTTPS client (`src/http.rs`): HTTP/1.1 with TLS 1.3 (TLS 1.2 as a fallback), the Mozilla root certificates and certificate date checks. HTTP/2 and HTTP/3 are not available in ESP-IDF.
- Blinks the user LED.
- Keeps the Wi-Fi password in an encrypted NVS partition. See [docs/secure-wifi-secrets.md](docs/secure-wifi-secrets.md).

## Requirements

- A XIAO ESP32-C5 with a USB cable that carries data.
- `rustup`. The file `rust-toolchain.toml` installs the pinned nightly and `rust-src`. The ESP32-C5 Rust target is tier 3, so it needs nightly and `build-std`.
- `espflash` and `ldproxy`: `cargo install espflash ldproxy --locked`.
- The ESP-IDF build prerequisites: `git`, `cmake`, `ninja`, `python3` with `venv` and `gzip`, and a C compiler for the host.

The first build downloads ESP-IDF v6.1 and its tools into `.embuild/` (git-ignored). This takes a while. Later builds are fast.

## Build and flash

Pass the Wi-Fi settings through the environment of the build. They are never stored in a tracked file. `read -rs` keeps the password out of your history:

```sh
export PULSAR_WIFI_SSID='my-network'
read -rs PULSAR_WIFI_PASSWORD && export PULSAR_WIFI_PASSWORD
cargo run --features insecure-dev-store
```

`cargo run` builds, flashes (with `partitions.csv`) and opens the serial monitor. Run it from the project root.

On the first run the credentials are stored on the chip. Later builds can leave both variables unset and the firmware uses the stored ones. If only one of them is set, the build is refused at start-up.

The secret store refuses to open unless flash encryption is on. The `insecure-dev-store` feature lets it open without it, for testing on your desk only. It logs a warning at every boot. Follow [docs/secure-wifi-secrets.md](docs/secure-wifi-secrets.md) to turn flash encryption on and drop the feature.

For a release build, add `--release`.

## Serial monitor without a terminal

`cargo run` needs a real terminal for the monitor. In a script or CI shell use:

```sh
espflash monitor --non-interactive
```

## Troubleshooting

**Flashing fails.**
1. Hold BOOT and press RESET.
2. Release BOOT.
3. Run `cargo run` again.

If more than one board is connected, add `--port <PORT>` to the `espflash` command. Use the name from `ls /dev/cu.usb*`.

**The build fails inside `std` with `st_atime` errors.**
A newer nightly than the pinned one is in use. Keep the `channel` in `rust-toolchain.toml`: from `nightly-2026-10-20` on, `std` does not compile for ESP-IDF with the current `libc` crate.

**The ESP-IDF build fails in a Python step.**
Delete `.embuild/espressif/python_env` and build again. It is rebuilt from your current Python.

**rust-analyzer says it can't find `core` or `std`.**
This is a false alarm for this tier-3 target. `cargo build` is the source of truth.

## Project layout

| Path | Contents |
|---|---|
| `src/main.rs` | Wiring only: start the LED, the screen and Wi-Fi. |
| `src/config.rs` | Every tunable constant (periods, stack sizes, text, hostname). |
| `src/board.rs` | The pin map for the board and expansion board. |
| `src/wifi.rs` | Credentials, WPA3 station, dual-band setup, reconnect with backoff. |
| `src/secrets.rs` | The encrypted secret store on top of NVS. |
| `src/status.rs` | The Wi-Fi status shared between Wi-Fi and the screen. |
| `src/clock.rs` | SNTP and the local time of day. |
| `src/http.rs` | The HTTP/HTTPS client. |
| `src/weather.rs` | Fetches the forecast and turns it into the scrolling text. |
| `src/message.rs` | The scrolling text shared between the weather thread and the screen. |
| `src/ui/` | The screen: status bar (with the clock) and scrolling text. |
| `src/led.rs` | The blinking LED. |
| `src/platform.rs` | The few ESP-IDF calls that need `unsafe`. |
| `partitions.csv` | Flash layout, including the `secrets` and `nvs_keys` partitions. |
| `sdkconfig.defaults` | ESP-IDF settings for development. |
| `sdkconfig.defaults.secure` | Flash encryption overlay. Applied only by you. |
