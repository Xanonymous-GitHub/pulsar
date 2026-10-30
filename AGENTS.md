# AGENTS.md

Firmware for the Seeed XIAO ESP32-C5 on the Seeeduino XIAO Expansion Board. Rust `std` on ESP-IDF v6.1 through `esp-idf-svc`. It started from `esp-generate`; that template no longer applies.

## Project-specific notes

- Target chip: `esp32c5`, Rust target `riscv32imac-esp-espidf` (tier 3: nightly, `rust-src`, `build-std`, linker `ldproxy`). See `.cargo/config.toml` and `rust-toolchain.toml`.
- This is a `std` project. Use plain threads (`thread::Builder` with a name and stack size), `log`, `thiserror` for module errors, and `anyhow` only in `main`.
- ESP-IDF settings live in `sdkconfig.defaults` (development) and `sdkconfig.defaults.secure` (flash encryption, applied only by the user). ESP-IDF itself is downloaded by `esp-idf-sys` into the git-ignored `.embuild/`.
- Layout and module roles are in `README.md` and the docs at the top of `src/lib.rs`. Keep `main.rs` as wiring only, and constants in `src/config.rs`.
- `unsafe` is denied crate-wide and allowed only in `src/platform.rs`, with a `// SAFETY:` comment on each use. Clippy pedantic is on; CI runs `cargo clippy -- -D warnings`.
- Log level is capped at `info` at compile time (`log` feature `max_level_info`), which also keeps Wi-Fi config out of debug logs.

## Project learnings

