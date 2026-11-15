//! A RISC-V processor in software: RV32IMA with Zicsr, machine and user mode, and the timer of a
//! CLINT. It is what a Linux kernel built for a small no-MMU machine needs, and nothing more.
//!
//! The core owns the registers and the timer. Memory is a byte slice the caller lends to
//! [`Cpu::run`] (it starts at [`RAM_BASE`] in the guest's address space), and every device other
//! than the timer sits behind the [`Bus`] trait.
//!
//! What is implemented, from the RISC-V specifications (unprivileged 20191213, privileged 1.12):
//!
//! - RV32I, M (multiply and divide), A (`LR`/`SC` and the atomic memory operations), `FENCE` as
//!   a no-operation, and the CSR instructions.
//! - Machine mode and user mode, traps with direct `mtvec`, `MRET`, `WFI`, `ECALL`, `EBREAK`.
//! - One interrupt: the machine timer (`mtime` and `mtimecmp`, counted in microseconds).
//! - No compressed instructions, no floating point, no supervisor mode, no memory protection.
//!
//! Unknown CSRs read as zero and ignore writes instead of trapping. A kernel probes a few CSRs at
//! boot, and this is how the reference machine of this kind (`mini-rv32ima`) answers.
//!
//! This file imports nothing from the crate, so it is tested on the host:
//!
//! ```sh
//! rustc --edition 2024 --test src/linux/cpu.rs -o /tmp/t && /tmp/t
//! ```

/// The guest address of the first byte of RAM.
pub const RAM_BASE: u32 = 0x8000_0000;

/// The first address of the device area. Loads and stores from here up to [`MMIO_END`] go to the
/// timer or the [`Bus`]; everything else outside RAM is an access fault.
pub const MMIO_BASE: u32 = 0x1000_0000;

/// The end (exclusive) of the device area.
pub const MMIO_END: u32 = 0x1200_0000;

/// The address of the CLINT (the timer). `mtimecmp` is at offset 0x4000 and `mtime` at 0xbff8,
/// each 64 bits wide in two words: the layout that RISC-V machines commonly use.
pub const CLINT_BASE: u32 = 0x1100_0000;

const CLINT_MTIMECMP: u32 = CLINT_BASE + 0x4000;
const CLINT_MTIME: u32 = CLINT_BASE + 0xbff8;

/// How often `mtime` counts: once per microsecond.
pub const TIMEBASE_HZ: u32 = 1_000_000;

const MSTATUS_MIE: u32 = 1 << 3;
const MSTATUS_MPIE: u32 = 1 << 7;
const MSTATUS_MPP_SHIFT: u32 = 11;
const MSTATUS_MPP: u32 = 0b11 << MSTATUS_MPP_SHIFT;
/// The bit of the machine timer interrupt in `mip` and `mie`.
const MTI: u32 = 1 << 7;
/// The interrupt bits that exist in `mie`: software, timer, external.
const MIE_WRITABLE: u32 = (1 << 3) | MTI | (1 << 11);

/// `misa`: 32 bits, with the extensions A, I and M.
const MISA: u32 = (1 << 30) | (1 << 0) | (1 << 8) | (1 << 12);

const MACHINE: u8 = 3;
const USER: u8 = 0;

/// Exception codes (`mcause` without the interrupt bit).
mod cause {
    pub const FETCH_MISALIGNED: u32 = 0;
    pub const FETCH_ACCESS: u32 = 1;
    pub const ILLEGAL_INSTRUCTION: u32 = 2;
    pub const BREAKPOINT: u32 = 3;
    pub const LOAD_ACCESS: u32 = 5;
    pub const STORE_ACCESS: u32 = 7;
    pub const ECALL_FROM_USER: u32 = 8;
    pub const ECALL_FROM_MACHINE: u32 = 11;
    /// The machine timer interrupt, with the interrupt bit.
    pub const MACHINE_TIMER: u32 = 0x8000_0007;
}

/// What a device asks the machine to do, as the result of a store.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signal {
    /// Stop the machine.
    PowerOff,
    /// Start the machine again from the beginning.
    Reboot,
}

/// The devices of the machine, apart from the timer. Addresses are guest addresses between
/// [`MMIO_BASE`] and [`MMIO_END`].
pub trait Bus {
    /// A load from a device register. Unknown registers read as zero.
    fn load(&mut self, address: u32) -> u32;
    /// A store to a device register. Returns a [`Signal`] if the store asks the machine to stop.
    fn store(&mut self, address: u32, value: u32) -> Option<Signal>;
}

/// Why [`Cpu::run`] came back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exit {
    /// The instruction budget is used up. Call again.
    Budget,
    /// The processor waits for an interrupt (`WFI`) and none is pending. Nothing happens until
    /// the timer fires, see [`Cpu::micros_until_timer`].
    Idle,
    /// A device asked for the machine to be powered off.
    PowerOff,
    /// A device asked for the machine to be restarted.
    Reboot,
}

/// A trap that an instruction raised: the exception code and the value for `mtval`.
struct Trap {
    cause: u32,
    value: u32,
}

impl Trap {
    const fn illegal(instruction: u32) -> Self {
        Self {
            cause: cause::ILLEGAL_INSTRUCTION,
            value: instruction,
        }
    }
}

/// How execution goes on after an instruction that did not trap.
enum Flow {
    /// Go on with the next instruction.
    Next,
    /// Go on, but the instruction may have enabled an interrupt that is pending: look first.
    Recheck,
    /// `WFI`: stop until an interrupt is pending.
    Wait,
    /// A device asked for the machine to stop.
    Stop(Signal),
}

/// The state of the processor.
#[derive(Clone, Debug)]
pub struct Cpu {
    regs: [u32; 32],
    pc: u32,
    mstatus: u32,
    mie: u32,
    mtvec: u32,
    mscratch: u32,
    mepc: u32,
    mcause: u32,
    mtval: u32,
    /// [`MACHINE`] or [`USER`].
    privilege: u8,
    /// Stopped by `WFI` until an interrupt is pending.
    waiting: bool,
    /// The address of the last `LR`, until the next `SC`.
    reservation: Option<u32>,
    /// Instructions executed.
    cycles: u64,
    /// `mtime`, in microseconds.
    time: u64,
    /// `mtimecmp`. Zero means "not set": the timer does not fire.
    timer_match: u64,
}

/// The bytes of `N` bytes of RAM at guest address `address`, or `None` if they are not all in RAM.
#[inline]
fn ram_ref<const N: usize>(ram: &[u8], address: u32) -> Option<&[u8; N]> {
    let offset = usize::try_from(address.wrapping_sub(RAM_BASE)).ok()?;
    ram.get(offset..)?.first_chunk()
}

/// Like [`ram_ref`], for writing.
#[inline]
fn ram_mut<const N: usize>(ram: &mut [u8], address: u32) -> Option<&mut [u8; N]> {
    let offset = usize::try_from(address.wrapping_sub(RAM_BASE)).ok()?;
    ram.get_mut(offset..)?.first_chunk_mut()
}

/// Whether `address` is in the device area.
#[inline]
const fn is_mmio(address: u32) -> bool {
    MMIO_BASE <= address && address < MMIO_END
}

/// The low or high half of a 64-bit register, picked by bit 2 of the address.
#[inline]
const fn half(value: u64, address: u32) -> u32 {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "taking one 32-bit half is the point"
    )]
    if address & 4 == 0 {
        value as u32
    } else {
        (value >> 32) as u32
    }
}

/// `value` with one 32-bit half replaced, picked by bit 2 of the address.
#[inline]
const fn with_half(value: u64, address: u32, half: u32) -> u64 {
    if address & 4 == 0 {
        (value & 0xffff_ffff_0000_0000) | half as u64
    } else {
        (value & 0x0000_0000_ffff_ffff) | ((half as u64) << 32)
    }
}

/// The sign-extended 12-bit immediate of an I-type instruction.
#[inline]
const fn imm_i(instruction: u32) -> u32 {
    (instruction.cast_signed() >> 20).cast_unsigned()
}

