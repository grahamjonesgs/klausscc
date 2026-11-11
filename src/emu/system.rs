//! MMU stage 1 (`klaussCPU/MMU_PLAN.md` §3-§6): user and supervisor modes,
//! precise exceptions, the system registers `MFSR`/`MTSR` reach (those of
//! stages 2 and 3 included), and the LR/SC reservation.
//!
//! At reset (and so on every program load, which is a new [`Cpu`]) the CPU
//! is in supervisor mode with `EVEC` = 0, and then nothing here changes what
//! a program sees: an illegal instruction, `TRAP` or a misaligned access
//! stops the emulator exactly as before stage 1, and so does every new cause
//! (as an illegal instruction), which is what the board does when `EVEC` is 0
//! (§1, §4.4).
//!
//! An exception is precise: the faulting instruction changes nothing (no
//! register, flag, SP or memory), and does not retire (no trace line, no
//! `PERF_INSTR` count).  The class handlers in `exec` check everything that
//! can fault before their first side effect and return the cause; [`Cpu::raise`]
//! then parks, debug-halts or enters the handler.

use super::mmu::{MPU_REGIONS, TLB_HI_BITS, TLB_LO_BITS, XlateFault};
use super::{CTX_FLAGS_SHIFT, CTX_INT_MASK_SHIFT, CTX_IRQ_DISABLED_SHIFT, CTX_PREV_MODE_SHIFT, CYCLES_PER_INSTRUCTION, Cpu, StopReason};

/// An exception cause: its `ECAUSE` value (`MMU_PLAN.md` §4.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Cause {
    /// Undefined encoding, nonzero reserved field or unimplemented SR.
    /// `EADDR` = the instruction word.
    Illegal = 1,
    /// A privileged instruction in user mode (§3.5).  `EADDR` = the
    /// instruction word.
    Privileged = 2,
    /// `TRAP`.  `EADDR` = the instruction word.
    Trap = 3,
    /// `SYSCALL`: the frame PC is the next instruction.  `EADDR` = 0.
    Syscall = 4,
    /// A misaligned 16/32-bit load (arch §8.3), or an LR not naturally
    /// aligned.  `EADDR` = the effective address.
    MisalignedLoad = 5,
    /// The same for stores and SC.
    MisalignedStore = 6,
    /// A user-mode load (stack pops included) from MMIO, or an LR to MMIO
    /// in either mode; an MPU denial (stage 2); a TLB address-map or read
    /// permission fault (stage 3).  `EADDR` = the effective address as
    /// computed (base + offset, before any alignment), or, when only the
    /// second doubleword of a two-access instruction faults, that
    /// doubleword's start.
    AccessLoad = 7,
    /// The same for stores (stack pushes included) and SC.
    AccessStore = 8,
    /// The same for fetches.  `EADDR` = the first instruction word that
    /// faults.
    AccessFetch = 9,
    /// A user-mode PC with `[1:0]` != 0.  `EADDR` = the PC.
    MisalignedFetch = 10,
    /// Stage 3: a useg or kmap load with no matching TLB entry.  `EADDR` =
    /// the VA, as for `ACCESS_LOAD`; `TLB_HI` is loaded from it.
    TlbMissLoad = 11,
    /// The same for stores.
    TlbMissStore = 12,
    /// The same for fetches.
    TlbMissFetch = 13,
}

impl Cause {
    /// Whether an attached debugger halts on this cause unless `EXC_PASS`
    /// is set (§4.4 step 1: ILLEGAL, TRAP, `MISALIGNED_LOAD` and
    /// `MISALIGNED_STORE`; not the new, user-mode-only `MISALIGNED_FETCH`).
    const fn debug_halts(self) -> bool {
        matches!(self, Self::Illegal | Self::Trap | Self::MisalignedLoad | Self::MisalignedStore)
    }

    /// A TLB miss, which vectors to `TLBVEC` when that is nonzero (§11.3).
    const fn tlb_miss(self) -> bool {
        matches!(self, Self::TlbMissLoad | Self::TlbMissStore | Self::TlbMissFetch)
    }
}

