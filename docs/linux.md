# Linux on the board

With the `linux` feature the firmware boots a real Linux kernel and gives you a root shell on the USB serial port:

```text
Welcome to Linux on pulsar (XIAO ESP32-C5)
~ # uname -a
Linux pulsar 7.3.4 #2 PREEMPT Sun Nov 15 10:13:50 UTC 2026 riscv32 GNU/Linux
~ # free
              total        used        free      shared  buff/cache   available
Mem:           5680        1532        3060           0        1088        2652
```

Wi-Fi, Bluetooth, the screen and the rest of the firmware keep running next to it.

## How it works

The ESP32-C5 cannot run Linux itself. It has no memory management unit, about 380 KB of RAM of its own, and Linux has no port for its interrupt controller, its timers or its boot process. A native port would mean writing and maintaining that port.

The board does have a fast processor and 8 MiB of PSRAM. So the firmware contains a small computer in software, one that Linux already knows, and Linux runs on that:

| Part | Where | What it is |
|---|---|---|
| Processor | `src/linux/cpu.rs` | RV32IMA with machine and user mode, no MMU, and the timer of a CLINT |
| Devices | `src/linux/machine.rs` | An 8250 serial port (the console) and a power switch |
| Description | `src/linux/fdt.rs` | The device tree that tells the kernel all this, built at boot |
| RAM | the board's PSRAM | All 8 MiB, mapped and handed to the guest by `platform::psram_take` |
| Kernel | the `linux` flash partition | One image file: kernel, BusyBox and the root file system |
| Console | the USB serial port | `src/linux/guest.rs` moves bytes between the port and the guest's serial port |

