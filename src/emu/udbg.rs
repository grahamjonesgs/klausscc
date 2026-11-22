//! User-mode debug (`klaussCPU/MMU_PLAN.md` §13, `CPU_ARCHITECTURE.md`
//! §13.5): the user single step and the two kernel-usable data
//! watchpoints, for ptrace under Linux.  An optional build feature
//! (`CORE1_UDBG_EN`), in the release bitstream since klaussCPU ustep
//! 60bcc36 (QSPI 2026-11-23); the emulator has it unless `--no-ustep`
//! ([`Cpu::set_has_udbg`]).  `CPU_FEATURES[28]` (the step) and `[29]` (the
//! watchpoints) come together.
//!
//! **Step.**  An `IRET` whose frame has PM = 1 and SS (bit 45) = 1 arms a
//! step if `EVEC` != 0.  Exactly one instruction then completes, and STEP
//! (`ECAUSE` 14 | SSTEP) is taken before the next one and before any
//! interrupt, with the frame PC = the instruction's architectural successor
//! and `EADDR` = its PC.  Every entry (an exception, an interrupt, STEP or
//! WATCH) pushes the armed state as frame bit 45 and disarms
//! ([`Cpu::enter`]), so a fault or `SYSCALL` of the stepped instruction is
//! taken as usual with SS = 1 in its frame and no STEP, and an interrupt
//! may come before the stepped instruction (its ISR's `IRET` re-arms) but
//! never after it.  A successful hardware TLB walk is no entry: the
//! instruction restarts and is still the stepped one.
//!
//! **Watch.**  `WPn_ADDR` `[31:3]` names a doubleword VA, `WPn_CTRL` its
//! byte mask `[7:0]`, R `[8]`, W `[9]` and U `[10]` (user accesses only).
//! Each completed data access of an instruction is compared, doubleword by
//! doubleword, after the fact: the instruction completes, then WATCH
//! (`ECAUSE` 15 | WP0 << 9 | WP1 << 10 | WRITE << 11 | SSTEP << 12) is taken
//! before the next one, with the frame PC = the successor and `EADDR` = the
//! raw EA (the second doubleword's start when only that one of a spanning
//! `MEMGET32` or `LEAVERET` hit).  A supervisor access hits only with U = 0
//! and DI = 0; nothing hits while `EVEC` = 0 or a debugger is attached.
//!
//! The emulator commits each instruction at once, so the STEP or WATCH
//! boundary is a pending entry ([`Cpu::udbg_pending`]) taken at the start
//! of the next step, before a `WAIT`'s sleep and before any interrupt.  A
//! debugger halt between the instruction and the entry (a breakpoint on the
//! successor, a debugger single step) therefore takes it at the first step
//! after resume, as the RTL does (§13.2, "the JTAG debug module wins").

use super::system::Cause;
use super::{CTX_STEP_SHIFT, CYCLES_PER_INSTRUCTION, Cpu};

/// `CPU_FEATURES[28]` the user single step and `[29]` the watchpoints.
pub(super) const CPU_FEATURES_UDBG: u64 = (1 << 28) | (1 << 29);

/// `ECAUSE[12:9]`: WP0 `[9]`, WP1 `[10]`, WRITE `[11]`, SSTEP `[12]`, kept
/// here as bits 0-3.
pub(super) const ECAUSE_UDBG_SHIFT: u32 = 9;
/// `ECAUSE` bit 11 WRITE, as a bit of [`Cpu::ecause_udbg`].
const UDBG_WRITE: u8 = 1 << 2;
/// `ECAUSE` bit 12 SSTEP, as a bit of [`Cpu::ecause_udbg`].
const UDBG_SSTEP: u8 = 1 << 3;

/// `WPn_ADDR`: the doubleword VA `[31:3]`.
const WP_ADDR_BITS: u32 = !7;
/// `WPn_CTRL`: BE `[7:0]`, R `[8]`, W `[9]`, U `[10]`.
const WP_CTRL_BITS: u32 = 0x7FF;
/// `WPn_CTRL[8]` R: loads hit.
const WP_R: u32 = 1 << 8;
/// `WPn_CTRL[9]` W: stores hit.
const WP_W: u32 = 1 << 9;
/// `WPn_CTRL[10]` U: user-mode accesses only.
const WP_U: u32 = 1 << 10;

/// The watchpoint SR numbers: `WP0_ADDR`, `WP0_CTRL`, `WP1_ADDR`,
/// `WP1_CTRL`.  `[19:16]` = 4, so no other SR decoder sees them.
pub(super) const SR_WP0_ADDR: u32 = 0x40;
/// The last watchpoint SR, `WP1_CTRL`.
pub(super) const SR_WP1_CTRL: u32 = 0x43;

/// One user watchpoint's two SRs, as written (the bits they have).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UserWatch {
    /// `WPn_ADDR`: `[31:3]`, `[2:0]` 0.
    pub addr: u32,
    /// `WPn_CTRL`: `[10:0]`.
    pub ctrl: u32,
}

