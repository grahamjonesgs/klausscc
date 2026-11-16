//! A DMA coherence checker for `--sd-dma` (klaussCPU SD DMA spec v1.2).
//!
//! The emulator has no caches: memory is coherent, so a driver that
//! forgets a cache clean or invalidate around a DMA works here and fails on
//! the board.  This checker makes that visible.  It does not model the
//! caches (a line may well have been evicted on the board); it tracks what
//! software must have done for the board to be correct whatever the caches
//! hold, per 32-byte line:
//!
//! - **dirty**: the CPU stored to the line since the last clean that covered
//!   it (`CACHE_CTRL` FLUSH or INVALIDATE, or a range clean).  A DMA that
//!   reads the line (memory to card) would send DDR's stale copy; one that
//!   writes it (card to memory) could be clobbered later by the line's
//!   writeback.  A range invalidate alone drops it, losing the CPU's data.
//! - **stale**: a DMA wrote the line since the last D-cache invalidate that
//!   covered it.  A CPU load or store to it could hit an old cached copy.
//! - **istale**: a DMA wrote the line since the last I-cache invalidate that
//!   covered it (`ICACHE_INV`, or a `RANGE_IINV`), and no CPU store has
//!   touched it since (the I-cache snoops CPU stores, not DMA).  Executing
//!   from it could run an old cached copy of the code; so could executing a
//!   line still **stale**, since I-cache fills come through the D-cache.
//!
//! A walk's effect counts when the walk completes (`MNT_BUSY` drops), not
//! when `CACHE_CTRL` is written, and a clean covers only the CPU stores
//! before its `CACHE_CTRL` write (spec v1.2 m3): a line stored to while a
//! walk is pending stays dirty after it.  The checker also flags a DMA that reads
//! or writes memory while a cache walk is still running: the walk's
//! writebacks may not be in DDR yet (or may land over the device's data),
//! and the CPU, not MMIO, is what waits for a walk.
//!
//! Each problem is reported on stderr (the first few) and counted; the run's
//! summary line gives the totals.
//!
//! (A DMA during a walk is flagged only when the walk covers the DMA's
//! lines: a whole-cache walk, or a range walk that overlaps them.)

/// Bytes per cache line.
pub(super) const LINE: u64 = 32;
/// Problems reported on stderr one by one; the rest are only counted.
const REPORTED: u64 = 16;

/// A bit per line, and how many are set.
#[derive(Debug)]
struct Lines {
    bits: Vec<u64>,
    count: u64,
}

impl Lines {
    fn new(lines: usize) -> Self {
        Self {
            bits: vec![0; lines.div_ceil(64)],
            count: 0,
        }
    }

    fn get(&self, line: usize) -> bool {
        self.bits.get(line / 64).is_some_and(|w| w >> (line % 64) & 1 != 0)
    }

    fn set(&mut self, line: usize, on: bool) {
        if let Some(w) = self.bits.get_mut(line / 64) {
            let bit = 1 << (line % 64);
            if (*w & bit != 0) != on {
                *w ^= bit;
                if on {
                    self.count += 1;
                } else {
                    self.count -= 1;
                }
            }
        }
    }

    fn clear_all(&mut self) {
        if self.count != 0 {
            self.bits.fill(0);
            self.count = 0;
        }
    }
}

/// The checker's state and counts.
#[derive(Debug)]
pub(super) struct Coherence {
    /// CPU stores not yet cleaned.
    dirty: Lines,
    /// DMA writes not yet invalidated in the D-cache.
    stale: Lines,
    /// DMA writes not yet invalidated in the I-cache.
    istale: Lines,
    /// Lines in RAM.
    lines: usize,
    /// Problems found.
    problems: u64,
    /// D-cache walks accepted and completed (sequence numbers from 1).
    walks_accepted: u64,
    walks_completed: u64,
    /// Lines the CPU stored to while a walk was pending, with the number
    /// of walks accepted before the store: walks up to that number do not
    /// clean it.
    late_stores: Vec<(u64, usize)>,
}