/// `CPU_FEATURES` (MMIO `0xF00F_0048`, §6.3) without the MPU: bit 0, stage
/// 1 (exceptions, user mode, SYSCALL, MFSR/MTSR, TP, CYCLE); bit 2, the
/// stage 3 TLB with `[15:8]` = its entry count (added by
/// [`Cpu::cpu_features`]); bit 3, LR/SC.  `0x200D` with the Nexys A7
/// bitstream's 32 entries (`0x100D` with 16).
const CPU_FEATURES_NO_MPU: u64 = 0b1101;
/// The `CPU_FEATURES` bits the stage 2 MPU adds: bit 1, with `[7:4]` = the
/// region count - 1 (`0x20FF` in all with 32 TLB entries).
const CPU_FEATURES_MPU: u64 = (((MPU_REGIONS - 1) as u64) << 4) | 0b10;

/// The `CPU_FEATURES` bit for the native SD controller (`--sd-native`,
/// `SD_NATIVE_PLAN.md` §4.1): bit 21.  The release bitstream has none.
const CPU_FEATURES_SD_NATIVE: u64 = 1 << 21;

/// `STATUS[8]` `MPU_EN` (stage 2).
const STATUS_MPU_EN: u64 = 1 << 8;
/// `STATUS[9]` TE (stage 3).
const STATUS_TE: u64 = 1 << 9;
/// `STATUS[10]` MAL64 (`HW_PERF_PLAN.md` §4).
const STATUS_MAL64: u64 = 1 << 10;
/// `CPU_FEATURES[22]` DCBZ and `[23]` STATUS.MAL64 (`HW_PERF_PLAN.md`).
/// `[24]` (`ETH_RX_SLOTS`) stays clear: the emulator has no Ethernet.
const CPU_FEATURES_PERF_HW: u64 = (1 << 22) | (1 << 23);
/// `ECAUSE[8]` GUARD: an `ACCESS_*` that an MPU GUARD region denied.
const ECAUSE_GUARD: u64 = 1 << 8;

/// The system-register numbers (`MFSR`/`MTSR`, §6.1, §10.1, §11.3).
pub(super) mod sr {
    /// `[0]` UM, `[1]` DI (read-only), `[8]` `MPU_EN`, `[9]` TE.
    pub(in super::super) const STATUS: u32 = 0;
    /// Exception vector, `[1:0]` read 0.
    pub(in super::super) const EVEC: u32 = 1;
    /// The last exception's cause, 8 bits.
    pub(in super::super) const ECAUSE: u32 = 2;
    /// The last exception's address or instruction word.
    pub(in super::super) const EADDR: u32 = 3;
    /// `SP_ALT`: the user SP while in supervisor mode.
    pub(in super::super) const USP: u32 = 4;
    /// Kernel scratch, 64 bits.
    pub(in super::super) const SCRATCH0: u32 = 5;
    /// Kernel scratch, 64 bits.
    pub(in super::super) const SCRATCH1: u32 = 6;
    /// Thread pointer, 64 bits; read and written in either mode.
    pub(in super::super) const TP: u32 = 7;
    /// The cycle counter, read-only; read in either mode.
    pub(in super::super) const CYCLE: u32 = 8;
    /// `MPU_BASE0`; `MPU_BASEn` is `MPU_BASE0 + 2n` and `MPU_LIMITn` the
    /// number after it, n = 0-15.
    pub(in super::super) const MPU_BASE0: u32 = 0x10;
    /// The last MPU SR, `MPU_LIMIT15`.
    pub(in super::super) const MPU_LIMIT15: u32 = 0x2F;
    /// `TLB_HI`: VPN `[31:16]`, G `[8]`, ASID `[7:0]`.
    pub(in super::super) const TLB_HI: u32 = 0x30;
    /// `TLB_LO`: PFN `[26:16]`, X/W/R/V `[3:0]`.
    pub(in super::super) const TLB_LO: u32 = 0x31;
    /// `TLB_INDEX`: P `[31]`, index `[4:0]`.
    pub(in super::super) const TLB_INDEX: u32 = 0x32;
    /// `ASID` `[7:0]`.
    pub(in super::super) const ASID: u32 = 0x33;
    /// `TLBVEC`: the TLB-miss vector, 0 = `EVEC`.
    pub(in super::super) const TLBVEC: u32 = 0x34;
}