/// The sign-extended 12-bit immediate of an S-type instruction.
#[inline]
const fn imm_s(instruction: u32) -> u32 {
    ((instruction & 0xfe00_0000).cast_signed() >> 20).cast_unsigned() | ((instruction >> 7) & 0x1f)
}

/// The sign-extended branch offset of a B-type instruction.
#[inline]
const fn imm_b(instruction: u32) -> u32 {
    ((instruction & 0x8000_0000).cast_signed() >> 19).cast_unsigned()
        | ((instruction & 0x80) << 4)
        | ((instruction >> 20) & 0x7e0)
        | ((instruction >> 7) & 0x1e)
}

/// The sign-extended jump offset of a J-type instruction.
#[inline]
const fn imm_j(instruction: u32) -> u32 {
    ((instruction & 0x8000_0000).cast_signed() >> 11).cast_unsigned()
        | (instruction & 0x000f_f000)
        | ((instruction >> 9) & 0x800)
        | ((instruction >> 20) & 0x7fe)
}

impl Cpu {
    /// A processor after reset: machine mode, interrupts off, about to execute at `entry`, with
    /// `a0` and `a1` set as a Linux kernel expects them (the hart number, zero, and the address of
    /// the device tree).
    #[must_use]
    pub const fn new(entry: u32, device_tree: u32) -> Self {
        let mut regs = [0; 32];
        regs[11] = device_tree;
        Self {
            regs,
            pc: entry,
            mstatus: 0,
            mie: 0,
            mtvec: 0,
            mscratch: 0,
            mepc: 0,
            mcause: 0,
            mtval: 0,
            privilege: MACHINE,
            waiting: false,
            reservation: None,
            cycles: 0,
            time: 0,
            timer_match: 0,
        }
    }

    /// The address of the next instruction.
    #[must_use]
    pub const fn pc(&self) -> u32 {
        self.pc
    }

    /// Register `x0` to `x31` (the index is taken modulo 32).
    #[must_use]
    pub const fn reg(&self, index: usize) -> u32 {
        self.regs[index % 32]
    }

    /// Instructions executed since reset.
    #[must_use]
    pub const fn cycles(&self) -> u64 {
        self.cycles
    }

    /// Microseconds until the timer fires, `Some(0)` if it is due, or `None` if it is not set.
    /// After [`Exit::Idle`] this is how long the caller may sleep.
    #[must_use]
    pub const fn micros_until_timer(&self) -> Option<u64> {
        if self.timer_match == 0 {
            None
        } else {
            Some(self.timer_match.saturating_sub(self.time))
        }
    }

    /// `mip`: only the timer interrupt exists, and it is pending while `mtime >= mtimecmp`.
    #[inline]
    const fn mip(&self) -> u32 {
        if self.timer_match != 0 && self.time >= self.timer_match {
            MTI
        } else {
            0
        }
    }

    /// Whether an interrupt is pending, enabled, and may be taken now. In machine mode that needs
    /// `mstatus.MIE`; user mode cannot keep machine interrupts out.
    #[inline]
    const fn interrupt_due(&self) -> bool {
        self.mip() & self.mie != 0 && (self.privilege < MACHINE || self.mstatus & MSTATUS_MIE != 0)
    }

    /// Runs for at most `budget` instructions. `elapsed_micros` is the time that has passed since
    /// the previous call; it advances `mtime`. The clock only moves here, so the timer can only
    /// become due at the start of a call: keep `budget` small enough for it to be served on time
    /// (a few thousand). An interrupt that is already pending is taken as soon as the guest
    /// enables it, in the middle of a call too.
    pub fn run(
        &mut self,
        ram: &mut [u8],
        bus: &mut impl Bus,
        elapsed_micros: u32,
        budget: u32,
    ) -> Exit {
        self.time = self.time.wrapping_add(u64::from(elapsed_micros));

        if self.waiting {
            // WFI ends when an enabled interrupt is pending, even while they are globally off.
            if self.mip() & self.mie == 0 {
                return Exit::Idle;
            }
            self.waiting = false;
        }
        if self.interrupt_due() {
            self.enter_trap(cause::MACHINE_TIMER, 0);
        }

        for _ in 0..budget {
            self.cycles = self.cycles.wrapping_add(1);
            match self.step(ram, bus) {
                Ok(Flow::Next) => {}
                Ok(Flow::Recheck) => {
                    if self.interrupt_due() {
                        self.enter_trap(cause::MACHINE_TIMER, 0);
                    }
                }
                Ok(Flow::Wait) => {
                    // With an interrupt already pending there is nothing to wait for.
                    if self.mip() & self.mie == 0 {
                        self.waiting = true;
                        return Exit::Idle;
                    }
                }
                Ok(Flow::Stop(Signal::PowerOff)) => return Exit::PowerOff,
                Ok(Flow::Stop(Signal::Reboot)) => return Exit::Reboot,
                Err(trap) => self.enter_trap(trap.cause, trap.value),
            }
        }
        Exit::Budget
    }

    /// Takes a trap: saves where and why in the machine CSRs, turns interrupts off and jumps to
    /// the handler in `mtvec`. `mepc` is the instruction that trapped, or for an interrupt the
    /// one that would have run next; both are the current `pc`.
    fn enter_trap(&mut self, cause: u32, value: u32) {
        self.mcause = cause;
        self.mtval = value;
        self.mepc = self.pc;
        let previous_mie = (self.mstatus & MSTATUS_MIE) << 4;
        self.mstatus = (self.mstatus & !(MSTATUS_MIE | MSTATUS_MPIE | MSTATUS_MPP))
            | previous_mie
            | (u32::from(self.privilege) << MSTATUS_MPP_SHIFT);
        self.privilege = MACHINE;
        self.pc = self.mtvec & !0b11;
    }

    #[inline]
    fn set_reg(&mut self, index: u32, value: u32) {
        if index != 0 {
            self.regs[index as usize] = value;
        }
    }

    /// Executes the instruction at `pc`.
    #[inline]
    fn step(&mut self, ram: &mut [u8], bus: &mut impl Bus) -> Result<Flow, Trap> {
        let pc = self.pc;
        if pc & 0b11 != 0 {
            return Err(Trap {
                cause: cause::FETCH_MISALIGNED,
                value: pc,
            });
        }
        let Some(bytes) = ram_ref::<4>(ram, pc) else {
            return Err(Trap {
                cause: cause::FETCH_ACCESS,
                value: pc,
            });
        };
        let instruction = u32::from_le_bytes(*bytes);

        let rd = (instruction >> 7) & 0x1f;
        let funct3 = (instruction >> 12) & 0b111;
        let rs1 = self.regs[((instruction >> 15) & 0x1f) as usize];
        let rs2 = self.regs[((instruction >> 20) & 0x1f) as usize];
        let mut next = pc.wrapping_add(4);

        match instruction & 0x7f {
            // LUI
            0x37 => self.set_reg(rd, instruction & 0xffff_f000),
            // AUIPC
            0x17 => self.set_reg(rd, pc.wrapping_add(instruction & 0xffff_f000)),
            // JAL
            0x6f => {
                self.set_reg(rd, next);
                next = pc.wrapping_add(imm_j(instruction));
            }
            // JALR
            0x67 if funct3 == 0 => {
                self.set_reg(rd, next);
                next = rs1.wrapping_add(imm_i(instruction)) & !1;
            }
            // BEQ, BNE, BLT, BGE, BLTU, BGEU
            0x63 => {
                let taken = match funct3 {
                    0 => rs1 == rs2,
                    1 => rs1 != rs2,
                    4 => rs1.cast_signed() < rs2.cast_signed(),
                    5 => rs1.cast_signed() >= rs2.cast_signed(),
                    6 => rs1 < rs2,
                    7 => rs1 >= rs2,
                    _ => return Err(Trap::illegal(instruction)),
                };
                if taken {
                    next = pc.wrapping_add(imm_b(instruction));
                }
            }
            // LB, LH, LW, LBU, LHU
            0x03 => {
                let address = rs1.wrapping_add(imm_i(instruction));
                let value = self.load(ram, bus, address, funct3, instruction)?;
                self.set_reg(rd, value);
            }
            // SB, SH, SW
            0x23 => {
                let address = rs1.wrapping_add(imm_s(instruction));
                let signal = self.store(ram, bus, address, rs2, funct3, instruction)?;
                if is_mmio(address) {
                    // The store has happened: `pc` moves on whatever comes of it. A store to
                    // the timer may have made its interrupt pending.
                    self.pc = next;
                    return Ok(signal.map_or(Flow::Recheck, Flow::Stop));
                }
            }
            // ADDI, SLTI, SLTIU, XORI, ORI, ANDI, SLLI, SRLI, SRAI
            0x13 => {
                let value = op_immediate(instruction, funct3, rs1)?;
                self.set_reg(rd, value);
            }
            // ADD, SUB, SLL, SLT, SLTU, XOR, SRL, SRA, OR, AND, and the M extension
            0x33 => {
                let value = op(instruction, funct3, rs1, rs2)?;
                self.set_reg(rd, value);
            }
            // FENCE, FENCE.I: one processor and no cache, so there is nothing to do.
            0x0f => {}
            // ECALL, EBREAK, MRET, WFI and the CSR instructions
            0x73 => {
                if funct3 == 0 {
                    return self.system(instruction);
                }
                if self.csr_instruction(instruction, funct3, rd, rs1)? {
                    self.pc = next;
                    return Ok(Flow::Recheck);
                }
            }
            // LR.W, SC.W and the atomic memory operations
            0x2f if funct3 == 2 => {
                let value = self.atomic(ram, instruction, rs1, rs2)?;
                self.set_reg(rd, value);
            }
            _ => return Err(Trap::illegal(instruction)),
        }

        self.pc = next;
        Ok(Flow::Next)
    }

