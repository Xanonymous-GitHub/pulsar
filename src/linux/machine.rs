//! The machine around the [processor](super::cpu): where its devices are, what they do, the
//! device tree that describes it all to a kernel, and how a kernel image is started.
//!
//! It is the machine of [mini-rv32ima](https://github.com/cnlohr/mini-rv32ima), so kernels built
//! for that project run here:
//!
//! | Address | Device |
//! |---|---|
//! | `0x1000_0000` | An 8250 UART: the console. Transmit is always ready; there is no interrupt, the kernel polls. |
//! | `0x1100_0000` | A CLINT: the timer, inside [`Cpu`]. |
//! | `0x1110_0000` | A "syscon" register: storing `0x5555` powers off, `0x7777` restarts. |
//! | `0x8000_0000` | RAM. The kernel image is at its start and the device tree at its end. |
//!
//! Like `cpu.rs` and `fdt.rs` this file needs nothing from the rest of the crate; `tools/linux-host.rs`
//! builds the three of them into a program that boots a kernel on a desktop computer.

use std::collections::VecDeque;

use super::cpu::{Bus, CLINT_BASE, Cpu, RAM_BASE, Signal, TIMEBASE_HZ};
use super::fdt::Fdt;

/// The address of the UART.
pub const UART_BASE: u32 = 0x1000_0000;
/// The UART's line control register. While its top bit (DLAB) is set, the first two registers
/// are the baud rate divisor instead of the data and interrupt-enable registers.
const UART_LINE_CONTROL: u32 = UART_BASE + 3;
const UART_DIVISOR_LATCH_ACCESS: u32 = 0x80;
/// The UART's line status register: bit 0 says a received byte is waiting.
const UART_LINE_STATUS: u32 = UART_BASE + 5;
/// Line status: the transmitter is empty and can take a byte (always).
const UART_TRANSMIT_READY: u32 = 0x60;
/// Line status: a received byte is waiting.
const UART_DATA_READY: u32 = 0x01;

/// The address of the power switch.
pub const SYSCON_BASE: u32 = 0x1110_0000;
const SYSCON_POWER_OFF: u32 = 0x5555;
const SYSCON_REBOOT: u32 = 0x7777;

/// The kernel command line: print early messages and the console on the UART.
pub const BOOT_ARGS: &str = "earlycon=uart8250,mmio,0x10000000,1000000 console=ttyS0";

/// Bytes typed on the console that wait for the guest to read them. More are refused, see
/// [`Devices::push_input`].
const INPUT_QUEUE_BYTES: usize = 64;

/// The kernel is told about whole pages of memory only.
const PAGE_BYTES: usize = 4096;

/// The length of the header at the start of a RISC-V kernel image.
pub const IMAGE_HEADER_BYTES: usize = 64;

/// The devices on the bus: the UART and the power switch.
#[derive(Debug, Default)]
pub struct Devices {
    /// Console input that the guest has not read yet.
    input: VecDeque<u8>,
    /// Console output that [`Devices::take_output`] has not handed out yet.
    output: Vec<u8>,
    /// The guest is setting the baud rate: what it stores in the data register is half of the
    /// divisor, not a character to print.
    setting_divisor: bool,
}

impl Devices {
    /// Devices with nothing typed and nothing printed.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            input: VecDeque::new(),
            output: Vec::new(),
            setting_divisor: false,
        }
    }

    /// How many more bytes [`push_input`](Self::push_input) takes right now.
    #[must_use]
    pub fn input_space(&self) -> usize {
        INPUT_QUEUE_BYTES.saturating_sub(self.input.len())
    }

    /// Gives console input to the guest. Returns how many bytes were taken: the rest did not fit
    /// (the guest has not read what it has) and should be offered again later.
    pub fn push_input(&mut self, bytes: &[u8]) -> usize {
        let taken = bytes.len().min(self.input_space());
        self.input.extend(&bytes[..taken]);
        taken
    }

    /// Calls `sink` with what the guest has printed since the last call, if anything.
    pub fn take_output(&mut self, sink: impl FnOnce(&[u8])) {
        if !self.output.is_empty() {
            sink(&self.output);
            self.output.clear();
        }
    }
}