/// Whether user mode may read SR `n` with `MFSR` (§6.1: STATUS, TP, CYCLE).
pub(super) const fn sr_user_readable(n: u32) -> bool {
    matches!(n, sr::STATUS | sr::TP | sr::CYCLE)
}

/// Whether user mode may write SR `n` with `MTSR` (§6.1: TP only).
pub(super) const fn sr_user_writable(n: u32) -> bool {
    n == sr::TP
}

/// The system state a debugger sees, beyond the registers of
/// [`DebugRegs`](super::DebugRegs) (the MPU regions and TLB entries are
/// [`Cpu::mpu_regions`] and [`Cpu::tlb_entries`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[expect(clippy::struct_excessive_bools, reason = "the STATUS bits")]
pub struct SystemRegs {
    /// User mode.
    pub um: bool,
    /// The interrupt-disable bit (`DI`).
    pub irq_disabled: bool,
    /// The inactive stack pointer (the user SP in supervisor mode, the
    /// kernel SP in user mode).
    pub sp_alt: u32,
    /// `EVEC`.
    pub evec: u32,
    /// `ECAUSE`: the cause code in `[7:0]` and GUARD in `[8]`.
    pub ecause: u16,
    /// `EADDR`.
    pub eaddr: u32,
    /// `SCRATCH0`, `SCRATCH1`.
    pub scratch: [u64; 2],
    /// `TP`.
    pub tp: u64,
    /// `CYCLE`.
    pub cycle: u64,
    /// The LR/SC reservation: the reserved doubleword's address.
    pub reservation: Option<u32>,
    /// `STATUS.MPU_EN`.
    pub mpu_en: bool,
    /// `STATUS.TE`.
    pub te: bool,
    /// `TLB_HI`.
    pub tlb_hi: u32,
    /// `TLB_LO`.
    pub tlb_lo: u32,
    /// `TLB_INDEX`.
    pub tlb_index: u32,
    /// `ASID`.
    pub asid: u8,
    /// `TLBVEC`.
    pub tlbvec: u32,
}

impl Cpu {
    /// Interrupt or exception entry, steps 1-2 and 4 of §3.3: from user
    /// mode, switch to the kernel stack and supervisor mode; push the frame
    /// `{PM, DI, INT_MASK, flags, frame_pc}`; clear the reservation.  The
    /// caller does the rest (vector, mask, `ECAUSE`).
    ///
    /// The push is not checked by the MPU (§10.2) and not translated
    /// (§11.3, [`Cpu::frame_slot`]).  With `TE` = 1 a kernel SP whose push
    /// is not in kseg is a double fault: the CPU stops with nothing of the
    /// entry done, and this returns false.
    pub(super) fn enter(&mut self, frame_pc: u32) -> bool {
        let prev_user = self.um;
        let kernel_sp = if prev_user { self.sp_alt } else { self.sp };
        let Some(slot) = self.frame_slot(kernel_sp) else {
            self.double_fault(kernel_sp);
            return false;
        };
        if prev_user {
            std::mem::swap(&mut self.sp, &mut self.sp_alt);
            self.um = false;
        }
        let ctx = (u64::from(prev_user) << CTX_PREV_MODE_SHIFT)
            | (u64::from(self.irq_disabled) << CTX_IRQ_DISABLED_SHIFT)
            | (u64::from(self.int_mask) << CTX_INT_MASK_SHIFT)
            | (u64::from(self.flags.to_ctx()) << CTX_FLAGS_SHIFT)
            | u64::from(frame_pc);
        self.write64(slot, ctx);
        self.sp = slot.va();
        self.reservation = None;
        self.waiting = false;
        true
    }

