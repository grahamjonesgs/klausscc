//! A DMA coherence checker for `--sd-dma` (klaussCPU SD DMA spec v1).
//!
//! The emulator has no data cache: memory is coherent, so a driver that
//! forgets a cache clean or invalidate around a DMA works here and fails on
//! the board.  This checker makes that visible.  It does not model the
//! cache (a line may well have been evicted on the board); it tracks what a
//! driver must have done for the board to be correct whatever the cache
//! holds, per 32-byte line:
//!
//! - **dirty**: the CPU stored to the line since the last clean that covered
//!   it (`CACHE_CTRL` FLUSH or INVALIDATE, or a range clean).  A DMA that
//!   reads the line (memory to card) would send DDR's stale copy; one that
//!   writes it (card to memory) could be clobbered later by the line's
//!   writeback.  A range invalidate alone drops it, losing the CPU's data.
//! - **stale**: a DMA wrote the line since the last invalidate that covered
//!   it.  A CPU load or store to it could hit an old cached copy.
//!
//! It also flags a DMA that reads memory while a cache walk (`CACHE_STATUS`
//! `MNT_BUSY`) is still running: the walk's writebacks may not be in DDR
//! yet, and the CPU, not MMIO, is what waits for a walk; and a write to the
//! range-op registers while a walk is pending.
//!
//! Each problem is reported on stderr (the first few) and counted; the run's
//! summary line gives the totals.

/// Bytes per D-cache line.
pub(super) const LINE: u64 = 32;
/// Problems reported on stderr one by one; the rest are only counted.
const REPORTED: u64 = 16;

/// A bit per line.
#[derive(Debug)]
struct Lines(Vec<u64>);

impl Lines {
    fn new(lines: usize) -> Self {
        Self(vec![0; lines.div_ceil(64)])
    }

    fn get(&self, line: usize) -> bool {
        self.0.get(line / 64).is_some_and(|w| w >> (line % 64) & 1 != 0)
    }

    fn set(&mut self, line: usize, on: bool) {
        if let Some(w) = self.0.get_mut(line / 64) {
            if on {
                *w |= 1 << (line % 64);
            } else {
                *w &= !(1 << (line % 64));
            }
        }
    }

    fn clear_all(&mut self) {
        self.0.fill(0);
    }
}

/// The checker's state and counts.
#[derive(Debug)]
pub(super) struct Coherence {
    /// CPU stores not yet cleaned.
    dirty: Lines,
    /// DMA writes not yet invalidated.
    stale: Lines,
    /// Lines in RAM.
    lines: usize,
    /// Problems found.
    problems: u64,
}

impl Coherence {
    /// A checker for `mem_size` bytes of RAM, all lines clean.
    pub(super) fn new(mem_size: usize) -> Self {
        let lines = mem_size.div_ceil(LINE as usize);
        Self {
            dirty: Lines::new(lines),
            stale: Lines::new(lines),
            lines,
            problems: 0,
        }
    }

    /// The lines `[addr, addr + len)` touches, clipped to RAM.
    fn span(&self, addr: u64, len: u64) -> std::ops::Range<usize> {
        if len == 0 {
            return 0..0;
        }
        let first = (addr / LINE).min(self.lines as u64) as usize;
        let end = (addr.saturating_add(len).div_ceil(LINE)).min(self.lines as u64) as usize;
        first..end
    }

    fn report(&mut self, what: &str) {
        self.problems += 1;
        if self.problems <= REPORTED {
            eprintln!("klausscc dma: coherence: {what}");
        } else if self.problems == REPORTED + 1 {
            eprintln!("klausscc dma: coherence: more problems, counted only");
        }
    }

    /// Problems found so far.
    pub(super) const fn problems(&self) -> u64 {
        self.problems
    }

    /// A CPU load or store of `n` bytes at physical `addr` (RAM).
    pub(super) fn cpu_access(&mut self, addr: u32, n: u32, store: bool) {
        for line in self.span(u64::from(addr), u64::from(n.max(1))) {
            if self.stale.get(line) {
                self.stale.set(line, false);
                let what = if store { "store to" } else { "load from" };
                self.report(&format!(
                    "CPU {what} line 0x{:08X}, which a DMA wrote, without an invalidate since (it could hit a stale cached copy)",
                    line as u64 * LINE
                ));
            }
            if store {
                self.dirty.set(line, true);
            }
        }
    }

    /// The device reads `len` bytes at `addr` (memory to card); `walk_busy`:
    /// a cache walk is still running.
    pub(super) fn dma_read(&mut self, addr: u64, len: u64, walk_busy: bool) {
        if walk_busy {
            self.report(&format!(
                "DMA read of 0x{addr:08X}+{len} while CACHE_STATUS.MNT_BUSY: wait for the clean before starting the DMA"
            ));
        }
        for line in self.span(addr, len) {
            if self.dirty.get(line) {
                self.dirty.set(line, false);
                self.report(&format!(
                    "DMA read of line 0x{:08X}, which the CPU wrote and nothing cleaned since (the card would get stale data)",
                    line as u64 * LINE
                ));
            }
        }
    }

    /// The device writes `len` bytes at `addr` (card to memory).
    pub(super) fn dma_write(&mut self, addr: u64, len: u64) {
        for line in self.span(addr, len) {
            if self.dirty.get(line) {
                self.dirty.set(line, false);
                self.report(&format!(
                    "DMA write of line 0x{:08X}, which the CPU wrote and nothing cleaned since (its writeback could clobber the data)",
                    line as u64 * LINE
                ));
            }
            self.stale.set(line, true);
        }
    }

    /// `CACHE_CTRL` FLUSH: every dirty line written back.
    pub(super) fn flush_all(&mut self) {
        self.dirty.clear_all();
    }

    /// `CACHE_CTRL` INVALIDATE: written back, then dropped.
    pub(super) fn invalidate_all(&mut self) {
        self.dirty.clear_all();
        self.stale.clear_all();
    }

    /// A range op on the lines `[addr, addr + len)` touches: `clean`
    /// writes dirty lines back, `inv` drops lines (without a writeback
    /// unless `clean` too).
    pub(super) fn range(&mut self, addr: u64, len: u64, clean: bool, inv: bool) {
        for line in self.span(addr, len) {
            if inv && !clean && self.dirty.get(line) {
                self.report(&format!(
                    "range invalidate (no clean) dropped line 0x{:08X}, which the CPU wrote: its data is lost",
                    line as u64 * LINE
                ));
            }
            if clean || inv {
                self.dirty.set(line, false);
            }
            if inv {
                self.stale.set(line, false);
            }
        }
    }

    /// The range-op registers written while a walk is pending.
    pub(super) fn op_regs_while_busy(&mut self, reg: &str) {
        self.report(&format!(
            "{reg} written while CACHE_STATUS.MNT_BUSY (the walk in flight may use either value)"
        ));
    }

    /// A misuse of the SD DMA registers.
    pub(super) fn sd_misuse(&mut self, what: &str) {
        self.report(what);
    }
}