impl Coherence {
    /// A checker for `mem_size` bytes of RAM, all lines clean.
    pub(super) fn new(mem_size: usize) -> Self {
        let lines = mem_size.div_ceil(LINE as usize);
        Self {
            dirty: Lines::new(lines),
            stale: Lines::new(lines),
            istale: Lines::new(lines),
            lines,
            problems: 0,
            walks_accepted: 0,
            walks_completed: 0,
            late_stores: Vec::new(),
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

    /// Dirty lines in all of RAM, for a whole-cache walk's time (the
    /// caller caps it at the cache's size).
    pub(super) const fn dirty_lines(&self) -> u64 {
        self.dirty.count
    }

    /// Dirty lines among those `[addr, addr + len)` touches.
    pub(super) fn dirty_lines_in(&self, addr: u64, len: u64) -> u64 {
        if self.dirty.count == 0 {
            return 0;
        }
        self.span(addr, len).filter(|&line| self.dirty.get(line)).count() as u64
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
                if self.walks_accepted > self.walks_completed {
                    self.late_stores.push((self.walks_accepted, line));
                }
                // The I-cache snoops CPU stores.
                self.istale.set(line, false);
            }
        }
    }

    /// The CPU fetches `n` bytes of instructions at physical `addr` (RAM).
    /// I-cache fills come through the D-cache, so a line a DMA wrote must
    /// have been invalidated on both sides.
    pub(super) fn cpu_fetch(&mut self, addr: u32, n: u32) {
        if self.istale.count == 0 && self.stale.count == 0 {
            return;
        }
        for line in self.span(u64::from(addr), u64::from(n.max(1))) {
            if self.istale.get(line) {
                self.istale.set(line, false);
                self.report(&format!(
                    "CPU executes line 0x{:08X}, which a DMA wrote, without an I-cache invalidate since (it could run a stale cached copy)",
                    line as u64 * LINE
                ));
            }
            if self.stale.get(line) {
                self.stale.set(line, false);
                self.report(&format!(
                    "CPU executes line 0x{:08X}, which a DMA wrote, without a D-cache invalidate since (the I-cache fill could take a stale D-cache copy)",
                    line as u64 * LINE
                ));
            }
        }
    }

    /// The device reads `len` bytes at `addr` (memory to card); `walk_busy`:
    /// a D-cache walk over these lines is still running.
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

    /// The device writes `len` bytes at `addr` (card to memory); `walk_busy`:
    /// a D-cache walk over these lines is still running.
    pub(super) fn dma_write(&mut self, addr: u64, len: u64, walk_busy: bool) {
        if walk_busy {
            self.report(&format!(
                "DMA write of 0x{addr:08X}+{len} while CACHE_STATUS.MNT_BUSY: the walk's writebacks could land over it"
            ));
        }
        for line in self.span(addr, len) {
            if self.dirty.get(line) {
                self.dirty.set(line, false);
                self.report(&format!(
                    "DMA write of line 0x{:08X}, which the CPU wrote and nothing cleaned since (its writeback could clobber the data)",
                    line as u64 * LINE
                ));
            }
            self.stale.set(line, true);
            self.istale.set(line, true);
        }
    }

    /// A D-cache walk accepted: its sequence number, for
    /// [`Coherence::walk_completed`].
    pub(super) const fn walk_accepted(&mut self) -> u64 {
        self.walks_accepted += 1;
        self.walks_accepted
    }

    /// Walk `seq` completed (after its effect was applied): the lines the
    /// CPU stored to after it was accepted are dirty again.
    pub(super) fn walk_completed(&mut self, seq: u64) {
        self.walks_completed = seq;
        self.late_stores.retain(|&(after, _)| after >= seq);
        for i in 0..self.late_stores.len() {
            let line = self.late_stores[i].1;
            self.dirty.set(line, true);
        }
        if self.walks_completed == self.walks_accepted {
            self.late_stores.clear();
        }
    }

    /// `CACHE_CTRL` FLUSH completed: every dirty line written back.
    pub(super) fn flush_all(&mut self) {
        self.dirty.clear_all();
    }

    /// `CACHE_CTRL` INVALIDATE completed: written back, then dropped.
    pub(super) fn invalidate_all(&mut self) {
        self.dirty.clear_all();
        self.stale.clear_all();
    }

    /// A D-cache range op on the lines `[addr, addr + len)` touches
    /// completed: `clean` wrote dirty lines back, `inv` dropped lines
    /// (without a writeback unless `clean` too).
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

    /// `ICACHE_INV`: the whole I-cache dropped.
    pub(super) fn icache_all(&mut self) {
        self.istale.clear_all();
    }

    /// A `RANGE_IINV` of the lines `[addr, addr + len)` touches completed.
    pub(super) fn icache_range(&mut self, addr: u64, len: u64) {
        if self.istale.count == 0 {
            return;
        }
        for line in self.span(addr, len) {
            self.istale.set(line, false);
        }
    }
}