    /// Raise `cause` for the instruction at the PC, whose word 0 is `word`
    /// and which would fall through to `next`, in the order of §4.4:
    ///
    /// 1. an attached debugger with `EXC_PASS` clear halts on ILLEGAL, TRAP
    ///    and `MISALIGNED_LOAD/STORE` as before stage 1;
    /// 2. with `EVEC` = 0 the core parks as before stage 1: ILLEGAL and
    ///    every new cause as an illegal instruction, TRAP as a TRAP, a
    ///    misaligned load or store as a misaligned access;
    /// 3. otherwise the exception is entered (§3.3): the frame PC is the
    ///    instruction (the next one for SYSCALL), `ECAUSE`/`EADDR` are
    ///    written, DI is set and the PC goes to `EVEC`.
    ///
    /// The PC stays on the instruction when it stops.  Returns whether the
    /// instruction counts as retired: only a TRAP that stops does, as
    /// before stage 1.
    pub(super) fn raise(&mut self, cause: Cause, eaddr: u32, word: u32, next: u32) -> bool {
        self.raise_fault(
            XlateFault {
                cause,
                eaddr,
                guard: false,
                tlb: false,
            },
            word,
            next,
        )
    }

    /// [`Cpu::raise`] for an exception that may carry MMU detail: an MPU
    /// GUARD denial sets `ECAUSE[8]`, and a TLB miss or TLB permission
    /// fault loads `TLB_HI` = {VPN of `EADDR`, G = 0, `ASID`} (§11.3).  A
    /// TLB miss enters `TLBVEC` when that is nonzero, else `EVEC`.
    ///
    /// With `EVEC` = 0 every cause parks (§4.4 step 2), a TLB miss too
    /// whatever `TLBVEC` holds, and a park writes neither `ECAUSE`, `EADDR`
    /// nor `TLB_HI`.  A double fault in the entry stops the CPU instead
    /// ([`Cpu::enter`]), with nothing written either.
    // Spec (confirmed in MMU_PLAN 424e236): §4.4 parks on EVEC = 0 for every cause; §11.3 does not say
    // whether a TLB miss with TLBVEC != 0 and EVEC = 0 parks or enters
    // TLBVEC.  This parks (the literal §4.4 reading).
    // Spec (confirmed in MMU_PLAN 424e236): §11.3 says hardware loads TLB_HI on a TLB fault; whether a
    // park (EVEC = 0) or a double fault loads it is not said.  Neither does
    // here, as neither writes ECAUSE/EADDR.
    pub(super) fn raise_fault(&mut self, fault: XlateFault, word: u32, next: u32) -> bool {
        let XlateFault { cause, eaddr, guard, tlb } = fault;
        let debug_halt = self.debugger_attached && !self.exc_pass && cause.debug_halts();
        if debug_halt || self.evec == 0 {
            self.stop = Some(match cause {
                Cause::Trap => StopReason::Trap,
                Cause::MisalignedLoad | Cause::MisalignedStore => StopReason::Misaligned(eaddr),
                // Every new cause parks as an illegal instruction (step 2;
                // with a debugger attached that park is a debug halt).
                _ => StopReason::InvalidOpcode(word),
            });
            self.parked = Some((cause, eaddr));
            return cause == Cause::Trap;
        }
        let frame_pc = if cause == Cause::Syscall { next } else { self.pc };
        if !self.enter(frame_pc) {
            self.parked = Some((cause, eaddr));
            return false;
        }
        self.ecause = cause as u8;
        self.ecause_guard = guard;
        self.eaddr = eaddr;
        if tlb {
            self.tlb_hi = (eaddr & 0xFFFF_0000) | u32::from(self.asid);
        }
        self.irq_disabled = true;
        self.pc = if cause.tlb_miss() && self.tlbvec != 0 { self.tlbvec } else { self.evec };
        self.exc_entries += 1;
        // The entry is not free on the board either; this keeps modelled
        // time moving through a run of faults.
        self.tick(CYCLES_PER_INSTRUCTION);
        false
    }