It is the machine of [mini-rv32ima](https://github.com/cnlohr/mini-rv32ima), a small emulator that showed that this works. The code here is written from the RISC-V specifications, and follows them where that project takes shortcuts.

The three files `cpu.rs`, `fdt.rs` and `machine.rs` use nothing from the rest of the firmware, so they also run on a desktop computer: for their unit tests, and to try a kernel image before flashing it (see [Try an image on your computer](#try-an-image-on-your-computer)).

## Build the kernel image

The image is built by [Buildroot](https://buildroot.org) from the kernel sources of [kernel.org](https://www.kernel.org), in a Docker container:

```sh
tools/linux-image/build.sh        # writes target/linux/Image
```

The first run builds a cross compiler and takes about half an hour and 10 GB in a Docker volume. Later runs take a few minutes. `tools/linux-image/build.sh clean` deletes the volume.

What it builds is in `tools/linux-image/`:

| File | Contents |
|---|---|
| `versions.env` | The kernel version and the Buildroot version (with its checksum). |
| `configs/pulsar_defconfig` | The Buildroot configuration: an RV32IMA no-MMU toolchain with uClibc-ng, static "flat" programs, BusyBox, the root file system inside the kernel image. |
| `board/pulsar/linux.config` | The kernel configuration: what the machine needs and nothing that 8 MiB cannot afford (no network, no disks, no modules). |
| `board/pulsar/busybox.fragment` | Changes to Buildroot's BusyBox configuration for machines without MMU: fewer applets. |
| `board/pulsar/overlay/` | Our files in `/etc`: a root shell on the console instead of a login. |
| `external.mk` | The one adjustment to a Buildroot package: uClibc-ng without FPU code, which Buildroot cannot configure for RISC-V. |

There are no patches and no forks.

### Upgrade the kernel or Buildroot

Change `LINUX_VERSION` in `tools/linux-image/versions.env` to any release from kernel.org (6.18 or newer: the C library is built against the 6.18 headers) and run `build.sh` again. Only the kernel is rebuilt.

For Buildroot, change `BUILDROOT_VERSION` and `BUILDROOT_SHA256` (the checksum is in the signed `.sign` file next to the download on buildroot.org), run `build.sh clean` and build again.

Then check the new image on your computer before you flash it.

## Try an image on your computer

```sh
rustc -O --edition 2024 tools/linux-host.rs -o target/linux-host
stty raw -echo; target/linux-host target/linux/Image; stty sane
```

This is the same emulator with the same 8 MiB. Type `poweroff` to leave. For a check without a keyboard:

```sh
target/linux-host target/linux/Image --mips 10 --until '~ # ' --limit 1500 < /dev/null
```

It ends with status 0 when the shell prompt appears. `--mips` makes the guest's clock follow the instruction count instead of the wall clock, so the run is repeatable; `--less 64` takes 64 KiB off the RAM, to see how much room is left.

## Build and flash the firmware

The `linux` feature needs the PSRAM settings in `sdkconfig.defaults.linux`:

```sh
export ESP_IDF_SDKCONFIG_DEFAULTS='sdkconfig.defaults;sdkconfig.defaults.linux'
cargo build --features linux,insecure-dev-store
espflash flash --partition-table partitions.csv target/riscv32imac-esp-espidf/debug/pulsar
espflash write-bin 0x320000 target/linux/Image
espflash monitor
```

- The kernel image goes to the `linux` partition at `0x320000` (see `partitions.csv`). It stays there when you flash the firmware again.
- Wait a few seconds between two `espflash` commands. Right after one, the next can fail with `Error while connecting to device`.
- A build with the feature but without those settings stops with an error that says so.

`espflash monitor` is the console: what you type goes to the guest. The firmware's own messages share the port; once the kernel starts, only its warnings and errors still appear.

## What to expect

Measured on a XIAO ESP32-C5 with Linux 7.3.4:

| | |
|---|---|
| Speed | about 1 million instructions per second (the firmware logs the figure when the guest stops or restarts) |
| `Run /init` | about 20 seconds after the kernel starts |
| Shell prompt | a few seconds later |
| RAM | 8192 KiB for the guest; 4480 KiB free when the kernel starts, about 3 MiB at the prompt |

The speed is set by the PSRAM, not by the emulator: every access to the guest's memory goes through the chip's 32 KiB cache, and compiling the emulator for speed instead of size made 5 % of difference.

Without an MMU Linux loads every running program whole into one contiguous block of RAM, rounded up to a power of two. BusyBox here needs a 512 KiB block per process, which is why it was made small: with a BusyBox that needs 1 MiB (as in the prebuilt mini-rv32ima image) the shell starts and then cannot run a second program.

## Limits

- The guest has no network and no storage: nothing survives `poweroff`, and the root file system is the one in the image.
- The guest has all of the PSRAM. The firmware keeps using internal RAM as before.
- One line in the boot log is expected: `psram_mspi: Failed to allocate dummy cacheline for PSRAM memory barrier!`. ESP-IDF wants a cache line of PSRAM for a workaround that only matters when PSRAM is used for DMA, and the guest's memory never is.
- The guest's clock starts at zero at every boot. It has no real-time clock.
- `reboot` in the guest loads the kernel from flash again; `poweroff` stops the guest until the board is reset. Both log a line such as `Linux: PowerOff after 52 s and 46 million instructions (887 thousand per second)`.

## If it does not start

| Log line | Meaning |
|---|---|
| `Linux not started: no PSRAM for the guest's memory` | The board has no PSRAM, or the firmware was not built with `sdkconfig.defaults.linux`. |
| `Linux not started: the flash has no 'linux' partition` | Flash with `--partition-table partitions.csv` of this build. |
| `cannot start the kernel in the 'linux' partition: not a RISC-V Linux kernel image` | Nothing, or something else, is at `0x320000`: write the image with `espflash write-bin`. |
| `... the kernel needs N bytes of RAM and the machine has M` | The image is too large for the PSRAM. |
| The kernel starts and stops with `Kernel panic - not syncing: No working init found` and `error -12` | The image needs more than 8 MiB. Try it with `tools/linux-host.rs` first. |
