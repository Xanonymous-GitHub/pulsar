# Protecting the Wi-Fi password on the chip

**Read this whole page before you run anything. Turning on flash encryption burns eFuses. That
cannot be undone, and in release mode it also changes how you can flash the board.** Nothing in this
repository does it for you. Only you run these steps.

## What is protected today

| Where | Protection |
| --- | --- |
| Your computer | The password only lives in the shell variable `PULSAR_WIFI_PASSWORD` while you build. It is not in any tracked file. |
| `secrets` partition (encrypted NVS) | The password is encrypted with keys stored in the `nvs_keys` partition. |
| `nvs_keys` partition | **Only protected if flash encryption is on.** Without it, anyone with the flash contents can read the keys and decrypt the password. |
| RAM | Wi-Fi settings are kept in RAM only (`config NVS flash: disabled` in the boot log). The password buffers are wiped when dropped. |

So until you do the steps below, the firmware refuses to open the secret store
(`secrets unavailable: ... flash encryption is not enabled`) and Wi-Fi does not start. For testing
on your own desk you can build with `--features insecure-dev-store`. That stores the keys in the
clear and logs a warning on every boot. Never ship that build.

What flash encryption does **not** help against: code that runs on the chip (it can read the
password), and anything on your computer (shell history, a terminal log). It also does not stop
someone from reading the password out of a running, unlocked device over JTAG, so see
"Lock the debug paths" below.

## Before you start

1. `espflash board-info` and check that `Flash encryption: disabled` and `Secure boot: disabled`
   is what you expect to see.
2. Run `espefuse summary` yourself and read it. `board-info` showed a non-zero key purpose
   (`Key Purposes: [0, 0, 0, 0, 0, 0, 19]`) on the development board. Find out what that is before
   you burn anything, because flash encryption needs a free key block.
3. Decide on the mode:
   * **Development** (the default in `sdkconfig.defaults.secure`): the bootloader encrypts the
     flash on first boot, but ROM download mode can still write new plaintext a limited number of
     times (the bootloader log shows the remaining count). Use this while the firmware changes.
     It is **not secure** against someone who holds the board: they can write their own plaintext.
   * **Release**: no more plaintext flashing and no manual encrypt/decrypt over UART. Updates must
     then be done over the air with the correct key. **Irreversible.** Use this only for a finished
     device and one that you have tested in development mode.
4. Keep the eFuse key safe. If the flash encryption key is generated on the chip (the default) you
   cannot read it back, and a lost device cannot be re-flashed in release mode.

## Steps

All commands run from the project root. Use `read -rs` so the password is not echoed or saved
in your shell history.

```sh
export PULSAR_WIFI_SSID='my-network'
read -rs PULSAR_WIFI_PASSWORD && export PULSAR_WIFI_PASSWORD

# Build with the secure overlay. A separate target dir keeps this build apart from the dev build.
CARGO_TARGET_DIR=target-secure \
ESP_IDF_SDKCONFIG_DEFAULTS="sdkconfig.defaults;sdkconfig.defaults.secure" \
cargo build --release
```

Building does not change the chip. The eFuses burn on the first boot of the flashed result.

The bootloader must be the one **built with the same overlay**, because the second-stage bootloader
is what encrypts the flash on first boot. The one `espflash` bundles is not built for it. Flash the
bootloader that `esp-idf-sys` built:

```sh
BOOTLOADER=$(ls -t target-secure/riscv32imac-esp-espidf/release/build/esp-idf-sys-*/out/build/bootloader/bootloader.bin | head -1)
espflash flash --bootloader "$BOOTLOADER" --partition-table partitions.csv \
  target-secure/riscv32imac-esp-espidf/release/pulsar
```

Then clear the variables (`unset PULSAR_WIFI_SSID PULSAR_WIFI_PASSWORD`) and open a serial monitor
(`espflash monitor`).

First boot: the bootloader encrypts the app, partition table, bootloader and every `encrypted`
partition, then restarts. This takes about a minute. Do not unplug the board. After it you should see:

* in the bootloader log, that flash encryption is on (and in development mode the number of
  plaintext flashes left),
* `flash encryption is on, the NVS keys are protected` from `pulsar::secrets`,
* the Wi-Fi connecting as before.

`espflash board-info` should now report flash encryption as enabled.

## Check that it worked

* The password is not in the log or in the flash: `espflash read-flash 0xf000 0x4000 secrets.bin`
  and `espflash read-flash 0x13000 0x1000 keys.bin` should show only random-looking bytes. Espressif's
  documentation does not promise that a raw read returns ciphertext, so treat this as a sanity
  check. The reliable proof is `espefuse summary` (`SPI_BOOT_CRYPT_CNT` set, flash encryption key
  block with purpose `XTS_AES_*`).
* A dev build run on the secured chip still works: it opens the store without the
  `insecure-dev-store` feature.

## Changing the password later

Set `PULSAR_WIFI_SSID` and `PULSAR_WIFI_PASSWORD` again and rebuild. The firmware stores the new
values on the next boot (it skips the write if nothing changed, which saves flash wear). In release
mode you must update through the over-the-air path, because plaintext flashing is disabled.

## Lock the debug paths (optional, for a finished device)

Release mode already disables manual UART encrypt/decrypt and the JTAG debug interface for the
flash encryption key. Also consider **Secure Boot v2**, which makes the chip run only firmware you
signed, and disabling the ROM download mode. Both are irreversible too. They are not enabled by
`sdkconfig.defaults.secure` and need their own careful read of Espressif's security documentation.

## Why not NVS encryption on its own?

* `CONFIG_NVS_ENCRYPTION` is left off in `sdkconfig.defaults.secure` on purpose. On the ESP32-C5 it
  selects the HMAC key scheme by default, and the first `nvs_flash_init()` would burn an HMAC key
  into an eFuse block without asking. The firmware never calls `nvs_flash_init`; it opens its own
  encrypted partition with the keys in `nvs_keys`.
* An HMAC-based NVS scheme (keys derived inside the chip from an eFuse key) would protect the
  password **without** full flash encryption. It is not implemented here because `esp-idf-svc`
  does not expose it. It is the natural next step if you want to avoid flash encryption.

## Leftovers in unused flash

`espflash` only writes what it flashes. If a partition used to hold plaintext (for example the
`secrets` partition from the earlier bare-metal firmware), the old bytes stay until the region is
erased. Erase a data partition you no longer use with `espflash erase-region <offset> <size>`.
This is plain flash, not an eFuse operation.