    /// The value of system register `n` (zero-extended), or `None` when
    /// there is no such SR (an illegal `MFSR`).
    pub(super) fn sr_read(&self, n: u32) -> Option<u64> {
        Some(match n {
            sr::STATUS => {
                u64::from(self.um)
                    | (u64::from(self.irq_disabled) << 1)
                    | if self.mpu_en { STATUS_MPU_EN } else { 0 }
                    | if self.te { STATUS_TE } else { 0 }
                    | if self.mal64 { STATUS_MAL64 } else { 0 }
            }
            sr::EVEC => u64::from(self.evec),
            sr::ECAUSE => u64::from(self.ecause) | if self.ecause_guard { ECAUSE_GUARD } else { 0 },
            sr::EADDR => u64::from(self.eaddr),
            sr::USP => u64::from(self.sp_alt),
            sr::SCRATCH0 => self.scratch[0],
            sr::SCRATCH1 => self.scratch[1],
            sr::TP => self.tp,
            // Modelled cycles since reset: CYCLES_PER_INSTRUCTION per
            // retired instruction and per exception entry, plus each
            // DELAY's wait (the same count as PERF_CYCLES, which a
            // PERF_CTRL clear rebases; CYCLE is never cleared).  The
            // emulator has no cycle model, so this is not the board's count.
            sr::CYCLE => self.cycles,
            // Without the MPU its SRs do not exist (ILLEGAL, as any other
            // SR number with nothing behind it).
            sr::MPU_BASE0..=sr::MPU_LIMIT15 if self.has_mpu => {
                let region = self.mpu[((n - sr::MPU_BASE0) / 2) as usize];
                // MPU_LIMITn [4:0] read 0 (§10.1); it is stored that way.
                u64::from(if n.is_multiple_of(2) { region.base } else { region.limit })
            }
            sr::TLB_HI => u64::from(self.tlb_hi),
            sr::TLB_LO => u64::from(self.tlb_lo),
            sr::TLB_INDEX => u64::from(self.tlb_index),
            sr::ASID => u64::from(self.asid),
            sr::TLBVEC => u64::from(self.tlbvec),
            _ => return None,
        })
    }

    /// Write system register `n` (bits it does not have are ignored).
    /// Returns false when that is illegal: no such SR, or `CYCLE`.
    ///
    /// Writing `STATUS` or `ASID` clears the LR/SC reservation (§11.3: every
    /// TLB, ASID or STATUS change does), whatever the value.
    // Spec (confirmed in MMU_PLAN 424e236): §11.3 lists MTSR ASID and MTSR STATUS among the TLB
    // changes that clear the reservation; it is cleared on every such MTSR,
    // also one that writes the value already there.
    // TLBVEC's [1:0] read 0, as EVEC's do (MMU_PLAN 424e236, §11.3).
    pub(super) fn sr_write(&mut self, n: u32, value: u64) -> bool {
        match n {
            // UM and DI are read-only here (IRET / DI / EI change them).
            // Without the MPU, MPU_EN is not there: it ignores the write.
            sr::STATUS => {
                self.mpu_en = self.has_mpu && value & STATUS_MPU_EN != 0;
                self.te = value & STATUS_TE != 0;
                self.mal64 = value & STATUS_MAL64 != 0;
                self.reservation = None;
            }
            sr::EVEC => self.evec = value as u32 & !3,
            // Spec (confirmed in MMU_PLAN 424e236): §6.1 makes ECAUSE RW with GUARD in [8]; MTSR writes
            // [8:0], so software can set or clear GUARD.
            sr::ECAUSE => {
                self.ecause = value as u8;
                self.ecause_guard = value & ECAUSE_GUARD != 0;
            }
            sr::EADDR => self.eaddr = value as u32,
            sr::USP => self.sp_alt = value as u32,
            sr::SCRATCH0 => self.scratch[0] = value,
            sr::SCRATCH1 => self.scratch[1] = value,
            sr::TP => self.tp = value,
            sr::MPU_BASE0..=sr::MPU_LIMIT15 if self.has_mpu => {
                let region = &mut self.mpu[((n - sr::MPU_BASE0) / 2) as usize];
                if n.is_multiple_of(2) {
                    region.base = value as u32;
                } else {
                    region.limit = value as u32 & !0x1F;
                }
            }
            sr::TLB_HI => self.tlb_hi = value as u32 & TLB_HI_BITS,
            sr::TLB_LO => self.tlb_lo = value as u32 & TLB_LO_BITS,
            sr::TLB_INDEX => self.tlb_index = value as u32 & self.tlb_index_bits(),
            sr::ASID => {
                self.asid = value as u8;
                self.reservation = None;
            }
            sr::TLBVEC => self.tlbvec = value as u32 & !3,
            _ => return false,
        }
        true
    }