impl UserWatch {
    /// Enabled: R or W set and the byte mask nonzero.
    #[must_use]
    pub const fn enabled(self) -> bool {
        self.ctrl & (WP_R | WP_W) != 0 && self.ctrl & 0xFF != 0
    }

    /// Whether an access of `be` (byte enables) in the doubleword at `dw`
    /// (`[2:0]` ignored), a store if `store`, hits, given the mode
    /// qualifier: `user` (a user-mode access) or else `irq_enabled`.
    const fn hits(self, dw: u32, be: u8, store: bool, user: bool, irq_enabled: bool) -> bool {
        self.enabled()
            && dw & WP_ADDR_BITS == self.addr
            && (be as u32) & self.ctrl & 0xFF != 0
            && self.ctrl & if store { WP_W } else { WP_R } != 0
            && (user || (self.ctrl & WP_U == 0 && irq_enabled))
    }
}

/// The watch hits of the current instruction (a pending WATCH once it has
/// completed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct UwHit {
    /// Which watchpoints matched: bit 0 WP0, bit 1 WP1.
    wps: u8,
    /// The hitting access was a store.
    store: bool,
    /// `EADDR`: the access's raw EA, or the second doubleword's start when
    /// only that one hit.
    eaddr: u32,
}

impl Cpu {
    /// Give the CPU the user single step and watchpoints (`MMU_PLAN.md`
    /// §13; the default, as the release bitstream has them), or take them
    /// away (`--no-ustep`, the bitstreams before them).  Without them `CPU_FEATURES[29:28]` read 0, SRs
    /// 0x40-0x43 are ILLEGAL, `ECAUSE` is 9 bits and frame bit 45 is
    /// ignored.  Taking them away clears the watchpoints and the step.
    pub fn set_has_udbg(&mut self, present: bool) {
        self.has_udbg = present;
        if !present {
            self.wp = [UserWatch::default(); 2];
            self.uw_any = false;
            self.us_armed = false;
            self.us_done = None;
            self.uw_hit = None;
            self.ecause_udbg = 0;
        }
    }

    /// Whether this CPU has the user single step and watchpoints
    /// ([`Cpu::set_has_udbg`]).
    #[must_use]
    pub const fn has_udbg(&self) -> bool {
        self.has_udbg
    }

    /// The two user watchpoints (`WPn_ADDR`, `WPn_CTRL`).
    #[must_use]
    pub const fn user_watches(&self) -> [UserWatch; 2] {
        self.wp
    }

    /// Whether a user single step is armed (or its STEP is pending).
    #[must_use]
    pub const fn user_step_armed(&self) -> bool {
        self.us_armed
    }

    /// SRs 0x40-0x43, or `None` for any other SR or without the feature.
    pub(super) const fn wp_sr_read(&self, n: u32) -> Option<u64> {
        if !self.has_udbg || n < SR_WP0_ADDR || n > SR_WP1_CTRL {
            return None;
        }
        let wp = self.wp[((n - SR_WP0_ADDR) / 2) as usize];
        Some(if n.is_multiple_of(2) { wp.addr as u64 } else { wp.ctrl as u64 })
    }

    /// Write one of SRs 0x40-0x43 (the bits it has); false for any other
    /// SR or without the feature.
    pub(super) fn wp_sr_write(&mut self, n: u32, value: u64) -> bool {
        if !self.has_udbg || !(SR_WP0_ADDR..=SR_WP1_CTRL).contains(&n) {
            return false;
        }
        let wp = &mut self.wp[((n - SR_WP0_ADDR) / 2) as usize];
        if n.is_multiple_of(2) {
            wp.addr = value as u32 & WP_ADDR_BITS;
        } else {
            wp.ctrl = value as u32 & WP_CTRL_BITS;
        }
        self.uw_any = self.wp.iter().any(|w| w.enabled());
        true
    }

    /// The doubleword at `dw` (`[2:0]` ignored) was accessed with the byte
    /// enables `be` by the current instruction, as a store if `store`;
    /// `eaddr` is the `EADDR` a WATCH reports if this is the instruction's
    /// first hit.  Call only for an access that happened (after every check
    /// that can fault), and only for the instruction's own data accesses:
    /// never an `IRET`'s frame read, an entry's push or a walker read.
    pub(super) fn uw_dw(&mut self, eaddr: u32, dw: u32, be: u8, store: bool) {
        if !self.uw_any || self.evec == 0 || self.debugger_attached {
            return;
        }
        let (user, irq_enabled) = (self.um, !self.irq_disabled);
        let mut wps = 0;
        for (i, wp) in self.wp.iter().enumerate() {
            if wp.hits(dw, be, store, user, irq_enabled) {
                wps |= 1 << i;
            }
        }
        if wps == 0 {
            return;
        }
        match self.uw_hit.as_mut() {
            Some(hit) => hit.wps |= wps,
            None => self.uw_hit = Some(UwHit { wps, store, eaddr }),
        }
    }

