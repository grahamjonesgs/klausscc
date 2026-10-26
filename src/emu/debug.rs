//! The debugger's view of the CPU: registers, side-effect-free memory
//! access, watchpoints and why execution stopped.

use super::Cpu;
use super::flags::Flags;
use crate::layout::is_mmio;

/// Reason the emulator stopped executing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// HALT instruction reached.
    Halt,
    /// Instruction-count cap hit (possible infinite loop).
    InstructionCap,
    /// TRAP instruction (software abort).
    Trap,
    /// Invalid / unimplemented opcode encountered.
    InvalidOpcode(u32),
    /// PC left the modelled address space.
    PcOutOfRange(u32),
    /// `WAIT` with no interrupt source able to wake it (PC after the WAIT).
    WaitForever(u32),
}

/// Data access a debugger watchpoint triggers on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchKind {
    /// Loads only.
    Read,
    /// Stores only.
    Write,
    /// Loads and stores.
    Access,
}

/// Which kind of data access an instruction made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// A load.
    Load,
    /// A store.
    Store,
}

/// The first watchpoint an instruction's data access hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WatchHit {
    /// The watchpoint's address ([`Watch::addr`]).
    pub addr: u32,
    /// The access that hit it.
    pub access: Access,
}

/// A debugger watchpoint over `[addr, addr + len)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Watch {
    /// First watched byte.
    pub addr: u32,
    /// Number of watched bytes.
    pub len: u32,
    /// Which accesses trigger it.
    pub kind: WatchKind,
}

/// Architectural registers as seen by a debugger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DebugRegs {
    /// R0..R15.
    pub r: [u64; 16],
    /// Stack pointer.
    pub sp: u32,
    /// Program counter.
    pub pc: u32,
    /// Flags nibble: bit 3 = Z, bit 2 = S, bit 1 = C, bit 0 = V.
    pub flags: u8,
    /// Per-source interrupt enables (`INT_MASK`, 4 bits).
    pub int_mask: u8,
}

impl Cpu {
    /// Record the first watchpoint the current instruction's data access hits.
    pub(super) fn note_access(&mut self, addr: u32, n: u32, access: Access) {
        if self.watches.is_empty() || self.watch_hit.is_some() {
            return;
        }
        let end = u64::from(addr) + u64::from(n);
        for w in &self.watches {
            let kind_matches = match w.kind {
                WatchKind::Read => access == Access::Load,
                WatchKind::Write => access == Access::Store,
                WatchKind::Access => true,
            };
            if kind_matches && u64::from(addr) < u64::from(w.addr) + u64::from(w.len) && u64::from(w.addr) < end {
                self.watch_hit = Some(WatchHit { addr: w.addr, access });
                return;
            }
        }
    }

    /// Snapshot of the architectural registers.
    #[must_use]
    pub fn debug_regs(&self) -> DebugRegs {
        DebugRegs {
            r: self.regs,
            sp: self.sp,
            pc: self.pc,
            flags: self.flags.to_nibble(),
            int_mask: self.int_mask,
        }
    }

    /// Overwrite the architectural registers.
    pub fn set_debug_regs(&mut self, regs: &DebugRegs) {
        self.regs = regs.r;
        self.sp = regs.sp;
        self.pc = regs.pc;
        self.flags = Flags::from_nibble(regs.flags);
        self.int_mask = regs.int_mask & 0xF;
    }

    /// Read RAM without side effects (MMIO is not touched).  Returns the
    /// number of bytes read, which stops short at the end of RAM.
    pub fn debug_read(&self, addr: u32, buf: &mut [u8]) -> usize {
        let a = addr as usize;
        if is_mmio(addr) || a >= self.mem.len() {
            return 0;
        }
        let n = buf.len().min(self.mem.len() - a);
        buf[..n].copy_from_slice(&self.mem[a..a + n]);
        n
    }

    /// Write RAM (MMIO is not touched).  Returns false if any byte is outside RAM.
    pub fn debug_write(&mut self, addr: u32, data: &[u8]) -> bool {
        let a = addr as usize;
        if is_mmio(addr) || a + data.len() > self.mem.len() {
            return false;
        }
        self.mem[a..a + data.len()].copy_from_slice(data);
        true
    }

    /// Read device registers for a debugger that asked for MMIO access:
    /// each 8-byte register the range touches is read once, with the same
    /// side effects as a CPU load (e.g. popping the UART RX FIFO).
    pub fn debug_read_mmio(&mut self, addr: u32, buf: &mut [u8]) -> usize {
        let mut done = 0;
        while done < buf.len() {
            let a = addr.wrapping_add(done as u32);
            if !is_mmio(a) {
                break;
            }
            let reg = self.mmio_read(a & !7).to_le_bytes();
            let lane = (a & 7) as usize;
            let n = (8 - lane).min(buf.len() - done);
            buf[done..done + n].copy_from_slice(&reg[lane..lane + n]);
            done += n;
        }
        done
    }

    /// Write device registers for a debugger: each aligned 8-byte register in
    /// the range is written once with the bytes given (missing bytes are 0).
    pub fn debug_write_mmio(&mut self, addr: u32, data: &[u8]) -> bool {
        let mut done = 0;
        while done < data.len() {
            let a = addr.wrapping_add(done as u32);
            if !is_mmio(a) {
                return false;
            }
            let lane = (a & 7) as usize;
            let n = (8 - lane).min(data.len() - done);
            let mut reg = [0_u8; 8];
            reg[lane..lane + n].copy_from_slice(&data[done..done + n]);
            self.mmio_write(a & !7, u64::from_le_bytes(reg));
            done += n;
        }
        true
    }

    /// The instruction word at `addr` (0 outside RAM).
    #[must_use]
    pub fn fetch_word(&self, addr: u32) -> u32 {
        self.read32(addr)
    }

    /// Advance one step — retire one instruction, or take a ready interrupt
    /// (its entry is a step of its own, so a breakpoint on the handler is
    /// seen), or finish a `WAIT`.  With `allow_irq` false (a single-step) no
    /// interrupt is taken.  Returns why execution stopped, if it did (HALT,
    /// TRAP, invalid opcode, PC out of range, WAIT forever); `None` to go on.
    pub fn step_one(&mut self, allow_irq: bool) -> Option<StopReason> {
        if self.stop.is_none() {
            self.step_inner(allow_irq);
        }
        self.stop
    }

    /// Forget a TRAP / fault stop so execution can resume (the debugger has
    /// moved the PC or patched memory).  HALT stays final.
    pub fn clear_stop(&mut self) {
        if self.stop != Some(StopReason::Halt) {
            self.stop = None;
        }
    }

    /// The watchpoints set, in the order they were added.
    #[must_use]
    pub fn watches(&self) -> &[Watch] {
        &self.watches
    }

    /// Add a watchpoint.
    pub fn add_watch(&mut self, watch: Watch) {
        self.watches.push(watch);
    }

    /// Remove a watchpoint equal to `watch`; false if there was none.
    pub fn remove_watch(&mut self, watch: &Watch) -> bool {
        let Some(pos) = self.watches.iter().position(|w| w == watch) else {
            return false;
        };
        self.watches.remove(pos);
        true
    }

    /// Take the watchpoint hit recorded by the last instruction, if any.
    pub fn take_watch_hit(&mut self) -> Option<WatchHit> {
        self.watch_hit.take()
    }
}
