//! MMU stage 1 (`klaussCPU/MMU_PLAN.md` §3-§6): user and supervisor modes,
//! precise exceptions, the system registers `MFSR`/`MTSR` reach, and the
//! LR/SC reservation.
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

use super::{CTX_FLAGS_SHIFT, CTX_INT_MASK_SHIFT, CTX_IRQ_DISABLED_SHIFT, CTX_PREV_MODE_SHIFT, CYCLES_PER_INSTRUCTION, Cpu, StopReason};
use crate::layout::is_mmio;

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
    /// in either mode.  `EADDR` = the lowest MMIO byte the access touches.
    AccessLoad = 7,
    /// The same for stores (stack pushes included) and SC.
    AccessStore = 8,
    /// A user-mode fetch from MMIO.  `EADDR` = the first instruction word
    /// in MMIO.
    AccessFetch = 9,
    /// A user-mode PC with `[1:0]` != 0.  `EADDR` = the PC.
    MisalignedFetch = 10,
}

impl Cause {
    /// Whether an attached debugger halts on this cause unless `EXC_PASS`
    /// is set (§4.4 step 1: ILLEGAL, TRAP and MISALIGNED_*).
    // MMU-SPEC-Q: §4.4 step 1 says "MISALIGNED_*" and step 2 lists
    // MISALIGNED_FETCH apart from "MISALIGNED_* → 5".  Read literally, step
    // 1 includes MISALIGNED_FETCH, so an attached debugger halts on it.
    const fn debug_halts(self) -> bool {
        matches!(
            self,
            Self::Illegal | Self::Trap | Self::MisalignedLoad | Self::MisalignedStore | Self::MisalignedFetch
        )
    }
}

/// `CPU_FEATURES` (MMIO `0xF00F_0048`, §6.3): bit 0, stage 1 (exceptions,
/// user mode, SYSCALL, MFSR/MTSR, TP, CYCLE); bit 3, LR/SC.
pub(super) const CPU_FEATURES_VALUE: u64 = 0b1001;

/// The system-register numbers (`MFSR`/`MTSR`, §6.1).
pub(super) mod sr {
    /// `[0]` UM, `[1]` DI; read-only bits (`MPU_EN` and TE are stages 2/3).
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
}

/// Whether user mode may read SR `n` with `MFSR` (§6.1: STATUS, TP, CYCLE).
pub(super) const fn sr_user_readable(n: u32) -> bool {
    matches!(n, sr::STATUS | sr::TP | sr::CYCLE)
}

/// Whether user mode may write SR `n` with `MTSR` (§6.1: TP only).
pub(super) const fn sr_user_writable(n: u32) -> bool {
    n == sr::TP
}

/// The stage 1 state a debugger sees, beyond the registers of
/// [`DebugRegs`](super::DebugRegs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
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
    /// `ECAUSE`.
    pub ecause: u8,
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
}

impl Cpu {
    /// Interrupt or exception entry, steps 1-2 and 4 of §3.3: from user
    /// mode, switch to the kernel stack and supervisor mode; push the frame
    /// `{PM, DI, INT_MASK, flags, frame_pc}`; clear the reservation.  The
    /// caller does the rest (vector, mask, `ECAUSE`).
    pub(super) fn enter(&mut self, frame_pc: u32) {
        let prev_user = self.um;
        if prev_user {
            std::mem::swap(&mut self.sp, &mut self.sp_alt);
            self.um = false;
        }
        let ctx = (u64::from(prev_user) << CTX_PREV_MODE_SHIFT)
            | (u64::from(self.irq_disabled) << CTX_IRQ_DISABLED_SHIFT)
            | (u64::from(self.int_mask) << CTX_INT_MASK_SHIFT)
            | (u64::from(self.flags.to_ctx()) << CTX_FLAGS_SHIFT)
            | u64::from(frame_pc);
        self.push(ctx);
        self.reservation = None;
        self.waiting = false;
    }