impl Bus for Devices {
    fn load(&mut self, address: u32) -> u32 {
        match address {
            UART_BASE if !self.setting_divisor => self.input.pop_front().map_or(0, u32::from),
            UART_LINE_STATUS => {
                UART_TRANSMIT_READY
                    | if self.input.is_empty() {
                        0
                    } else {
                        UART_DATA_READY
                    }
            }
            _ => 0,
        }
    }

    fn store(&mut self, address: u32, value: u32) -> Option<Signal> {
        match (address, value) {
            (UART_BASE, _) => {
                if !self.setting_divisor {
                    self.output.push(value.to_le_bytes()[0]);
                }
                None
            }
            (UART_LINE_CONTROL, _) => {
                self.setting_divisor = value & UART_DIVISOR_LATCH_ACCESS != 0;
                None
            }
            (SYSCON_BASE, SYSCON_POWER_OFF) => Some(Signal::PowerOff),
            (SYSCON_BASE, SYSCON_REBOOT) => Some(Signal::Reboot),
            _ => None,
        }
    }
}

/// The device tree of the machine, for a kernel that may use `memory_bytes` of RAM.
#[must_use]
pub fn device_tree(memory_bytes: u32, boot_args: &str) -> Vec<u8> {
    /// Handles by which nodes refer to each other.
    const CPU: u32 = 1;
    const CPU_INTERRUPTS: u32 = 2;
    const SYSCON: u32 = 3;
    /// The numbers of the machine software and machine timer interrupts of the processor.
    const SOFTWARE_INTERRUPT: u32 = 3;
    const TIMER_INTERRUPT: u32 = 7;

    let mut tree = Fdt::new();
    tree.begin_node("");
    tree.prop_cells("#address-cells", &[2]);
    tree.prop_cells("#size-cells", &[2]);
    tree.prop_str("compatible", "riscv-minimal-nommu");
    tree.prop_str("model", "riscv-minimal-nommu,pulsar");

    tree.begin_node("chosen");
    tree.prop_str("bootargs", boot_args);
    tree.end_node();

    tree.begin_node("memory@80000000");
    tree.prop_str("device_type", "memory");
    tree.prop_cells("reg", &[0, RAM_BASE, 0, memory_bytes]);
    tree.end_node();

    tree.begin_node("cpus");
    tree.prop_cells("#address-cells", &[1]);
    tree.prop_cells("#size-cells", &[0]);
    tree.prop_cells("timebase-frequency", &[TIMEBASE_HZ]);
    tree.begin_node("cpu@0");
    tree.prop_cells("phandle", &[CPU]);
    tree.prop_str("device_type", "cpu");
    tree.prop_cells("reg", &[0]);
    tree.prop_str("status", "okay");
    tree.prop_str("compatible", "riscv");
    tree.prop_str("riscv,isa", "rv32ima");
    tree.prop_str("mmu-type", "riscv,none");
    tree.begin_node("interrupt-controller");
    tree.prop_cells("#interrupt-cells", &[1]);
    tree.prop_empty("interrupt-controller");
    tree.prop_str("compatible", "riscv,cpu-intc");
    tree.prop_cells("phandle", &[CPU_INTERRUPTS]);
    tree.end_node();
    tree.end_node();
    tree.begin_node("cpu-map");
    tree.begin_node("cluster0");
    tree.begin_node("core0");
    tree.prop_cells("cpu", &[CPU]);
    tree.end_node();
    tree.end_node();
    tree.end_node();
    tree.end_node();

    tree.begin_node("soc");
    tree.prop_cells("#address-cells", &[2]);
    tree.prop_cells("#size-cells", &[2]);
    tree.prop_str("compatible", "simple-bus");
    tree.prop_empty("ranges");

    tree.begin_node("uart@10000000");
    tree.prop_cells("clock-frequency", &[0x0100_0000]);
    tree.prop_cells("reg", &[0, UART_BASE, 0, 0x100]);
    tree.prop_str("compatible", "ns16850");
    tree.end_node();

    tree.begin_node("poweroff");
    tree.prop_cells("value", &[SYSCON_POWER_OFF]);
    tree.prop_cells("offset", &[0]);
    tree.prop_cells("regmap", &[SYSCON]);
    tree.prop_str("compatible", "syscon-poweroff");
    tree.end_node();

    tree.begin_node("reboot");
    tree.prop_cells("value", &[SYSCON_REBOOT]);
    tree.prop_cells("offset", &[0]);
    tree.prop_cells("regmap", &[SYSCON]);
    tree.prop_str("compatible", "syscon-reboot");
    tree.end_node();

    tree.begin_node("syscon@11100000");
    tree.prop_cells("phandle", &[SYSCON]);
    tree.prop_cells("reg", &[0, SYSCON_BASE, 0, 0x1000]);
    tree.prop_str("compatible", "syscon");
    tree.end_node();

    tree.begin_node("clint@11000000");
    tree.prop_cells(
        "interrupts-extended",
        &[
            CPU_INTERRUPTS,
            SOFTWARE_INTERRUPT,
            CPU_INTERRUPTS,
            TIMER_INTERRUPT,
        ],
    );
    tree.prop_cells("reg", &[0, CLINT_BASE, 0, 0x0001_0000]);
    tree.prop_strs("compatible", &["sifive,clint0", "riscv,clint0"]);
    tree.end_node();

    tree.finish()
}