    /// The stage 1 state, for a debugger.
    #[must_use]
    pub fn system_regs(&self) -> SystemRegs {
        SystemRegs {
            um: self.um,
            irq_disabled: self.irq_disabled,
            sp_alt: self.sp_alt,
            evec: self.evec,
            ecause: u16::from(self.ecause) | if self.ecause_guard { 0x100 } else { 0 },
            eaddr: self.eaddr,
            scratch: self.scratch,
            tp: self.tp,
            cycle: self.cycles,
            reservation: self.reservation.map(|dw| dw << 3),
            mpu_en: self.mpu_en,
            te: self.te,
            tlb_hi: self.tlb_hi,
            tlb_lo: self.tlb_lo,
            tlb_index: self.tlb_index,
            asid: self.asid,
            tlbvec: self.tlbvec,
        }
    }

    /// Set the mode bits and the inactive SP from a debugger (the board's
    /// debug registers 20 and 21, writable while halted, §6.2).
    pub fn set_mode_regs(&mut self, sp_alt: u32, um: bool, irq_disabled: bool) {
        self.sp_alt = sp_alt;
        self.um = um;
        self.irq_disabled = irq_disabled;
    }

    /// Give the CPU the stage 2 MPU (`--mpu`), or take it away (the
    /// default, as the Nexys A7 bitstream has none).  Without it
    /// `CPU_FEATURES` reads `0x200D` (bit 1 and `[7:4]` clear), the
    /// `MPU_BASEn`/`MPU_LIMITn` SRs (0x10-0x2F) are ILLEGAL and `STATUS[8]`
    /// `MPU_EN` reads 0 and ignores writes, so nothing is ever checked.
    /// Set before running; taking the MPU away also clears `MPU_EN`.
    pub const fn set_has_mpu(&mut self, present: bool) {
        self.has_mpu = present;
        self.mpu_en &= present;
    }

    /// Whether this CPU has the stage 2 MPU ([`Cpu::set_has_mpu`]).
    #[must_use]
    pub const fn has_mpu(&self) -> bool {
        self.has_mpu
    }

    /// `CPU_FEATURES` (MMIO `0xF00F_0048`): `0x200D` (`0x100D` with 16 TLB
    /// entries), or `0x20FF` with the MPU, plus bit 21 with the native SD
    /// controller.
    pub(super) fn cpu_features(&self) -> u64 {
        let mpu = if self.has_mpu { CPU_FEATURES_MPU } else { 0 };
        let sd = if self.has_sd_native() { CPU_FEATURES_SD_NATIVE } else { 0 };
        CPU_FEATURES_NO_MPU | (u64::from(self.tlb_n) << 8) | CPU_FEATURES_PERF_HW | mpu | sd
    }

    /// Tell the CPU a debugger is attached: ILLEGAL, TRAP and `MISALIGNED_LOAD/STORE`
    /// then stop it (a debug halt) instead of entering `EVEC`, unless
    /// `EXC_PASS` is set ([`Cpu::set_exc_pass`]).
    pub fn set_debugger_attached(&mut self, attached: bool) {
        self.debugger_attached = attached;
    }

    /// `EXC_PASS` (the board's `DMCONTROL[5]`): with a debugger attached,
    /// let ILLEGAL, TRAP and `MISALIGNED_LOAD/STORE` go to the program's `EVEC`.
    pub fn set_exc_pass(&mut self, on: bool) {
        self.exc_pass = on;
    }

    /// Whether `EXC_PASS` is set.
    #[must_use]
    pub const fn exc_pass(&self) -> bool {
        self.exc_pass
    }

    /// The cause and address of the exception the CPU last stopped on
    /// (parked or debug-halted) instead of entering a handler; `None` if it
    /// has not.
    #[must_use]
    pub const fn parked_exception(&self) -> Option<(Cause, u32)> {
        self.parked
    }
}