- The package/bin is named `pulsar`. The directory was renamed from `esp32-c5`; `esp32-c5` is stale and should not reappear.
- The nightly is pinned to `nightly-2026-10-19`: from 2026-10-20 `std` reads `libc::stat.st_atime` but libc 0.2.191 has only `st_atim` for ESP-IDF, so the `std` build fails. Raise the pin only after a test build.
- Do NOT put `CONFIG_PARTITION_TABLE_*` in `sdkconfig.defaults`: `esp-idf-sys` builds a stub app and cannot find `partitions.csv`. The table is applied at flash time (`--partition-table partitions.csv` in the runner), so run `cargo run` from the project root.
- `cargo run` flashes fine but its monitor needs a TTY. Read logs with `espflash monitor --non-interactive`, and redirect to a file (a pipe into `head` buffers and hangs).
- Wi-Fi credentials come from the shell env at build time (`PULSAR_WIFI_SSID`, `PULSAR_WIFI_PASSWORD`, read by `option_env!` in `src/wifi.rs`) and are kept in secret-store key `wifi`. Unset both to reuse the stored ones; only one set is an error. Never commit, log or echo them; the user types the password themselves.
- The secret store (`src/secrets.rs`) is an encrypted NVS partition (`secrets`) whose keys live in `nvs_keys` (flagged `encrypted`). It refuses to open without flash encryption; the off-by-default `insecure-dev-store` feature allows it for testing. The flash-encryption check must run BEFORE `EspEncryptedNvsPartition::take`, which generates the keys if absent.
- Never run eFuse / secure-boot commands (`espefuse`, burning keys) from automation, and never flash a build made with `sdkconfig.defaults.secure`: the first boot burns eFuses for good. Do not set `CONFIG_NVS_ENCRYPTION`: on the C5 it selects the HMAC scheme and `nvs_flash_init()` would burn an eFuse key.
- `espflash` only overwrites what it writes, so plaintext from an old layout survives in unused regions (the old bare-metal `secrets` data did). Erase a data partition with `espflash erase-region` when its format changes.
- `esp-idf-svc` 0.53 ignores `ClientConfiguration::pmf_cfg` (hardcodes PMF off in the conversion); WPA3 is enforced by `AuthMethod::WPA3Personal` and the ESP-IDF C layer (the log shows `pmf:1`). Band mode and per-band Wi-Fi 6 protocols are set through `esp-idf-sys` in `src/platform.rs`.
- 5 GHz is preferred through `scan_threshold` `rssi_5g_adjustment` (`config::WIFI_PREFER_5GHZ_MARGIN_DB`, set in `platform::prefer_5ghz`): a 5 GHz AP within that many dB of the best 2.4 GHz AP wins. It applies at connect time only; the board does not roam to 5 GHz later. In the C struct the field is `wifi_sta_config_t.threshold`.
- The station hostname (`pulsar`) is set on the station `EspNetif` DHCP settings, so `EspWifi::wrap_all` needs an AP netif as well (SoftAP support is compiled in).
- HTTPS needs `CONFIG_MBEDTLS_SSL_PROTO_TLS1_3=y` (off by default) and the certificate date check `CONFIG_MBEDTLS_HAVE_TIME_DATE=y` (off by default), which needs the SNTP clock (`src/clock.rs`); `http::Client` waits for it before HTTPS requests.
- `esp_http_client` is HTTP/1.1 only, so there is no HTTP/2 or HTTP/3. `esp-idf-svc` panics on unsupported methods and when a connection is reused after an error, `esp_http_client` keeps request headers between requests, and an `http://` to `https://` redirect fails (400). So `http::Client` opens a new connection per request.
- The default 512-byte HTTP receive buffer is too small for real headers; `config::HTTP_RX_BUFFER_BYTES` raises it. A thread that makes HTTPS requests needs at least 10 KiB of stack.
- Asia/Taipei is a fixed UTC+8 (no daylight saving), so `clock::LocalTime` uses an offset and needs no time zone database or `setenv("TZ")`.
- rust-analyzer reports false "can't find crate for core/std" for this target; trust `cargo build`.
- The display is an SSD1306 128x64 on I2C (XIAO D4 = SDA = GPIO23, D5 = SCL = GPIO24); the user LED is GPIO27.
- Bluetooth is NimBLE through `esp32-nimble` (`src/bluetooth/`, `docs/bluetooth.md`). `BLEDevice::take()` runs `nvs_flash_init()` on the plain `nvs` partition; never set `BT_NIMBLE_NVS_PERSIST`. Bonds are saved in the secret store (`bonds.rs`) as raw NimBLE structs with a length check, so an ESP-IDF bump may force phones to re-pair.
- With `BT_NIMBLE_EXT_ADV=y`, `esp32-nimble` swaps in `BLEExtAdvertising`: no restart after a connect or disconnect (`advertising::restart` does it), no Flags field (added by our vendored patch; without it phone Settings pages do not list the board), and `complete_service` writes the wrong AD type.
- Any advertising set makes Wi-Fi's two-band scan take about 24 s instead of 13 s (not interval- or PHY-related), so advertising starts after Wi-Fi's first attempt (`BLE_WIFI_WAIT`). `BlockingWifi::connect` has a fixed 15 s timeout, so `wifi::join` waits for the events itself (`WIFI_CONNECT_TIMEOUT`).
- `esp32-nimble` is vendored in `vendor/esp32-nimble` (`[patch.crates-io]`, see `PATCHES.md` there): upstream `todo!()`s on pairing actions a peer can trigger (remote abort), races on the subscriber list, and an MTU underflow on notify. Drop it when upstream fixes both; re-apply the patches when bumping the crate.
- `espflash monitor` right after flashing can fail with `Broken pipe` (the USB port re-enumerates): wait a few seconds and retry.
- On this nightly `Atomic*::fetch_update` is renamed `try_update`; the deprecation fails `clippy -D warnings`.
- `CONFIG_MQTT_PROTOCOL_5=y` is required: `esp-idf-svc` gates the MQTT 5 types on it. Commit `components_esp32c5.lock` (the MQTT component is fetched into it).
- Never wait or lock inside the `esp-mqtt` event callback (the MQTT task holds the client lock there); the blocking `EspMqttConnection` API deadlocks when its reader publishes.
- A retained command is replayed by the broker after every subscribe, at any time, and `esp-idf-svc` does not expose the retain flag. So `restart` reboots only after the broker echoes the board's own empty retained clear (`src/telemetry/confirmation.rs`); a time window cannot tell a late replay from a real command.
- MQTT 5 session expiry and Will Delay work only with a kept session (`persistent_session`): a clean start ends the old session and the broker sends the Will at once.
- `cargo test` cannot run (device-only crate, `harness = false`). Pure modules with no crate imports are tested on the host: `rustc --edition 2024 --test src/mqtt/topic.rs -o /tmp/t && /tmp/t` (same for `src/mqtt/reassemble.rs`, `src/mqtt/budget.rs` and `src/telemetry/confirmation.rs`).
- Clippy `doc_markdown` rejects a bare `QoS` in doc comments: write it in backticks.
- Heap is the tight resource (about 80 KB free with Wi-Fi, BLE and the display on; a thread costs its whole stack). Measure with the `MQTT online, free heap ...` log line before adding threads or buffers.
- TLS error `-0x008D` (PSA out of memory) or `alloc(16725 bytes) failed` means the heap ran dry; see the mbedTLS settings in `sdkconfig.defaults` and the 6 KiB MQTT message cap.
- `eim_config.toml` and the Espressif Installation Manager install are not used by the Rust build.

## Useful references

- esp-idf-svc: https://github.com/esp-rs/esp-idf-svc
- esp-idf-sys build options: https://github.com/esp-rs/esp-idf-sys/blob/master/BUILD-OPTIONS.md
- ESP-IDF ESP32-C5 flash encryption: https://docs.espressif.com/projects/esp-idf/en/latest/esp32c5/security/flash-encryption.html
- ESP-IDF NVS encryption: https://docs.espressif.com/projects/esp-idf/en/latest/esp32c5/api-reference/storage/nvs_flash.html
- Project guide for the secret store: `docs/secure-wifi-secrets.md`