/// What can keep a kernel from being started.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BootError {
    /// The data does not start with the header of a RISC-V Linux kernel image.
    NotAKernelImage,
    /// The kernel needs more memory than there is, in bytes, for itself alone.
    TooLittleRam {
        /// What the kernel says it occupies once it runs (its code, data and zeroed data).
        needed: u64,
        /// What the machine can give it.
        available: u64,
    },
}

impl core::fmt::Display for BootError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NotAKernelImage => f.write_str("not a RISC-V Linux kernel image"),
            Self::TooLittleRam { needed, available } => write!(
                f,
                "the kernel needs {needed} bytes of RAM and the machine has {available}"
            ),
        }
    }
}

impl core::error::Error for BootError {}

/// How many bytes a kernel occupies in RAM once it runs, read from the header at the start of
/// its image (`Documentation/arch/riscv/boot-image-header.rst` in the Linux sources). That is
/// more than the image file holds: the kernel's zero-initialised data is not in the file.
///
/// # Errors
///
/// Fails if `header` is not the start of a RISC-V Linux kernel image.
pub fn kernel_size(header: &[u8]) -> Result<u64, BootError> {
    /// "RSC" and the byte 5, at offset 0x38: the mark of a RISC-V kernel image.
    const MAGIC: [u8; 4] = *b"RSC\x05";
    let magic = header.get(0x38..0x3c).ok_or(BootError::NotAKernelImage)?;
    let size = header
        .get(0x10..)
        .and_then(<[u8]>::first_chunk::<8>)
        .ok_or(BootError::NotAKernelImage)?;
    if magic != MAGIC {
        return Err(BootError::NotAKernelImage);
    }
    Ok(u64::from_le_bytes(*size))
}

/// Gets a machine ready to start the kernel whose image is at the start of `ram`: writes the
/// device tree into `ram`, and returns a processor that is about to execute the first
/// instruction of the kernel, with the address of the device tree in `a1`.
///
/// The device tree goes right behind the kernel (behind its zeroed data, which is not in the
/// image), and the kernel is told about all of `ram`, down to the last whole page. The kernel
/// keeps the tree's page for itself. This placement matters on a small machine: a no-MMU kernel
/// loads every program into one contiguous block, here of 1 MiB, and a tree at the top of RAM
/// would cut the last megabyte in two.
///
/// # Errors
///
/// Fails if `ram` does not start with a kernel image, or is too small for that kernel.
pub fn boot(ram: &mut [u8], boot_args: &str) -> Result<Cpu, BootError> {
    let needed = kernel_size(ram)?;
    let memory_bytes = ram.len() / PAGE_BYTES * PAGE_BYTES;
    let too_little = BootError::TooLittleRam {
        needed,
        available: memory_bytes as u64,
    };
    let memory = u32::try_from(memory_bytes).map_err(|_| too_little)?;
    let tree = device_tree(memory, boot_args);

    let tree_offset = usize::try_from(needed)
        .ok()
        .and_then(|kernel_end| kernel_end.checked_next_multiple_of(PAGE_BYTES))
        .ok_or(too_little)?;
    let tree_address = u32::try_from(tree_offset)
        .ok()
        .and_then(|offset| RAM_BASE.checked_add(offset))
        .ok_or(too_little)?;
    ram.get_mut(..memory_bytes)
        .and_then(|memory| memory.get_mut(tree_offset..)?.get_mut(..tree.len()))
        .ok_or(too_little)?
        .copy_from_slice(&tree);
    Ok(Cpu::new(RAM_BASE, tree_address))
}