    /// A load of the width `funct3` names.
    #[inline]
    fn load(
        &mut self,
        ram: &[u8],
        bus: &mut impl Bus,
        address: u32,
        funct3: u32,
        instruction: u32,
    ) -> Result<u32, Trap> {
        let fault = Trap {
            cause: cause::LOAD_ACCESS,
            value: address,
        };
        let from_ram = match funct3 {
            0 | 4 => ram_ref::<1>(ram, address).map(|bytes| u32::from(bytes[0])),
            1 | 5 => ram_ref::<2>(ram, address).map(|bytes| u32::from(u16::from_le_bytes(*bytes))),
            2 => ram_ref::<4>(ram, address).map(|bytes| u32::from_le_bytes(*bytes)),
            _ => return Err(Trap::illegal(instruction)),
        };
        let raw = match from_ram {
            Some(value) => value,
            None if is_mmio(address) => self.device_load(bus, address),
            None => return Err(fault),
        };
        Ok(match funct3 {
            0 => (raw.cast_signed() << 24 >> 24).cast_unsigned(),
            1 => (raw.cast_signed() << 16 >> 16).cast_unsigned(),
            4 => raw & 0xff,
            5 => raw & 0xffff,
            _ => raw,
        })
    }

    /// A store of the width `funct3` names.
    #[inline]
    fn store(
        &mut self,
        ram: &mut [u8],
        bus: &mut impl Bus,
        address: u32,
        value: u32,
        funct3: u32,
        instruction: u32,
    ) -> Result<Option<Signal>, Trap> {
        let bytes = value.to_le_bytes();
        let stored = match funct3 {
            0 => ram_mut::<1>(ram, address).map(|slot| slot[0] = bytes[0]),
            1 => ram_mut::<2>(ram, address).map(|slot| slot.copy_from_slice(&bytes[..2])),
            2 => ram_mut::<4>(ram, address).map(|slot| *slot = bytes),
            _ => return Err(Trap::illegal(instruction)),
        };
        if stored.is_some() {
            return Ok(None);
        }
        if !is_mmio(address) {
            return Err(Trap {
                cause: cause::STORE_ACCESS,
                value: address,
            });
        }
        let value = match funct3 {
            0 => value & 0xff,
            1 => value & 0xffff,
            _ => value,
        };
        Ok(self.device_store(bus, address, value))
    }

    /// A load from the device area: the timer is here, the rest is on the bus.
    fn device_load(&mut self, bus: &mut impl Bus, address: u32) -> u32 {
        match address & !0b111 {
            CLINT_MTIME => half(self.time, address),
            CLINT_MTIMECMP => half(self.timer_match, address),
            _ => bus.load(address),
        }
    }

    /// A store to the device area. `mtime` cannot be written.
    fn device_store(&mut self, bus: &mut impl Bus, address: u32, value: u32) -> Option<Signal> {
        match address & !0b111 {
            CLINT_MTIMECMP => {
                self.timer_match = with_half(self.timer_match, address, value);
                None
            }
            CLINT_MTIME => None,
            _ => bus.store(address, value),
        }
    }

    /// `ECALL`, `EBREAK`, `MRET` and `WFI`.
    fn system(&mut self, instruction: u32) -> Result<Flow, Trap> {
        match instruction {
            // ECALL
            0x0000_0073 => Err(Trap {
                cause: if self.privilege == MACHINE {
                    cause::ECALL_FROM_MACHINE
                } else {
                    cause::ECALL_FROM_USER
                },
                value: 0,
            }),
            // EBREAK
            0x0010_0073 => Err(Trap {
                cause: cause::BREAKPOINT,
                value: self.pc,
            }),
            // MRET: back to the mode and the interrupt state from before the trap.
            0x3020_0073 if self.privilege == MACHINE => {
                let previous = (self.mstatus & MSTATUS_MPP) >> MSTATUS_MPP_SHIFT;
                let previous_mie = (self.mstatus & MSTATUS_MPIE) >> 4;
                self.mstatus =
                    (self.mstatus & !(MSTATUS_MIE | MSTATUS_MPP)) | previous_mie | MSTATUS_MPIE;
                self.privilege = if previous == u32::from(MACHINE) {
                    MACHINE
                } else {
                    USER
                };
                self.pc = self.mepc;
                Ok(Flow::Recheck)
            }
            // WFI
            0x1050_0073 if self.privilege == MACHINE => {
                self.pc = self.pc.wrapping_add(4);
                Ok(Flow::Wait)
            }
            _ => Err(Trap::illegal(instruction)),
        }
    }

    /// `CSRRW`, `CSRRS`, `CSRRC` and their immediate forms. Returns whether the instruction
    /// wrote a CSR that decides which interrupts are taken (`mstatus` or `mie`).
    fn csr_instruction(
        &mut self,
        instruction: u32,
        funct3: u32,
        rd: u32,
        rs1: u32,
    ) -> Result<bool, Trap> {
        let number = instruction >> 20;
        // Bits 9:8 of a CSR number are the lowest privilege that may use it.
        if (number >> 8) & 0b11 > u32::from(self.privilege) {
            return Err(Trap::illegal(instruction));
        }
        let field = (instruction >> 15) & 0x1f;
        let operand = if funct3 & 0b100 == 0 { rs1 } else { field };
        let old = self.csr_read(number);
        let (new, writes) = match funct3 & 0b11 {
            1 => (operand, true),
            // Set and clear do not write at all when the source is x0 or the immediate is 0.
            2 => (old | operand, field != 0),
            3 => (old & !operand, field != 0),
            _ => return Err(Trap::illegal(instruction)),
        };
        if writes {
            self.csr_write(number, new);
        }
        self.set_reg(rd, old);
        Ok(writes && matches!(number, 0x300 | 0x304))
    }

    fn csr_read(&self, number: u32) -> u32 {
        match number {
            0x300 => self.mstatus,
            0x301 => MISA,
            0x304 => self.mie,
            0x305 => self.mtvec,
            0x340 => self.mscratch,
            0x341 => self.mepc,
            0x342 => self.mcause,
            0x343 => self.mtval,
            0x344 => self.mip(),
            // cycle, instret and their machine-mode names: instructions executed.
            0xc00 | 0xc02 | 0xb00 | 0xb02 => half(self.cycles, 0),
            0xc80 | 0xc82 | 0xb80 | 0xb82 => half(self.cycles, 4),
            // time
            0xc01 => half(self.time, 0),
            0xc81 => half(self.time, 4),
            _ => 0,
        }
    }