    /// Raise `cause` for the instruction at the PC, whose word 0 is `word`
    /// and which would fall through to `next`, in the order of §4.4:
    ///
    /// 1. an attached debugger with `EXC_PASS` clear halts on ILLEGAL, TRAP
    ///    and MISALIGNED_* as before stage 1;
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
        let debug_halt = self.debugger_attached && !self.exc_pass && cause.debug_halts();
        if debug_halt || self.evec == 0 {
            self.stop = Some(match cause {
                Cause::Trap => StopReason::Trap,
                Cause::MisalignedLoad | Cause::MisalignedStore => StopReason::Misaligned(eaddr),
                // Only an attached debugger stops on this one as misaligned
                // (step 1); parked, it is a new cause (step 2).
                Cause::MisalignedFetch if debug_halt => StopReason::Misaligned(eaddr),
                _ => StopReason::InvalidOpcode(word),
            });
            self.parked = Some((cause, eaddr));
            return cause == Cause::Trap;
        }
        let frame_pc = if cause == Cause::Syscall { next } else { self.pc };
        self.enter(frame_pc);
        self.ecause = cause as u8;
        self.eaddr = eaddr;
        self.irq_disabled = true;
        self.pc = self.evec;
        self.exc_entries += 1;
        // The entry is not free on the board either; this keeps modelled
        // time moving through a run of faults.
        self.tick(CYCLES_PER_INSTRUCTION);
        false
    }

    /// The user-mode fetch check of §3.6 for an instruction at `pc`: a PC
    /// with `[1:0]` != 0 is `MISALIGNED_FETCH`, and an instruction with any
    /// word in MMIO is `ACCESS_FETCH` at that word.  `None` when it may run.
    pub(super) fn user_fetch_fault(&self, pc: u32) -> Option<(Cause, u32)> {
        // MMU-SPEC-Q: §3.6 / §4.1 do not order MISALIGNED_FETCH against
        // ACCESS_FETCH for a misaligned PC in MMIO; here the misaligned PC
        // wins (it is known before any word is fetched).
        if pc & 3 != 0 {
            return Some((Cause::MisalignedFetch, pc));
        }
        if is_mmio(pc) {
            return Some((Cause::AccessFetch, pc));
        }
        // The length is in word 0 (LEN 00, illegal, is one word).
        let words = (self.read32(pc) >> 30).max(1);
        (1..words)
            .map(|k| pc.wrapping_add(4 * k))
            .find(|&addr| is_mmio(addr))
            .map(|addr| (Cause::AccessFetch, addr))
    }

    /// The value of system register `n` (zero-extended), or `None` when
    /// there is no such SR (an illegal `MFSR`).
    pub(super) fn sr_read(&self, n: u32) -> Option<u64> {
        Some(match n {
            sr::STATUS => u64::from(self.um) | (u64::from(self.irq_disabled) << 1),
            sr::EVEC => u64::from(self.evec),
            sr::ECAUSE => u64::from(self.ecause),
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
            _ => return None,
        })
    }

    /// Write system register `n` (bits it does not have are ignored).
    /// Returns false when that is illegal: no such SR, or `CYCLE`.
    pub(super) fn sr_write(&mut self, n: u32, value: u64) -> bool {
        match n {
            // UM and DI are read-only here (IRET / DI / EI change them), and
            // the bits stages 2 and 3 add do not exist yet.
            sr::STATUS => {}
            sr::EVEC => self.evec = value as u32 & !3,
            sr::ECAUSE => self.ecause = value as u8,
            sr::EADDR => self.eaddr = value as u32,
            sr::USP => self.sp_alt = value as u32,
            sr::SCRATCH0 => self.scratch[0] = value,
            sr::SCRATCH1 => self.scratch[1] = value,
            sr::TP => self.tp = value,
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
            ecause: self.ecause,
            eaddr: self.eaddr,
            scratch: self.scratch,
            tp: self.tp,
            cycle: self.cycles,
            reservation: self.reservation.map(|dw| dw << 3),
        }
    }

    /// Set the mode bits and the inactive SP from a debugger (the board's
    /// debug registers 20 and 21, writable while halted, §6.2).
    pub fn set_mode_regs(&mut self, sp_alt: u32, um: bool, irq_disabled: bool) {
        self.sp_alt = sp_alt;
        self.um = um;
        self.irq_disabled = irq_disabled;
    }

    /// Tell the CPU a debugger is attached: ILLEGAL, TRAP and MISALIGNED_*
    /// then stop it (a debug halt) instead of entering `EVEC`, unless
    /// `EXC_PASS` is set ([`Cpu::set_exc_pass`]).
    pub fn set_debugger_attached(&mut self, attached: bool) {
        self.debugger_attached = attached;
    }

    /// `EXC_PASS` (the board's `DMCONTROL[5]`): with a debugger attached,
    /// let ILLEGAL, TRAP and MISALIGNED_* go to the program's `EVEC`.
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