    /// The `len` bytes at `ea` (1-8; a `MEMGET32` may run into the next
    /// doubleword) were accessed: [`Cpu::uw_dw`] for each doubleword, the
    /// second with `EADDR` = its start.
    pub(super) fn uw_bytes(&mut self, ea: u32, len: u32, store: bool) {
        if !self.uw_any {
            return;
        }
        let lane = ea & 7;
        let mask = (1_u32 << len) - 1;
        let first = ea & !7;
        self.uw_dw(ea, first, (mask << lane) as u8, store);
        if lane + len > 8 {
            let second = first.wrapping_add(8);
            self.uw_dw(second, second, (mask >> (8 - lane)) as u8, store);
        }
    }

    /// A doubleword access of all 8 bytes at `ea` aligned down (a 64-bit
    /// load or store, a stack push or pop): [`Cpu::uw_dw`].
    pub(super) fn uw_dword(&mut self, ea: u32, store: bool) {
        if self.uw_any {
            self.uw_dw(ea, ea & !7, 0xFF, store);
        }
    }

    /// Whether a STEP or WATCH is pending: the last instruction was the
    /// stepped one, or hit a watchpoint.
    pub(super) const fn udbg_pending(&self) -> bool {
        self.us_done.is_some() || self.uw_hit.is_some()
    }

    /// Take the pending STEP or WATCH (§13.2-§13.4): one entry, WATCH when
    /// a watchpoint hit (with SSTEP when the instruction was also the
    /// stepped one), else STEP.  The frame PC is the PC (the instruction's
    /// successor), SS the armed state ([`Cpu::enter`]); `ECAUSE`, `EADDR`,
    /// DI and the vector `EVEC` (never `TLBVEC`) as for any exception.
    // klausscc choice: a pending entry whose EVEC a debugger has since
    // cleared is dropped (with the step), as nothing could take it; the
    // hardware cannot get there (arming and hits need EVEC != 0, and EVEC
    // cannot change in user mode or between an access and its WATCH).
    pub(super) fn take_udbg_entry(&mut self) {
        let sstep = self.us_done.map(|pc| (UDBG_SSTEP, pc));
        let (cause, bits, eaddr) = match (self.uw_hit, sstep) {
            (Some(hit), _) => (
                Cause::Watch,
                hit.wps | if hit.store { UDBG_WRITE } else { 0 } | sstep.map_or(0, |(b, _)| b),
                hit.eaddr,
            ),
            (None, Some((bit, pc))) => (Cause::Step, bit, pc),
            (None, None) => return,
        };
        if self.evec == 0 {
            self.uw_hit = None;
            self.us_done = None;
            self.us_armed = false;
            return;
        }
        // The frame PC is the PC: the stepped / hitting instruction's
        // successor.  A double fault (TE = 1, a kernel SP outside kseg)
        // parks instead, and drops the entry ([`Cpu::udbg_double_fault`]).
        if !self.enter(self.pc) {
            return;
        }
        self.uw_hit = None;
        self.ecause = cause as u8;
        self.ecause_guard = false;
        self.ecause_udbg = bits;
        self.eaddr = eaddr;
        self.irq_disabled = true;
        self.pc = self.evec;
        self.exc_entries += 1;
        self.tick(CYCLES_PER_INSTRUCTION);
    }

    /// A `DOUBLE_FAULT` park in place of an entry disarms the step and drops
    /// a pending STEP, as the entry would have (`MMU_PLAN.md` §13.2, as
    /// merged to klaussCPU master in c5da780).
    // klausscc choice: a pending WATCH whose entry double-faults is dropped
    // too (the spec names the step state only); the park is what replaced
    // that entry, and a debugger resuming from it continues at the
    // successor with no WATCH.
    pub(super) const fn udbg_double_fault(&mut self) {
        self.us_armed = false;
        self.us_done = None;
        self.uw_hit = None;
    }

    /// Frame bit 45 for an entry's push: the armed step state.  The entry
    /// disarms ([`Cpu::enter`]).
    pub(super) fn udbg_take_arm(&mut self) -> u64 {
        let ss = u64::from(self.us_armed) << CTX_STEP_SHIFT;
        self.us_armed = false;
        self.us_done = None;
        ss
    }

    /// An `IRET` popped `ctx`: arm a step if it returns to user mode (PM)
    /// with SS set and `EVEC` != 0; otherwise SS does nothing.
    pub(super) const fn udbg_iret(&mut self, ctx: u64, to_user: bool) {
        self.us_armed = self.has_udbg && to_user && (ctx >> CTX_STEP_SHIFT) & 1 != 0 && self.evec != 0;
    }

    /// A debugger wrote the mode register: UM = 0 disarms the step and
    /// drops a pending STEP (klaussCPU ustep 60bcc36, the review fix-up:
    /// a step is armed only in user mode).
    pub(super) const fn udbg_mode_write(&mut self, um: bool) {
        if !um {
            self.us_armed = false;
            self.us_done = None;
        }
    }
}