    fn csr_write(&mut self, number: u32, value: u32) {
        match number {
            0x300 => {
                let writable = MSTATUS_MIE | MSTATUS_MPIE | MSTATUS_MPP;
                self.mstatus = (self.mstatus & !writable) | (value & writable);
            }
            0x304 => self.mie = value & MIE_WRITABLE,
            0x305 => self.mtvec = value,
            0x340 => self.mscratch = value,
            0x341 => self.mepc = value & !0b11,
            0x342 => self.mcause = value,
            0x343 => self.mtval = value,
            _ => {}
        }
    }

    /// `LR.W`, `SC.W` and the atomic memory operations, on RAM only. Returns the value for `rd`.
    fn atomic(
        &mut self,
        ram: &mut [u8],
        instruction: u32,
        address: u32,
        source: u32,
    ) -> Result<u32, Trap> {
        let operation = instruction >> 27;
        let Some(slot) = ram_mut::<4>(ram, address) else {
            return Err(Trap {
                cause: if operation == 0b00010 {
                    cause::LOAD_ACCESS
                } else {
                    cause::STORE_ACCESS
                },
                value: address,
            });
        };
        let old = u32::from_le_bytes(*slot);
        let new = match operation {
            // LR.W
            0b00010 => {
                self.reservation = Some(address);
                return Ok(old);
            }
            // SC.W: succeeds (rd = 0) only on the address of the last LR.W.
            0b00011 => {
                if self.reservation.take() != Some(address) {
                    return Ok(1);
                }
                *slot = source.to_le_bytes();
                return Ok(0);
            }
            0b00001 => source,
            0b00000 => old.wrapping_add(source),
            0b00100 => old ^ source,
            0b01100 => old & source,
            0b01000 => old | source,
            0b10000 => old.cast_signed().min(source.cast_signed()).cast_unsigned(),
            0b10100 => old.cast_signed().max(source.cast_signed()).cast_unsigned(),
            0b11000 => old.min(source),
            0b11100 => old.max(source),
            _ => return Err(Trap::illegal(instruction)),
        };
        *slot = new.to_le_bytes();
        Ok(old)
    }
}

/// An operation on a register and an immediate. Returns the result.
#[inline]
fn op_immediate(instruction: u32, funct3: u32, rs1: u32) -> Result<u32, Trap> {
    let imm = imm_i(instruction);
    let shift = imm & 0x1f;
    Ok(match (funct3, instruction >> 25) {
        (0, _) => rs1.wrapping_add(imm),
        (1, 0x00) => rs1 << shift,
        (2, _) => u32::from(rs1.cast_signed() < imm.cast_signed()),
        (3, _) => u32::from(rs1 < imm),
        (4, _) => rs1 ^ imm,
        (5, 0x00) => rs1 >> shift,
        (5, 0x20) => (rs1.cast_signed() >> shift).cast_unsigned(),
        (6, _) => rs1 | imm,
        (7, _) => rs1 & imm,
        _ => return Err(Trap::illegal(instruction)),
    })
}

/// An operation on two registers. Returns the result.
#[inline]
fn op(instruction: u32, funct3: u32, rs1: u32, rs2: u32) -> Result<u32, Trap> {
    Ok(match (instruction >> 25, funct3) {
        (0x00, 0) => rs1.wrapping_add(rs2),
        (0x20, 0) => rs1.wrapping_sub(rs2),
        (0x00, 1) => rs1 << (rs2 & 0x1f),
        (0x00, 2) => u32::from(rs1.cast_signed() < rs2.cast_signed()),
        (0x00, 3) => u32::from(rs1 < rs2),
        (0x00, 4) => rs1 ^ rs2,
        (0x00, 5) => rs1 >> (rs2 & 0x1f),
        (0x20, 5) => (rs1.cast_signed() >> (rs2 & 0x1f)).cast_unsigned(),
        (0x00, 6) => rs1 | rs2,
        (0x00, 7) => rs1 & rs2,
        (0x01, _) => multiply_divide(funct3, rs1, rs2),
        _ => return Err(Trap::illegal(instruction)),
    })
}

/// The M extension. Division by zero and the one overflow (`i32::MIN / -1`) have defined
/// results instead of trapping.
#[inline]
fn multiply_divide(funct3: u32, a: u32, b: u32) -> u32 {
    let (signed_a, signed_b) = (a.cast_signed(), b.cast_signed());
    match funct3 {
        // MUL
        0 => a.wrapping_mul(b),
        // MULH
        1 => half(
            (i64::from(signed_a) * i64::from(signed_b)).cast_unsigned(),
            4,
        ),
        // MULHSU
        2 => half((i64::from(signed_a) * i64::from(b)).cast_unsigned(), 4),
        // MULHU
        3 => half(u64::from(a) * u64::from(b), 4),
        // DIV and DIVU by zero give all ones, REM and REMU by zero give the dividend.
        4 | 5 if b == 0 => u32::MAX,
        _ if b == 0 => a,
        // DIV
        4 => signed_a.wrapping_div(signed_b).cast_unsigned(),
        // DIVU
        5 => a / b,
        // REM
        6 => signed_a.wrapping_rem(signed_b).cast_unsigned(),
        // REMU
        _ => a % b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bus with one register that remembers what was stored, and a power switch.
    #[derive(Default)]
    struct TestBus {
        last_store: Option<(u32, u32)>,
    }

    impl Bus for TestBus {
        fn load(&mut self, address: u32) -> u32 {
            address & 0xff
        }

        fn store(&mut self, address: u32, value: u32) -> Option<Signal> {
            self.last_store = Some((address, value));
            (address == 0x1110_0000).then_some(Signal::PowerOff)
        }
    }

    // A small assembler: one function per instruction format.
    fn r(funct7: u32, rs2: u32, rs1: u32, funct3: u32, rd: u32, opcode: u32) -> u32 {
        (funct7 << 25) | (rs2 << 20) | (rs1 << 15) | (funct3 << 12) | (rd << 7) | opcode
    }
    fn i(imm: i32, rs1: u32, funct3: u32, rd: u32, opcode: u32) -> u32 {
        ((imm.cast_unsigned() & 0xfff) << 20) | (rs1 << 15) | (funct3 << 12) | (rd << 7) | opcode
    }
    fn s(imm: i32, rs2: u32, rs1: u32, funct3: u32) -> u32 {
        let imm = imm.cast_unsigned();
        ((imm & 0xfe0) << 20)
            | (rs2 << 20)
            | (rs1 << 15)
            | (funct3 << 12)
            | ((imm & 0x1f) << 7)
            | 0x23
    }
    fn b(offset: i32, rs2: u32, rs1: u32, funct3: u32) -> u32 {
        let imm = offset.cast_unsigned();
        ((imm & 0x1000) << 19)
            | ((imm & 0x7e0) << 20)
            | (rs2 << 20)
            | (rs1 << 15)
            | (funct3 << 12)
            | ((imm & 0x1e) << 7)
            | ((imm & 0x800) >> 4)
            | 0x63
    }
    fn u(imm: u32, rd: u32, opcode: u32) -> u32 {
        (imm & 0xffff_f000) | (rd << 7) | opcode
    }
    fn jal(offset: i32, rd: u32) -> u32 {
        let imm = offset.cast_unsigned();
        ((imm & 0x10_0000) << 11)
            | ((imm & 0x7fe) << 20)
            | ((imm & 0x800) << 9)
            | (imm & 0xf_f000)
            | (rd << 7)
            | 0x6f
    }
    fn addi(rd: u32, rs1: u32, imm: i32) -> u32 {
        i(imm, rs1, 0, rd, 0x13)
    }
    fn csrrw(rd: u32, csr: u32, rs1: u32) -> u32 {
        (csr << 20) | (rs1 << 15) | (1 << 12) | (rd << 7) | 0x73
    }
    fn csrrs(rd: u32, csr: u32, rs1: u32) -> u32 {
        (csr << 20) | (rs1 << 15) | (2 << 12) | (rd << 7) | 0x73
    }
    /// Loads a full 32-bit constant into `rd` (LUI + ADDI).
    fn li(rd: u32, value: u32) -> [u32; 2] {
        let low = (value & 0xfff).cast_signed() << 20 >> 20;
        [
            u(value.wrapping_sub(low.cast_unsigned()), rd, 0x37),
            addi(rd, rd, low),
        ]
    }

    const ECALL: u32 = 0x0000_0073;
    const EBREAK: u32 = 0x0010_0073;
    const MRET: u32 = 0x3020_0073;
    const WFI: u32 = 0x1050_0073;
    const RAM_BYTES: usize = 4096;

    /// A machine with `program` at the start of RAM.
    fn machine(program: &[u32]) -> (Cpu, Vec<u8>, TestBus) {
        let mut ram = vec![0_u8; RAM_BYTES];
        for (index, word) in program.iter().enumerate() {
            ram[index * 4..index * 4 + 4].copy_from_slice(&word.to_le_bytes());
        }
        (Cpu::new(RAM_BASE, 0), ram, TestBus::default())
    }

    /// Runs `program` for as many instructions as it has and returns the processor.
    fn run(program: &[u32]) -> Cpu {
        let (mut cpu, mut ram, mut bus) = machine(program);
        let budget = u32::try_from(program.len()).unwrap();
        assert_eq!(cpu.run(&mut ram, &mut bus, 0, budget), Exit::Budget);
        cpu
    }

    /// The result in x3 of one register-register operation on `a` and `b`.
    fn alu(funct7: u32, funct3: u32, a: u32, b: u32) -> u32 {
        let [a0, a1] = li(1, a);
        let [b0, b1] = li(2, b);
        run(&[a0, a1, b0, b1, r(funct7, 2, 1, funct3, 3, 0x33)]).reg(3)
    }

    #[test]
    fn reset_state_is_what_a_kernel_expects() {
        let cpu = Cpu::new(RAM_BASE, 0x8012_3000);
        assert_eq!(cpu.pc(), RAM_BASE);
        assert_eq!(cpu.reg(10), 0);
        assert_eq!(cpu.reg(11), 0x8012_3000);
    }

    #[test]
    fn x0_is_always_zero() {
        assert_eq!(run(&[addi(0, 0, 5), addi(1, 0, 7)]).reg(0), 0);
    }

    #[test]
    fn immediates_are_sign_extended() {
        let cpu = run(&[
            addi(1, 0, -1),
            addi(2, 1, -2048),
            i(-1, 0, 4, 3, 0x13),
            i(-1, 0, 3, 4, 0x13),
        ]);
        assert_eq!(cpu.reg(1), u32::MAX);
        assert_eq!(cpu.reg(2), 0xffff_f7ff);
        assert_eq!(cpu.reg(3), u32::MAX); // XORI x0, -1
        assert_eq!(cpu.reg(4), 1); // SLTIU x0, -1: 0 < 0xffffffff
    }

    #[test]
    fn li_builds_every_constant() {
        for value in [
            0,
            1,
            0x7ff,
            0x800,
            0xfff,
            0x1234_5678,
            0x8000_0000,
            0xffff_f800,
            u32::MAX,
        ] {
            let [high, low] = li(5, value);
            assert_eq!(run(&[high, low]).reg(5), value, "{value:#x}");
        }
    }

    #[test]
    fn integer_operations() {
        assert_eq!(alu(0x00, 0, 7, u32::MAX), 6); // ADD wraps
        assert_eq!(alu(0x20, 0, 3, 5), (-2_i32).cast_unsigned()); // SUB
        assert_eq!(alu(0x00, 1, 1, 33), 2); // SLL uses the low 5 bits
        assert_eq!(alu(0x00, 2, (-1_i32).cast_unsigned(), 1), 1); // SLT
        assert_eq!(alu(0x00, 3, u32::MAX, 1), 0); // SLTU
        assert_eq!(alu(0x00, 4, 0b1100, 0b1010), 0b0110); // XOR
        assert_eq!(alu(0x00, 5, 0x8000_0000, 31), 1); // SRL
        assert_eq!(alu(0x20, 5, 0x8000_0000, 31), u32::MAX); // SRA
        assert_eq!(alu(0x00, 6, 0b1100, 0b1010), 0b1110); // OR
        assert_eq!(alu(0x00, 7, 0b1100, 0b1010), 0b1000); // AND
    }

    #[test]
    fn shifts_by_immediate() {
        let [a0, a1] = li(1, 0x8000_0001);
        let cpu = run(&[
            a0,
            a1,
            i(4, 1, 1, 2, 0x13),
            i(4, 1, 5, 3, 0x13),
            i(0x400 | 4, 1, 5, 4, 0x13),
        ]);
        assert_eq!(cpu.reg(2), 0x10);
        assert_eq!(cpu.reg(3), 0x0800_0000);
        assert_eq!(cpu.reg(4), 0xf800_0000);
    }

    #[test]
    fn multiplication() {
        let minus_two = (-2_i32).cast_unsigned();
        assert_eq!(alu(1, 0, 0x1_0001, 0x1_0001), 0x2_0001); // MUL keeps the low half
        assert_eq!(alu(1, 1, minus_two, 3), u32::MAX); // MULH: -6 >> 32
        assert_eq!(alu(1, 1, 0x8000_0000, 0x8000_0000), 0x4000_0000); // MULH: (-2^31)^2
        assert_eq!(alu(1, 2, minus_two, u32::MAX), 0xffff_fffe); // MULHSU: -2 * (2^32 - 1)
        assert_eq!(alu(1, 3, u32::MAX, u32::MAX), 0xffff_fffe); // MULHU
    }

    #[test]
    fn division_never_traps() {
        let min = 0x8000_0000;
        let minus_one = u32::MAX;
        assert_eq!(
            alu(1, 4, (-7_i32).cast_unsigned(), 2),
            (-3_i32).cast_unsigned()
        ); // DIV rounds to zero
        assert_eq!(alu(1, 6, (-7_i32).cast_unsigned(), 2), minus_one); // REM takes the dividend's sign
        assert_eq!(alu(1, 4, 7, 0), u32::MAX); // DIV by zero
        assert_eq!(alu(1, 5, 7, 0), u32::MAX); // DIVU by zero
        assert_eq!(alu(1, 6, 7, 0), 7); // REM by zero
        assert_eq!(alu(1, 7, 7, 0), 7); // REMU by zero
        assert_eq!(alu(1, 4, min, minus_one), min); // DIV overflow
        assert_eq!(alu(1, 6, min, minus_one), 0); // REM overflow
        assert_eq!(alu(1, 5, minus_one, 2), 0x7fff_ffff); // DIVU
        assert_eq!(alu(1, 7, minus_one, 16), 15); // REMU
    }

    #[test]
    fn upper_immediates_and_jumps() {
        let cpu = run(&[
            u(0xabcd_e000, 1, 0x37),
            u(0x1000, 2, 0x17),
            jal(8, 3),
            addi(4, 0, 1),
            addi(5, 0, 1),
        ]);
        assert_eq!(cpu.reg(1), 0xabcd_e000);
        assert_eq!(cpu.reg(2), RAM_BASE + 4 + 0x1000); // AUIPC is relative to its own address
        assert_eq!(cpu.reg(3), RAM_BASE + 12); // the link is the instruction after JAL
        assert_eq!(cpu.reg(4), 0); // skipped
        assert_eq!(cpu.reg(5), 1);
    }

    #[test]
    fn jumps_backwards_and_through_a_register() {
        // 0: jal x0, +12   4: addi x1, x1, 1   8: jal x0, +12   12: jal x0, -8   20: jalr x2, x3
        let [t0, t1] = li(3, RAM_BASE + 33);
        let (mut cpu, mut ram, mut bus) = machine(&[
            jal(12, 0),
            addi(1, 1, 1),
            jal(12, 0),
            jal(-8, 0),
            0,
            t0,
            t1,
            i(0, 3, 0, 2, 0x67),
        ]);
        cpu.run(&mut ram, &mut bus, 0, 6);
        assert_eq!(cpu.reg(1), 1);
        assert_eq!(cpu.pc(), RAM_BASE + 28);
        cpu.run(&mut ram, &mut bus, 0, 1);
        assert_eq!(cpu.pc(), RAM_BASE + 32, "JALR clears bit 0 of the target");
        assert_eq!(cpu.reg(2), RAM_BASE + 32);
    }

    #[test]
    fn branches() {
        // Each branch that is taken skips an `addi x10, x10, 1`.
        let taken = |funct3: u32, a: i32, b_value: i32| {
            let cpu = run(&[
                addi(1, 0, a),
                addi(2, 0, b_value),
                b(8, 2, 1, funct3),
                addi(10, 10, 1),
                addi(0, 0, 0),
            ]);
            cpu.reg(10) == 0
        };
        assert!(taken(0, 5, 5) && !taken(0, 5, 6)); // BEQ
        assert!(taken(1, 5, 6) && !taken(1, 5, 5)); // BNE
        assert!(taken(4, -1, 1) && !taken(4, 1, -1)); // BLT is signed
        assert!(taken(5, 1, -1) && taken(5, 3, 3) && !taken(5, -1, 1)); // BGE
        assert!(taken(6, 1, -1) && !taken(6, -1, 1)); // BLTU is unsigned
        assert!(taken(7, -1, 1) && taken(7, 3, 3) && !taken(7, 1, -1)); // BGEU
    }

    #[test]
    fn a_backward_branch_loops() {
        // x1 counts down from 5; x2 counts the iterations.
        let cpu = {
            let (mut cpu, mut ram, mut bus) =
                machine(&[addi(1, 0, 5), addi(2, 2, 1), addi(1, 1, -1), b(-8, 0, 1, 1)]);
            cpu.run(&mut ram, &mut bus, 0, 1 + 3 * 5);
            cpu
        };
        assert_eq!(cpu.reg(1), 0);
        assert_eq!(cpu.reg(2), 5);
        assert_eq!(cpu.pc(), RAM_BASE + 16);
    }

    #[test]
    fn loads_and_stores_of_every_width() {
        let [a0, a1] = li(1, RAM_BASE + 0x100);
        let [v0, v1] = li(2, 0x8182_8384);
        let cpu = run(&[
            a0,
            a1,
            v0,
            v1,
            s(0, 2, 1, 2),       // SW
            i(0, 1, 2, 3, 0x03), // LW
            i(0, 1, 0, 4, 0x03), // LB
            i(0, 1, 4, 5, 0x03), // LBU
            i(2, 1, 1, 6, 0x03), // LH
            i(2, 1, 5, 7, 0x03), // LHU
            s(5, 2, 1, 0),       // SB at an odd address
            s(9, 2, 1, 1),       // SH at an odd address
            i(4, 1, 2, 8, 0x03),
            i(8, 1, 2, 9, 0x03),
        ]);
        assert_eq!(cpu.reg(3), 0x8182_8384);
        assert_eq!(cpu.reg(4), 0xffff_ff84);
        assert_eq!(cpu.reg(5), 0x84);
        assert_eq!(cpu.reg(6), 0xffff_8182);
        assert_eq!(cpu.reg(7), 0x8182);
        assert_eq!(cpu.reg(8), 0x0000_8400);
        assert_eq!(cpu.reg(9), 0x0083_8400);
    }

    #[test]
    fn negative_store_offsets() {
        let [a0, a1] = li(1, RAM_BASE + 0x100);
        let cpu = run(&[
            a0,
            a1,
            addi(2, 0, 0x55),
            s(-4, 2, 1, 2),
            i(-4, 1, 2, 3, 0x03),
        ]);
        assert_eq!(cpu.reg(3), 0x55);
    }

    /// Runs `program` with a trap handler address in `mtvec` and returns the processor right
    /// after the trap was taken.
    fn trapped(program: &[u32]) -> Cpu {
        trapped_after(program, 0)
    }

    /// Like [`trapped`], for a trap that comes `extra` steps after the last instruction of
    /// `program` (a jump to a bad address traps at the fetch that follows it).
    fn trapped_after(program: &[u32], extra: u32) -> Cpu {
        let handler = RAM_BASE + 0x800;
        let [h0, h1] = li(31, handler);
        let mut full = vec![h0, h1, csrrw(0, 0x305, 31)];
        full.extend_from_slice(program);
        let (mut cpu, mut ram, mut bus) = machine(&full);
        let budget = u32::try_from(full.len()).unwrap() + extra;
        assert_eq!(cpu.run(&mut ram, &mut bus, 0, budget), Exit::Budget);
        assert_eq!(cpu.pc(), handler, "no trap was taken");
        cpu
    }

    #[test]
    fn the_last_bytes_of_ram_are_usable_and_the_next_one_is_not() {
        let end = RAM_BASE + u32::try_from(RAM_BYTES).unwrap();
        let [a0, a1] = li(1, end);
        let cpu = run(&[
            a0,
            a1,
            addi(2, 0, 0x7f),
            s(-1, 2, 1, 0),
            i(-1, 1, 0, 3, 0x03),
            i(-4, 1, 2, 4, 0x03),
        ]);
        assert_eq!(cpu.reg(3), 0x7f);
        assert_eq!(cpu.reg(4), 0x7f00_0000);

        let cpu = trapped(&[a0, a1, i(-3, 1, 2, 3, 0x03)]); // a word that hangs over the end
        assert_eq!(cpu.mcause, cause::LOAD_ACCESS);
        assert_eq!(cpu.mtval, end - 3);
        assert_eq!(cpu.mepc, RAM_BASE + 20);

        let cpu = trapped(&[a0, a1, s(0, 2, 1, 0)]);
        assert_eq!(cpu.mcause, cause::STORE_ACCESS);
        assert_eq!(cpu.mtval, end);
    }

    #[test]
    fn addresses_below_ram_fault_unless_they_are_devices() {
        let cpu = trapped(&[i(0x100, 0, 2, 3, 0x03)]);
        assert_eq!((cpu.mcause, cpu.mtval), (cause::LOAD_ACCESS, 0x100));

        let [a0, a1] = li(1, 0x1000_0042);
        let (mut cpu, mut ram, mut bus) = machine(&[
            a0,
            a1,
            i(0, 1, 4, 3, 0x03),
            addi(2, 0, 0x1ab),
            s(1, 2, 1, 0),
        ]);
        cpu.run(&mut ram, &mut bus, 0, 5);
        assert_eq!(cpu.reg(3), 0x42, "the bus answered the load");
        assert_eq!(
            bus.last_store,
            Some((0x1000_0043, 0xab)),
            "a byte store sends one byte"
        );
    }

    #[test]
    fn a_device_can_power_the_machine_off() {
        let [a0, a1] = li(1, 0x1110_0000);
        let (mut cpu, mut ram, mut bus) = machine(&[a0, a1, s(0, 0, 1, 2), addi(5, 0, 1)]);
        assert_eq!(cpu.run(&mut ram, &mut bus, 0, 100), Exit::PowerOff);
        assert_eq!(cpu.pc(), RAM_BASE + 12, "the store is not repeated");
        assert_eq!(cpu.reg(5), 0);
    }

    #[test]
    fn csr_instructions() {
        let cpu = run(&[
            addi(1, 0, 0x55),
            csrrw(2, 0x340, 1), // mscratch = 0x55, x2 = 0
            csrrs(3, 0x340, 0), // read only
            (0x340 << 20) | (0xa << 15) | (6 << 12) | (4 << 7) | 0x73, // CSRRSI: set 0b01010
            (0x340 << 20) | (0x1 << 15) | (7 << 12) | (5 << 7) | 0x73, // CSRRCI: clear bit 0
            csrrs(6, 0x340, 0),
            csrrs(7, 0x301, 0), // misa
            csrrs(8, 0x7c0, 0), // an unknown CSR reads as zero
        ]);
        assert_eq!(cpu.reg(2), 0);
        assert_eq!(cpu.reg(3), 0x55);
        assert_eq!(cpu.reg(4), 0x55);
        assert_eq!(cpu.reg(5), 0x5f);
        assert_eq!(cpu.reg(6), 0x5e);
        assert_eq!(cpu.reg(7), 0x4000_1101);
        assert_eq!(cpu.reg(8), 0);
    }

    #[test]
    fn mstatus_keeps_only_the_bits_that_exist() {
        let cpu = run(&[
            addi(1, 0, -1),
            csrrw(0, 0x300, 1),
            csrrs(2, 0x300, 0),
            csrrw(0, 0x341, 1),
            csrrs(3, 0x341, 0),
        ]);
        assert_eq!(cpu.reg(2), MSTATUS_MIE | MSTATUS_MPIE | MSTATUS_MPP);
        assert_eq!(cpu.reg(3), 0xffff_fffc, "mepc is always aligned");
    }

    #[test]
    fn ecall_and_illegal_instructions_trap_into_the_handler() {
        let cpu = trapped(&[ECALL]);
        assert_eq!(cpu.mcause, cause::ECALL_FROM_MACHINE);
        assert_eq!(cpu.mepc, RAM_BASE + 12);

        let cpu = trapped(&[EBREAK]);
        assert_eq!(cpu.mcause, cause::BREAKPOINT);

        let cpu = trapped(&[0xffff_ffff]);
        assert_eq!(
            (cpu.mcause, cpu.mtval),
            (cause::ILLEGAL_INSTRUCTION, 0xffff_ffff)
        );

        let cpu = trapped(&[0]);
        assert_eq!(
            cpu.mcause,
            cause::ILLEGAL_INSTRUCTION,
            "all zeros is illegal by design"
        );
    }

    #[test]
    fn a_jump_out_of_ram_faults_at_the_fetch() {
        let cpu = trapped_after(&[i(0x100, 0, 0, 1, 0x67)], 1);
        assert_eq!(
            (cpu.mcause, cpu.mtval, cpu.mepc),
            (cause::FETCH_ACCESS, 0x100, 0x100)
        );
        let cpu = trapped_after(&[i(0x102, 0, 0, 1, 0x67)], 1);
        assert_eq!((cpu.mcause, cpu.mtval), (cause::FETCH_MISALIGNED, 0x102));
    }

    #[test]
    fn a_trap_saves_the_interrupt_state_and_mret_restores_it() {
        // Handler at +0x800: add 4 to mepc and return.
        let handler = RAM_BASE + 0x800;
        let [h0, h1] = li(31, handler);
        let (mut cpu, mut ram, mut bus) = machine(&[
            h0,
            h1,
            csrrw(0, 0x305, 31),
            addi(1, 0, 8),
            csrrs(0, 0x300, 1), // interrupts on
            ECALL,
            addi(10, 0, 1),
        ]);
        let handler_code = [
            csrrs(5, 0x300, 0),
            csrrs(6, 0x341, 0),
            addi(6, 6, 4),
            csrrw(0, 0x341, 6),
            MRET,
        ];
        for (index, word) in handler_code.iter().enumerate() {
            ram[0x800 + index * 4..0x800 + index * 4 + 4].copy_from_slice(&word.to_le_bytes());
        }
        cpu.run(&mut ram, &mut bus, 0, 6 + 5 + 1);
        assert_eq!(
            cpu.reg(5),
            MSTATUS_MPIE | MSTATUS_MPP,
            "in the handler: MIE off, MPIE = old MIE, MPP = machine"
        );
        assert_eq!(cpu.reg(10), 1, "execution went on after the ECALL");
        assert_eq!(
            cpu.mstatus,
            MSTATUS_MIE | MSTATUS_MPIE,
            "after MRET: MIE back on, MPP = user"
        );
        assert_eq!(cpu.privilege, MACHINE);
    }

    #[test]
    fn user_mode_cannot_touch_machine_state() {
        // Drop to user mode at +0x100, where a CSR read, an MRET and a WFI must all trap.
        let handler = RAM_BASE + 0x800;
        let [h0, h1] = li(31, handler);
        let [u0, u1] = li(30, RAM_BASE + 0x100);
        for (forbidden, name) in [
            (csrrs(1, 0x300, 0), "csr"),
            (MRET, "mret"),
            (WFI, "wfi"),
            (ECALL, "ecall"),
        ] {
            let (mut cpu, mut ram, mut bus) = machine(&[
                h0,
                h1,
                csrrw(0, 0x305, 31),
                u0,
                u1,
                csrrw(0, 0x341, 30),
                MRET,
            ]);
            ram[0x100..0x104].copy_from_slice(&forbidden.to_le_bytes());
            cpu.run(&mut ram, &mut bus, 0, 7);
            assert_eq!(
                (cpu.pc(), cpu.privilege),
                (RAM_BASE + 0x100, USER),
                "{name}"
            );
            cpu.run(&mut ram, &mut bus, 0, 1);
            assert_eq!((cpu.pc(), cpu.privilege), (handler, MACHINE), "{name}");
            let expected = if forbidden == ECALL {
                cause::ECALL_FROM_USER
            } else {
                cause::ILLEGAL_INSTRUCTION
            };
            assert_eq!(cpu.mcause, expected, "{name}");
            assert_eq!(
                cpu.mstatus & MSTATUS_MPP,
                0,
                "{name}: the trap came from user mode"
            );
        }
    }

    /// A machine that has set `mtvec`, `mtimecmp` to 1000 and enabled the timer interrupt in
    /// `mie` (and globally if `global`), followed by `tail`.
    fn with_timer(global: bool, tail: &[u32]) -> (Cpu, Vec<u8>, TestBus) {
        let [h0, h1] = li(31, RAM_BASE + 0x800);
        let [c0, c1] = li(1, CLINT_MTIMECMP);
        let mut program = vec![
            h0,
            h1,
            csrrw(0, 0x305, 31),
            c0,
            c1,
            addi(2, 0, 1000),
            s(0, 2, 1, 2),
            s(4, 0, 1, 2),
            addi(3, 0, 0x80),
            csrrs(0, 0x304, 3),
            addi(3, 0, if global { 8 } else { 0 }),
            csrrs(0, 0x300, 3),
        ];
        program.extend_from_slice(tail);
        let (mut cpu, mut ram, mut bus) = machine(&program);
        assert_eq!(cpu.run(&mut ram, &mut bus, 0, 12), Exit::Budget);
        (cpu, ram, bus)
    }

    #[test]
    fn the_timer_interrupts_when_its_time_has_come() {
        let (mut cpu, mut ram, mut bus) =
            with_timer(true, &[addi(10, 10, 1), addi(10, 10, 1), addi(10, 10, 1)]);
        assert_eq!(cpu.micros_until_timer(), Some(1000));
        cpu.run(&mut ram, &mut bus, 999, 1);
        assert_eq!(cpu.reg(10), 1, "not yet");
        assert_eq!(cpu.micros_until_timer(), Some(1));

        let before = cpu.pc();
        cpu.run(&mut ram, &mut bus, 1, 0);
        assert_eq!(cpu.pc(), RAM_BASE + 0x800);
        assert_eq!(cpu.mcause, cause::MACHINE_TIMER);
        assert_eq!(
            cpu.mepc, before,
            "the interrupted instruction runs after the handler"
        );
        assert_eq!(
            cpu.mstatus & MSTATUS_MIE,
            0,
            "interrupts are off in the handler"
        );
        assert_eq!(cpu.csr_read(0x344), MTI);
    }

    #[test]
    fn a_pending_interrupt_is_taken_the_moment_it_is_enabled() {
        // The timer is due but interrupts are off; then the guest turns them on in mid-batch.
        // A kernel's idle loop does exactly this, with interrupts on for a few instructions only.
        let enable = csrrs(0, 0x300, 3);
        let (mut cpu, mut ram, mut bus) = with_timer(
            false,
            &[addi(10, 10, 1), addi(3, 0, 8), enable, addi(10, 10, 1)],
        );
        assert_eq!(cpu.run(&mut ram, &mut bus, 5000, 3), Exit::Budget);
        assert_eq!(
            cpu.pc(),
            RAM_BASE + 0x800,
            "trapped right after the CSR write"
        );
        assert_eq!(
            cpu.mepc,
            RAM_BASE + 15 * 4,
            "and returns to the instruction after it"
        );
        assert_eq!(cpu.reg(10), 1);
    }

    #[test]
    fn wfi_does_not_stop_when_the_interrupt_is_already_pending() {
        let (mut cpu, mut ram, mut bus) = with_timer(false, &[WFI, addi(10, 0, 1)]);
        assert_eq!(cpu.run(&mut ram, &mut bus, 5000, 2), Exit::Budget);
        assert_eq!(cpu.reg(10), 1);
    }

    #[test]
    fn setting_the_timer_into_the_past_interrupts_at_once() {
        // mtimecmp = 1000 and interrupts on, at time 0. Then the guest moves mtimecmp to 1.
        let (mut cpu, mut ram, mut bus) =
            with_timer(true, &[addi(2, 0, 1), s(0, 2, 1, 2), addi(10, 10, 1)]);
        assert_eq!(cpu.run(&mut ram, &mut bus, 500, 2), Exit::Budget);
        assert_eq!(cpu.pc(), RAM_BASE + 0x800);
        assert_eq!(cpu.mepc, RAM_BASE + 14 * 4);
    }

    #[test]
    fn mret_into_enabled_interrupts_takes_the_pending_one() {
        // Interrupts off, timer due, MPIE set: MRET turns interrupts on and the timer fires
        // before the instruction MRET returned to.
        let [t0, t1] = li(4, RAM_BASE + 0x400);
        let (mut cpu, mut ram, mut bus) = with_timer(
            false,
            &[
                t0,
                t1,
                csrrw(0, 0x341, 4),
                addi(3, 0, 0x80),
                csrrs(0, 0x300, 3),
                MRET,
            ],
        );
        assert_eq!(cpu.run(&mut ram, &mut bus, 5000, 6), Exit::Budget);
        assert_eq!(cpu.pc(), RAM_BASE + 0x800);
        assert_eq!(cpu.mepc, RAM_BASE + 0x400);
        assert_eq!(cpu.mcause, cause::MACHINE_TIMER);
    }

    #[test]
    fn the_timer_waits_while_interrupts_are_off() {
        let (mut cpu, mut ram, mut bus) = with_timer(false, &[addi(10, 10, 1), addi(10, 10, 1)]);
        cpu.run(&mut ram, &mut bus, 5000, 2);
        assert_eq!(cpu.reg(10), 2);
        assert_ne!(cpu.pc(), RAM_BASE + 0x800);
    }

    #[test]
    fn the_guest_reads_the_time_from_the_clint() {
        let [c0, c1] = li(1, CLINT_MTIME);
        let (mut cpu, mut ram, mut bus) = machine(&[
            c0,
            c1,
            i(0, 1, 2, 2, 0x03),
            i(4, 1, 2, 3, 0x03),
            csrrs(4, 0xc01, 0),
        ]);
        cpu.run(&mut ram, &mut bus, 0x1234_5678, 2);
        cpu.run(&mut ram, &mut bus, u32::MAX, 3);
        assert_eq!(cpu.reg(2), 0x1234_5677);
        assert_eq!(cpu.reg(3), 1);
        assert_eq!(cpu.reg(4), 0x1234_5677);
    }

    #[test]
    fn wfi_sleeps_until_the_timer_and_wakes_even_with_interrupts_off() {
        let (mut cpu, mut ram, mut bus) = with_timer(false, &[WFI, addi(10, 0, 1)]);
        assert_eq!(cpu.run(&mut ram, &mut bus, 0, 100), Exit::Idle);
        assert_eq!(cpu.run(&mut ram, &mut bus, 10, 100), Exit::Idle);
        assert_eq!(cpu.reg(10), 0);
        assert_eq!(cpu.micros_until_timer(), Some(990));
        assert_eq!(cpu.run(&mut ram, &mut bus, 990, 1), Exit::Budget);
        assert_eq!(cpu.reg(10), 1, "woke up and went on, without a trap");
    }

    #[test]
    fn wfi_without_a_timer_sleeps_for_good() {
        let (mut cpu, mut ram, mut bus) = machine(&[WFI]);
        assert_eq!(cpu.run(&mut ram, &mut bus, 0, 100), Exit::Idle);
        assert_eq!(cpu.micros_until_timer(), None);
        assert_eq!(cpu.run(&mut ram, &mut bus, u32::MAX, 100), Exit::Idle);
    }

    /// Runs one atomic operation on a word that holds `old` with the source `source`, and returns
    /// what was read into x3 and what is in the word afterwards.
    fn amo(operation: u32, old: u32, source: u32) -> (u32, u32) {
        let [a0, a1] = li(1, RAM_BASE + 0x200);
        let [s0, s1] = li(2, source);
        let (mut cpu, mut ram, mut bus) =
            machine(&[a0, a1, s0, s1, r(operation << 2, 2, 1, 2, 3, 0x2f)]);
        ram[0x200..0x204].copy_from_slice(&old.to_le_bytes());
        cpu.run(&mut ram, &mut bus, 0, 5);
        (
            cpu.reg(3),
            u32::from_le_bytes(ram[0x200..0x204].try_into().unwrap()),
        )
    }

    #[test]
    fn atomic_memory_operations() {
        let minus_one = u32::MAX;
        assert_eq!(amo(0b00001, 5, 9), (5, 9)); // AMOSWAP
        assert_eq!(amo(0b00000, 5, 9), (5, 14)); // AMOADD
        assert_eq!(amo(0b00100, 0b1100, 0b1010), (0b1100, 0b0110)); // AMOXOR
        assert_eq!(amo(0b01100, 0b1100, 0b1010), (0b1100, 0b1000)); // AMOAND
        assert_eq!(amo(0b01000, 0b1100, 0b1010), (0b1100, 0b1110)); // AMOOR
        assert_eq!(amo(0b10000, minus_one, 1), (minus_one, minus_one)); // AMOMIN is signed
        assert_eq!(amo(0b10100, minus_one, 1), (minus_one, 1)); // AMOMAX
        assert_eq!(amo(0b11000, minus_one, 1), (minus_one, 1)); // AMOMINU is unsigned
        assert_eq!(amo(0b11100, minus_one, 1), (minus_one, minus_one)); // AMOMAXU
    }

    #[test]
    fn store_conditional_needs_a_reservation_on_the_same_address() {
        let [a0, a1] = li(1, RAM_BASE + 0x200);
        let lr = |rd: u32, rs1: u32| r(0b00010 << 2, 0, rs1, 2, rd, 0x2f);
        let sc = |rd: u32, rs1: u32, rs2: u32| r(0b00011 << 2, rs2, rs1, 2, rd, 0x2f);
        let cpu = run(&[
            a0,
            a1,
            addi(2, 1, 4),
            addi(9, 0, 77),
            sc(3, 1, 9), // no reservation: fails
            lr(4, 1),
            sc(5, 2, 9), // another address: fails, and the reservation is gone
            sc(6, 1, 9), // fails
            lr(4, 1),
            sc(7, 1, 9), // succeeds
            sc(8, 1, 9), // only once
            i(0, 1, 2, 10, 0x03),
        ]);
        assert_eq!(
            (cpu.reg(3), cpu.reg(5), cpu.reg(6), cpu.reg(7), cpu.reg(8)),
            (1, 1, 1, 0, 1)
        );
        assert_eq!(cpu.reg(10), 77);
    }

    #[test]
    fn atomics_outside_ram_fault() {
        let cpu = trapped(&[r(0b00010 << 2, 0, 0, 2, 3, 0x2f)]);
        assert_eq!(cpu.mcause, cause::LOAD_ACCESS);
        let cpu = trapped(&[r(0b00001 << 2, 0, 0, 2, 3, 0x2f)]);
        assert_eq!(cpu.mcause, cause::STORE_ACCESS);
    }

    #[test]
    fn the_cycle_counter_counts_instructions() {
        let cpu = run(&[addi(1, 0, 1), addi(1, 0, 1), csrrs(5, 0xc00, 0)]);
        assert_eq!(cpu.reg(5), 3);
        assert_eq!(cpu.cycles(), 3);
    }
}
