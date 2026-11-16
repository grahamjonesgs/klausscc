//! A timed model of the planned unified L2 (`KLAUSSCC_L2T`), for the RTL
//! session's decisions: associativity, 64 B sectored vs 32 B lines, and a
//! next-line L2 prefetch.
//!
//! Trace-driven: the emulator's retired instruction stream feeds the
//! board's L1s (the fetch front end [`Nlb`] around the 64 KiB direct-mapped
//! 32 B I-cache, klaussCPU f799182, and the 64 KiB 2-way 32 B write-back
//! D-cache), once; their miss events (I fills, D read / write misses, DCBZ
//! installs, dirty victims) drive several memory systems ([`MemSys`]) side
//! by side, each with its own clock.  The core is in order with blocking
//! loads: each retired instruction costs a calibrated base (every cycle the
//! board spends that is not a memory stall: issue, hazards, branch bubbles,
//! micro-ITLB refills, the walker's own cycles, the D-cache hit path), and
//! each L1 miss adds the stall the memory system computes.  Times are core
//! cycles (100 MHz; the MIG UI clock is the CPU clock).
//!
//! Today's memory unit (the D-cache, `mem_read_write.sv`), as modelled:
//! - It is blocking.  A data access or an I fill that arrives while the
//!   previous miss is still installing its line or writing back its victim
//!   waits ("parks") until it is done.
//! - A read miss stalls the core until the critical doubleword (critical
//!   word first, early restart, `PIPELINE_M10`); the rest of the 32 B line
//!   arrives `rest` cycles later and then a dirty victim is written back in
//!   the shadow (no L2: a DDR write of `w` cycles).
//! - A write miss is acknowledged at the latch (no stall) and its fetch runs
//!   in the shadow; a DCBZ miss installs without a fetch.
//! - An I fill goes through the D-cache: one that hits it costs `ih`; one
//!   that misses fetches the line, installs it in the D-cache (evicting as a
//!   data miss does) and the fetch restarts `ix` after the whole 32 B line
//!   is in (the I-cache installs from the D-cache's line answer).
//! - DDR: one transaction at a time.  A read's critical 8 B arrives `rc`
//!   after the L1 miss issue when DDR is idle (`pre` of it before the DDR
//!   command, `post` after the first beat); a 32 B sector is four 64-bit
//!   beats, one per cycle.  A 32 B write occupies DDR `w`, each further
//!   sector 4 more.
//!
//! The L2 (the RTL session's spec, 2026-11-15): unified, physically
//! indexed, tree pseudo-LRU, non-inclusive, write-back; 64 B lines of two
//! 32 B sectors with per-sector valid and dirty bits (or plain 32 B lines).
//! - A hit gives the critical 8 B `h` after the L1 miss issue, the line
//!   `rest` later; it holds the data port `occ_hit` cycles (one hit per ~4).
//! - A miss is known `tag` cycles after the issue; the DDR fetches the whole
//!   line, the requested sector first, and the L1 restarts on the critical
//!   8 B as today (so `tag` + today's DDR timing).  A sector miss (the line
//!   is there from a writeback's sector allocate) fetches only the missing
//!   sector.
//! - An L1 victim (32 B) that hits the L2, or misses and allocates its
//!   sector without a fetch, holds the L2 `occ_wb` cycles; the D-cache is
//!   free after that.
//! - A dirty L2 victim (its dirty sectors) goes to a write buffer of `wb`
//!   64 B entries that drains to DDR when DDR is idle, and first when it is
//!   full; an allocation that finds it full waits for a drain.  A demand
//!   miss on a line still in the buffer takes it back (a hit at `h`).
//! - Variant P: after an I demand miss in the L2, or a demand hit on a line
//!   a prefetch brought, the next line is queued for prefetch into the L2
//!   (never the L1); it issues when DDR is idle and nothing else is waiting
//!   (demand > prefetch > drain, a full buffer first), and a demand miss
//!   drops it while still queued.  An issued prefetch is not aborted: a
//!   demand miss waits for it, and a demand for the line it is fetching
//!   waits for its data.
//! - DDR runs one transaction at a time, so background work (drains,
//!   prefetches) that started when DDR was idle delays a later demand miss;
//!   an L2 hit proceeds under it (hit-under-miss).
//!
//! Not modelled: DDR row / bank state and refresh (a constant latency);
//! data-port conflicts between DDR fills and hits (separate ports);
//! wrong-path fetch; the core overlapping a fetch stall with a data stall
//! (the board attributes overlapping cycles to one bucket; here they add).

use super::{Cache, Nlb, VALID};
use std::collections::VecDeque;

/// Request kinds, also the index of the per-kind L2 counts.
const KI: usize = 0;
const KD: usize = 1;
const KW: usize = 2;

/// Beats (cycles) per 32 B sector on the 64-bit MIG UI.
const SECTOR_BEATS: u64 = 4;

/// Timing of today's memory unit and DDR (see the module comment).
#[derive(Clone, Copy, Debug)]
pub(super) struct Params {
    pub rc: u64,
    pub pre: u64,
    pub post: u64,
    pub rest: u64,
    pub w: u64,
    pub ih: u64,
    pub ix: u64,
}

impl Params {
    /// DDR command to the first beat.
    fn rddr(&self) -> u64 {
        self.rc - self.pre - self.post
    }
}

/// One L2 shape.
#[derive(Clone, Copy, Debug)]
pub(super) struct L2Cfg {
    pub kib: u32,
    pub ways: usize,
    /// 32 or 64 (two 32 B sectors).
    pub line: u32,
    pub h: u64,
    pub wb: usize,
    pub pf: bool,
    pub tag: u64,
    pub occ_hit: u64,
    pub occ_wb: u64,
}

impl L2Cfg {
    pub(super) fn base() -> Self {
        Self { kib: 128, ways: 4, line: 64, h: 6, wb: 2, pf: false, tag: 2, occ_hit: 4, occ_wb: 2 }
    }
}

/// Tree pseudo-LRU over `ways` (a power of two, at most 8): node `i`'s bit
/// set means its LRU side is the right half.
fn plru_touch(bits: &mut u8, ways: usize, w: usize) {
    let (mut node, mut lo, mut span) = (1usize, 0usize, ways);
    while span > 1 {
        span /= 2;
        if w >= lo + span {
            *bits &= !(1 << node);
            node = 2 * node + 1;
            lo += span;
        } else {
            *bits |= 1 << node;
            node *= 2;
        }
    }
}

fn plru_victim(bits: u8, ways: usize) -> usize {
    let (mut node, mut lo, mut span) = (1usize, 0usize, ways);
    while span > 1 {
        span /= 2;
        if bits >> node & 1 != 0 {
            node = 2 * node + 1;
            lo += span;
        } else {
            node *= 2;
        }
    }
    lo
}

struct L2 {
    cfg: L2Cfg,
    sets: u32,
    /// log2(line / 32): the L1-line to L2-line shift.
    sh: u32,
    full: u8,
    /// L2 line number + 1 per slot (0 = empty).
    tag: Vec<u32>,
    valid: Vec<u8>,
    dirty: Vec<u8>,
    /// Brought by a prefetch, no demand access yet.
    pf: Vec<bool>,
    /// When the slot's data is all in (an in-flight fill).
    ready: Vec<u64>,
    plru: Vec<u8>,
}

impl L2 {
    fn new(cfg: L2Cfg) -> Self {
        let sets = (cfg.kib << 10) / cfg.line / cfg.ways as u32;
        let n = sets as usize * cfg.ways;
        let sh = (cfg.line / 32).trailing_zeros();
        Self {
            cfg,
            sets,
            sh,
            full: ((1u32 << (1 << sh)) - 1) as u8,
            tag: vec![0; n],
            valid: vec![0; n],
            dirty: vec![0; n],
            pf: vec![false; n],
            ready: vec![0; n],
            plru: vec![0; sets as usize],
        }
    }

    fn set_of(&self, line: u32) -> usize {
        (line % self.sets) as usize
    }

    fn find(&self, line: u32) -> Option<usize> {
        let b = self.set_of(line) * self.cfg.ways;
        (b..b + self.cfg.ways).find(|&i| self.tag[i] == line + 1)
    }

    /// The slot to allocate in `line`'s set: an empty way, else the PLRU's.
    fn victim(&self, line: u32) -> usize {
        let set = self.set_of(line);
        let b = set * self.cfg.ways;
        (b..b + self.cfg.ways).find(|&i| self.tag[i] == 0).unwrap_or_else(|| b + plru_victim(self.plru[set], self.cfg.ways))
    }

    fn touch(&mut self, slot: usize) {
        let ways = self.cfg.ways;
        plru_touch(&mut self.plru[slot / ways], ways, slot % ways);
    }
}

/// A write-buffer entry: a dirty L2 victim.
#[derive(Clone, Copy)]
struct Wbe {
    line: u32,
    mask: u8,
    at: u64,
}

/// Per-window counts of one memory system.
#[derive(Clone, Copy, Default)]
pub(super) struct Stats {
    pub ifill_dhit: u64,
    pub ifill_dmiss: u64,
    pub dread_miss: u64,
    pub dwrite_miss: u64,
    pub zero_miss: u64,
    pub wbacks: u64,
    /// The board counters' analogs: cycles with an I fill in flight; cycles
    /// blocked on the miss chain (parks and waits to the critical word).
    pub ic_fill: u64,
    pub dc_stall: u64,
    /// Data-side stall (parks of data accesses, read misses to the critical word).
    pub d_stall: u64,
    pub park_i: u64,
    pub park_d: u64,
    pub hits: [u64; 3],
    pub misses: [u64; 3],
    pub sector_misses: u64,
    pub ddr_rd: u64,
    pub ddr_wr: u64,
    /// Waits for a full write buffer: events, cycles of the requester
    /// (demand misses: core-visible; writebacks: memory-unit occupancy).
    pub wbfull_ev: u64,
    pub wbfull_cyc: u64,
    pub wb_fwd: u64,
    pub drains: u64,
    pub pf_queued: u64,
    pub pf_issued: u64,
    pub pf_useful: u64,
    pub pf_unused: u64,
    pub pf_dropped: u64,
    /// Demand hits on a line whose prefetch was still in flight.
    pub pf_late: u64,
    /// Demand misses that waited for background DDR work.
    pub ddr_waits: u64,
    pub ddr_wait_cyc: u64,
}

/// An L1 miss event.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Ev {
    IFill,
    Read,
    Write,
    Zero,
}

/// The state an L2 and today's system share: the timing and DDR.
struct Shared {
    p: Params,
    ddr_free: u64,
    s: Stats,
}

impl Shared {
    /// A DDR read of `n` sectors commanded at `c`: (the critical 8 B at the
    /// L1, when DDR is free again).
    fn read(&mut self, c: u64, n: u64) -> (u64, u64) {
        let p = self.p;
        self.ddr_free = c + p.rddr() + SECTOR_BEATS * n - 1;
        self.s.ddr_rd += 32 * n;
        (c + p.rddr() + p.post, self.ddr_free)
    }

    /// A drain of write-buffer entry `e`, starting at `start`.
    fn drain(&mut self, start: u64, e: Wbe) {
        let n = u64::from(e.mask.count_ones());
        self.ddr_free = start + self.p.w + SECTOR_BEATS * (n - 1);
        self.s.ddr_wr += 32 * n;
        self.s.drains += 1;
    }
}

/// An L2 with its write buffer and prefetch queue.
struct L2Sys {
    c: L2,
    port_free: u64,
    wb: VecDeque<Wbe>,
    pfq: Option<(u32, u64)>,
}

impl L2Sys {
    /// A 32 B L1 line fill requested at `s` (the L1 miss issue): (the
    /// critical 8 B at the L1, the whole 32 B line in the L1).
    fn fetch(&mut self, sh: &mut Shared, s: u64, line: u32, kind: usize) -> (u64, u64) {
        let p = sh.p;
        let cfg = self.c.cfg;
        self.bg(sh, s);
        let l2line = line >> self.c.sh;
        let bit = 1u8 << (line & ((1 << self.c.sh) - 1));
        // A miss on a line still in the write buffer takes it back.
        if self.c.find(l2line).is_none()
            && let Some(e) = self.wb.iter().position(|e| e.line == l2line).and_then(|i| self.wb.remove(i))
        {
            sh.s.wb_fwd += 1;
            let (slot, _) = self.alloc(sh, l2line, s);
            self.c.valid[slot] = e.mask;
            self.c.dirty[slot] = e.mask;
        }
        if let Some(slot) = self.c.find(l2line)
            && self.c.valid[slot] & bit != 0
        {
            let p0 = s.max(self.port_free);
            self.port_free = p0 + cfg.occ_hit;
            let mut crit = p0 + cfg.h;
            if self.c.ready[slot] > p0 {
                // A prefetch still bringing it: its data from DDR.
                sh.s.pf_late += 1;
                crit = crit.max(self.c.ready[slot] + p.post);
            }
            self.c.touch(slot);
            sh.s.hits[kind] += 1;
            if std::mem::take(&mut self.c.pf[slot]) {
                sh.s.pf_useful += 1;
                if cfg.pf {
                    self.queue_pf(sh, l2line + 1, crit);
                }
            }
            return (crit, crit + p.rest);
        }
        sh.s.misses[kind] += 1;
        if self.pfq.take().is_some() {
            sh.s.pf_dropped += 1;
        }
        let mut c = s + cfg.tag + p.pre;
        let slot = if let Some(slot) = self.c.find(l2line) {
            sh.s.sector_misses += 1;
            slot
        } else {
            // A full write buffer: the allocation waits for a drain.
            let (slot, ready) = self.alloc(sh, l2line, c);
            c = ready;
            slot
        };
        let need = self.c.full & !self.c.valid[slot];
        if sh.ddr_free > c {
            sh.s.ddr_waits += 1;
            sh.s.ddr_wait_cyc += sh.ddr_free - c;
        }
        let (crit, free) = sh.read(c.max(sh.ddr_free), u64::from(need.count_ones()));
        self.c.valid[slot] |= need;
        self.c.ready[slot] = free;
        self.c.pf[slot] = false;
        self.c.touch(slot);
        if cfg.pf && kind == KI {
            self.queue_pf(sh, l2line + 1, crit);
        }
        (crit, crit + p.rest)
    }

    /// Allocate an (empty) slot for L2 line `line` at time `at`; its
    /// victim's dirty sectors go to the write buffer.  Returns the slot and
    /// when the buffer had room (`at` unless it was full).
    fn alloc(&mut self, sh: &mut Shared, line: u32, at: u64) -> (usize, u64) {
        let slot = self.c.victim(line);
        let (old, dirty, pf) = (self.c.tag[slot], self.c.dirty[slot], self.c.pf[slot]);
        self.c.tag[slot] = line + 1;
        self.c.valid[slot] = 0;
        self.c.dirty[slot] = 0;
        self.c.pf[slot] = false;
        self.c.ready[slot] = 0;
        if old != 0 && pf {
            sh.s.pf_unused += 1;
        }
        let mut ready = at;
        if old != 0 && dirty != 0 {
            while self.wb.len() >= self.c.cfg.wb
                && let Some(e) = self.wb.pop_front()
            {
                // Full: the oldest drains first.
                sh.drain(sh.ddr_free.max(e.at), e);
                ready = ready.max(sh.ddr_free);
            }
            if ready > at {
                sh.s.wbfull_ev += 1;
                sh.s.wbfull_cyc += ready - at;
            }
            self.wb.push_back(Wbe { line: old - 1, mask: dirty, at: ready });
        }
        (slot, ready)
    }

    fn queue_pf(&mut self, sh: &mut Shared, line: u32, at: u64) {
        if self.c.find(line).is_some_and(|s| self.c.valid[s] == self.c.full) || self.wb.iter().any(|e| e.line == line) {
            return;
        }
        if self.pfq.replace((line, at)).is_some() {
            sh.s.pf_dropped += 1;
        }
        sh.s.pf_queued += 1;
    }

    /// Start the background DDR work (prefetches, drains) that can start
    /// before `until`: when DDR is idle, a full buffer's drain first, then
    /// a queued prefetch, then any drain.
    fn bg(&mut self, sh: &mut Shared, until: u64) {
        loop {
            let pf = if self.wb.len() >= self.c.cfg.wb { None } else { self.pfq };
            let start = match (pf, self.wb.front()) {
                (Some((_, at)), _) => sh.ddr_free.max(at),
                (None, Some(e)) => sh.ddr_free.max(e.at),
                (None, None) => break,
            };
            if start >= until {
                break;
            }
            if let Some((line, _)) = pf {
                self.pfq = None;
                self.prefetch(sh, line, start);
            } else if let Some(e) = self.wb.pop_front() {
                sh.drain(start, e);
            }
        }
    }

    fn prefetch(&mut self, sh: &mut Shared, line: u32, start: u64) {
        let (slot, c) = match self.c.find(line) {
            Some(s) if self.c.valid[s] == self.c.full => return,
            Some(s) => (s, start),
            None => self.alloc(sh, line, start),
        };
        let need = self.c.full & !self.c.valid[slot];
        let (_, free) = sh.read(c.max(sh.ddr_free), u64::from(need.count_ones()));
        sh.s.pf_issued += 1;
        self.c.valid[slot] |= need;
        self.c.ready[slot] = free;
        self.c.pf[slot] = true;
        self.c.touch(slot);
    }

    /// The D-cache's dirty victim, 32 B line `v`, leaves at `at`; returns
    /// when the memory unit is free again.
    fn wback(&mut self, sh: &mut Shared, at: u64, v: u32) -> u64 {
        self.bg(sh, at);
        let cfg = self.c.cfg;
        let l2line = v >> self.c.sh;
        let bit = 1u8 << (v & ((1 << self.c.sh) - 1));
        let p0 = at.max(self.port_free);
        self.port_free = p0 + cfg.occ_wb;
        let mut end = p0 + cfg.occ_wb;
        let slot = if let Some(slot) = self.c.find(l2line) {
            sh.s.hits[KW] += 1;
            slot
        } else {
            sh.s.misses[KW] += 1;
            let back = self.wb.iter().position(|e| e.line == l2line).and_then(|i| self.wb.remove(i));
            let (slot, ready) = self.alloc(sh, l2line, p0);
            end = end.max(ready + cfg.occ_wb);
            if let Some(e) = back {
                sh.s.wb_fwd += 1;
                self.c.valid[slot] = e.mask;
                self.c.dirty[slot] = e.mask;
            }
            slot
        };
        self.c.valid[slot] |= bit;
        self.c.dirty[slot] |= bit;
        self.c.touch(slot);
        end
    }
}

/// One memory system behind the board's L1s: today's (no L2) or with an L2.
pub(super) struct MemSys {
    pub name: String,
    /// Core cycles beyond the base so far.
    pub stall: u64,
    mu_free: u64,
    sh: Shared,
    l2: Option<L2Sys>,
}

impl MemSys {
    pub(super) fn new(name: String, p: Params, l2: Option<L2Cfg>) -> Self {
        Self {
            name,
            stall: 0,
            mu_free: 0,
            sh: Shared { p, ddr_free: 0, s: Stats::default() },
            l2: l2.map(|c| L2Sys { c: L2::new(c), port_free: 0, wb: VecDeque::new(), pfq: None }),
        }
    }

    pub(super) fn stats(&self) -> &Stats {
        &self.sh.s
    }

    fn clear(&mut self) {
        self.sh.s = Stats::default();
    }

    /// An L1 event at base time `base` (the core's time without stalls):
    /// `l1hit` whether the L1 D-cache held the line (for an I fill: the
    /// fill hit the D-cache), `line` the 32 B line (`pa >> 5`), `victim`
    /// the dirty 32 B line the D-cache evicted, if any.
    pub(super) fn event(&mut self, base: u64, ev: Ev, l1hit: bool, line: u32, victim: Option<u32>) {
        let t = base + self.stall;
        if l1hit && ev != Ev::IFill {
            if t < self.mu_free {
                let d = self.mu_free - t;
                let s = &mut self.sh.s;
                self.stall += d;
                s.park_d += d;
                s.d_stall += d;
                s.dc_stall += d;
            }
            return;
        }
        let s0 = t.max(self.mu_free);
        let park = s0 - t;
        let p = self.sh.p;
        let done = match ev {
            Ev::IFill if l1hit => {
                let s = &mut self.sh.s;
                s.ifill_dhit += 1;
                s.park_i += park;
                s.dc_stall += park;
                s.ic_fill += s0 + p.ih - t;
                s0 + p.ih
            }
            Ev::IFill => {
                let (crit, ld) = self.fetch(s0, line, KI);
                self.mu_free = self.victim(ld, victim);
                let s = &mut self.sh.s;
                s.ifill_dmiss += 1;
                s.park_i += park;
                s.ic_fill += ld + p.ix - t;
                s.dc_stall += crit - t;
                ld + p.ix
            }
            Ev::Read => {
                let (crit, ld) = self.fetch(s0, line, KD);
                self.mu_free = self.victim(ld, victim);
                let s = &mut self.sh.s;
                s.dread_miss += 1;
                s.park_d += park;
                s.dc_stall += crit - t;
                s.d_stall += crit - t;
                crit
            }
            Ev::Write | Ev::Zero => {
                self.mu_free = if ev == Ev::Write {
                    let (_, ld) = self.fetch(s0, line, KD);
                    self.victim(ld, victim)
                } else {
                    self.victim(s0 + 1, victim).max(s0 + 1)
                };
                let s = &mut self.sh.s;
                if ev == Ev::Write {
                    s.dwrite_miss += 1;
                } else {
                    s.zero_miss += 1;
                }
                s.park_d += park;
                s.dc_stall += park;
                s.d_stall += park;
                s0
            }
        };
        self.stall += done - t;
    }

    fn fetch(&mut self, s: u64, line: u32, kind: usize) -> (u64, u64) {
        if let Some(l2) = self.l2.as_mut() {
            return l2.fetch(&mut self.sh, s, line, kind);
        }
        let p = self.sh.p;
        let (crit, _) = self.sh.read((s + p.pre).max(self.sh.ddr_free), 1);
        (crit, crit + p.rest)
    }

    /// The D-cache's dirty victim leaves at `at`; returns when the memory
    /// unit is free again.  Today: a DDR write in the shadow.
    fn victim(&mut self, at: u64, victim: Option<u32>) -> u64 {
        let Some(v) = victim else { return at };
        self.sh.s.wbacks += 1;
        if let Some(l2) = self.l2.as_mut() {
            return l2.wback(&mut self.sh, at, v);
        }
        let start = at.max(self.sh.ddr_free);
        self.sh.drain(start, Wbe { line: v, mask: 1, at });
        self.sh.s.drains -= 1;
        self.sh.ddr_free
    }
}

/// Settings from the environment (`KLAUSSCC_L2T_*`).
fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

fn env_list(name: &str, default: &str) -> Vec<u64> {
    std::env::var(name).unwrap_or_else(|_| default.to_string()).split(',').filter_map(|v| v.trim().parse().ok()).collect()
}

/// The board's L1s and the memory systems they feed.
pub(super) struct Timed {
    nlb: Nlb,
    d: Cache,
    ddirty: Vec<bool>,
    /// Base cost per instruction, in thousandths of a cycle.
    base_milli: u64,
    /// Instructions retired (all time).
    n: u64,
    sys: Vec<MemSys>,
    /// Window: start (instructions, per-system stall); user-mode
    /// instructions; kernel entries (user -> kernel fetch transitions).
    w_n0: u64,
    w_stall0: Vec<u64>,
    w_user: u64,
    w_kentry: u64,
    last_um: bool,
    um: bool,
}

impl Timed {
    /// The model if `KLAUSSCC_L2T` is set.
    pub(super) fn from_env() -> Option<Box<Self>> {
        std::env::var_os("KLAUSSCC_L2T")?;
        let base_milli = env_u64("KLAUSSCC_L2T_BASE", 2470);
        let mk = |rc, w| Params {
            rc,
            pre: env_u64("KLAUSSCC_L2T_PRE", 2),
            post: env_u64("KLAUSSCC_L2T_POST", 2),
            rest: 3,
            w,
            ih: env_u64("KLAUSSCC_L2T_IH", 10),
            ix: env_u64("KLAUSSCC_L2T_IX", 2),
        };
        let (rc0, w0) = (env_u64("KLAUSSCC_L2T_RC", 44), env_u64("KLAUSSCC_L2T_W", 10));
        let b = L2Cfg::base();
        let mut sys = Vec::new();
        // The calibration grid: today's system and the main L2 variants at
        // each DDR timing.
        for &rc in &env_list("KLAUSSCC_L2T_RCS", "36,40,44,48") {
            for &w in &env_list("KLAUSSCC_L2T_WS", "10,16") {
                let p = mk(rc, w);
                let tag = format!("rc{rc} w{w}");
                sys.push(MemSys::new(format!("{tag} noL2"), p, None));
                sys.push(MemSys::new(format!("{tag} L2 base"), p, Some(b)));
                sys.push(MemSys::new(format!("{tag} L2 32B"), p, Some(L2Cfg { line: 32, ..b })));
                sys.push(MemSys::new(format!("{tag} L2 base+P"), p, Some(L2Cfg { pf: true, ..b })));
            }
        }
        // The rest of the sweep at the chosen timing.
        let p = mk(rc0, w0);
        let tag = format!("rc{rc0} w{w0}");
        for (name, c) in [
            ("H4", L2Cfg { h: 4, ..b }),
            ("H8", L2Cfg { h: 8, ..b }),
            ("8way", L2Cfg { ways: 8, ..b }),
            ("8way+P", L2Cfg { ways: 8, pf: true, ..b }),
            ("32B H4", L2Cfg { line: 32, h: 4, ..b }),
            ("256K", L2Cfg { kib: 256, ..b }),
            ("WB1", L2Cfg { wb: 1, ..b }),
            ("WB4", L2Cfg { wb: 4, ..b }),
            ("WB1+P", L2Cfg { wb: 1, pf: true, ..b }),
            ("WB4+P", L2Cfg { wb: 4, pf: true, ..b }),
        ] {
            sys.push(MemSys::new(format!("{tag} L2 {name}"), p, Some(c)));
        }
        let d = Cache::new("D", 64 << 10, 2, 32);
        let slots = d.sets as usize * d.ways;
        let n = sys.len();
        Some(Box::new(Self {
            nlb: Nlb::new(64 << 10, 1, 32, false),
            d,
            ddirty: vec![false; slots],
            base_milli,
            n: 0,
            sys,
            w_n0: 0,
            w_stall0: vec![0; n],
            w_user: 0,
            w_kentry: 0,
            last_um: false,
            um: false,
        }))
    }

    fn base(&self) -> u64 {
        self.n * self.base_milli / 1000
    }

    /// The D-cache access for the 32 B line at `pa`: (hit, dirty victim).
    fn daccess(&mut self, pa: u32, store: bool) -> (bool, Option<u32>) {
        let (a, m) = (self.d.accesses, self.d.misses);
        let (hit, way, evicted) = self.d.access(pa & !31);
        (self.d.accesses, self.d.misses) = (a, m);
        let set = ((pa >> 5) % self.d.sets) as usize;
        let slot = set * self.d.ways + way;
        let mut victim = None;
        if !hit {
            if let Some(tag) = evicted
                && self.ddirty[slot]
            {
                victim = Some((tag & !VALID) as u32);
            }
            self.ddirty[slot] = false;
        }
        if store {
            self.ddirty[slot] = true;
        }
        (hit, victim)
    }

    fn event(&mut self, ev: Ev, hit: bool, line: u32, victim: Option<u32>) {
        let base = self.base();
        for s in &mut self.sys {
            s.event(base, ev, hit, line, victim);
        }
    }

    /// An instruction fetch of `len` words at physical `pa`.
    pub(super) fn fetch(&mut self, pc: u32, um: bool, pa: u32, len: u32) {
        self.um = um;
        if um && !self.last_um {
            // (counted at the return to user mode; the entries balance)
        } else if !um && self.last_um {
            self.w_kentry += 1;
        }
        self.last_um = um;
        for d in (pa >> 3)..=((pa + 4 * len.max(1) - 1) >> 3) {
            let va = pc.wrapping_add((d << 3).wrapping_sub(pa & !7));
            if self.nlb.fetch(d << 3, ((va >> 16) as u16, um)) {
                let (hit, victim) = self.daccess(d << 3, false);
                self.event(Ev::IFill, hit, d >> 2, victim);
            }
        }
    }

    pub(super) fn retire(&mut self) {
        self.n += 1;
        self.w_user += u64::from(self.um);
    }

    /// A data access at physical `pa` (RAM).
    pub(super) fn data(&mut self, pa: u32, store: bool) {
        if store {
            self.nlb.snoop(pa);
        }
        let (hit, victim) = self.daccess(pa, store);
        self.event(if store { Ev::Write } else { Ev::Read }, hit, pa >> 5, victim);
    }

    /// DCBZ of the line at `pa`.
    pub(super) fn zero_line(&mut self, pa: u32) {
        self.nlb.snoop(pa);
        let (hit, victim) = self.daccess(pa, true);
        self.event(Ev::Zero, hit, pa >> 5, victim);
    }

    pub(super) fn icache_inv(&mut self) {
        self.nlb.flush();
    }

    pub(super) fn icache_range_inv(&mut self, pa: u64, len: u64) {
        self.nlb.range_inv(pa, len);
    }

    pub(super) fn discard(&mut self) {
        self.nlb.discard();
    }

    pub(super) fn clear(&mut self) {
        self.w_n0 = self.n;
        for (k, s) in self.sys.iter_mut().enumerate() {
            self.w_stall0[k] = s.stall;
            s.clear();
        }
        self.nlb.clear();
        self.w_user = 0;
        self.w_kentry = 0;
    }

    pub(super) fn report(&self, w: u64) {
        let n = self.n - self.w_n0;
        let base = n * self.base_milli / 1000;
        eprintln!(
            "L2T w{w} instructions {n} user {} kernel {} kernel_entries {} base_milli {} base_cycles {base} icnlb_demand_misses {} uitlb_misses {}",
            self.w_user,
            n - self.w_user,
            self.w_kentry,
            self.base_milli,
            self.nlb.c.misses,
            self.nlb.ut_misses
        );
        for (k, m) in self.sys.iter().enumerate() {
            let s = m.stats();
            let stall = m.stall - self.w_stall0[k];
            let rate = |h: u64, ms: u64| 100.0 * h as f64 / (h + ms).max(1) as f64;
            eprintln!(
                "L2T w{w} [{}] cycles {} stall {stall} | ifill dhit {} dmiss {} ic_fill {} park_i {} | dread_miss {} dwrite_miss {} zero_miss {} wbacks {} d_stall {} park_d {} dc_stall {} | \
L2 I {}/{} {:.1}% D {}/{} {:.1}% WB {}/{} {:.1}% sector_misses {} | ddr_rd {} ddr_wr {} drains {} | wbfull {} cyc {} wb_fwd {} ddr_waits {} cyc {} | \
pf queued {} issued {} useful {} unused {} dropped {} late {}",
                m.name,
                base + stall,
                s.ifill_dhit,
                s.ifill_dmiss,
                s.ic_fill,
                s.park_i,
                s.dread_miss,
                s.dwrite_miss,
                s.zero_miss,
                s.wbacks,
                s.d_stall,
                s.park_d,
                s.dc_stall,
                s.hits[KI],
                s.misses[KI],
                rate(s.hits[KI], s.misses[KI]),
                s.hits[KD],
                s.misses[KD],
                rate(s.hits[KD], s.misses[KD]),
                s.hits[KW],
                s.misses[KW],
                rate(s.hits[KW], s.misses[KW]),
                s.sector_misses,
                s.ddr_rd,
                s.ddr_wr,
                s.drains,
                s.wbfull_ev,
                s.wbfull_cyc,
                s.wb_fwd,
                s.ddr_waits,
                s.ddr_wait_cyc,
                s.pf_queued,
                s.pf_issued,
                s.pf_useful,
                s.pf_unused,
                s.pf_dropped,
                s.pf_late
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: Params = Params { rc: 40, pre: 2, post: 2, rest: 3, w: 10, ih: 10, ix: 2 };

    fn l2(cfg: L2Cfg) -> MemSys {
        MemSys::new("t".into(), P, Some(cfg))
    }

    #[test]
    fn plru_tree() {
        let mut b = 0u8;
        for w in 0..4 {
            plru_touch(&mut b, 4, w);
        }
        assert_eq!(plru_victim(b, 4), 0);
        plru_touch(&mut b, 4, 0);
        assert_eq!(plru_victim(b, 4), 2); // 0,1 side recent: the other pair's LRU
        let mut b = 0u8;
        for w in [3, 1, 7, 5, 0, 2, 4, 6] {
            plru_touch(&mut b, 8, w);
        }
        assert_eq!(plru_victim(b, 8), 1);
    }

    /// Today's system: a read miss stalls `rc`; its dirty victim's DDR write
    /// keeps the D-cache busy, so the next access parks.
    #[test]
    fn no_l2_read_miss_and_shadow_writeback() {
        let mut m = MemSys::new("t".into(), P, None);
        m.event(100, Ev::Read, false, 0x10, Some(0x99));
        assert_eq!(m.stall, 40);
        // line in at 143, victim written 143..153: a hit at 141 base parks.
        m.event(101, Ev::Read, true, 0x10, None);
        assert_eq!(m.stall, 40 + (153 - 141));
        assert_eq!((m.sh.s.ddr_rd, m.sh.s.ddr_wr, m.sh.s.wbacks), (32, 32, 1));
        // A write miss does not stall the core but occupies the D-cache.
        let mut m = MemSys::new("t".into(), P, None);
        m.event(0, Ev::Write, false, 0x10, None);
        assert_eq!(m.stall, 0);
        m.event(5, Ev::IFill, true, 0x20, None);
        assert_eq!(m.stall, 43 - 5 + 10); // parked until the line was in, then ih
    }

    /// An L2 miss: tag check + today's DDR; the requested sector first, the
    /// other sector after; then a hit on the other sector costs `h`.
    #[test]
    fn l2_miss_critical_sector_first_then_hit() {
        let mut m = l2(L2Cfg::base());
        m.event(0, Ev::Read, false, 0x41, None); // sector 1 of L2 line 0x20
        assert_eq!(m.stall, 2 + 40);
        assert_eq!(m.sh.ddr_free, 2 + 2 + 36 + 7); // both sectors' beats
        assert_eq!(m.sh.s.ddr_rd, 64);
        m.event(100, Ev::Read, false, 0x40, None); // the other sector: an L2 hit
        assert_eq!(m.stall, 42 + 6);
        assert_eq!((m.sh.s.hits[KD], m.sh.s.misses[KD]), (1, 1));
        // 32 B lines fetch only the line.
        let mut m = l2(L2Cfg { line: 32, ..L2Cfg::base() });
        m.event(0, Ev::Read, false, 0x41, None);
        m.event(100, Ev::Read, false, 0x40, None);
        assert_eq!((m.sh.s.misses[KD], m.sh.s.ddr_rd), (2, 64));
    }

    /// A writeback that misses allocates its sector without a fetch; a read
    /// of that sector hits, of the other sector is a sector miss (32 B).
    #[test]
    fn writeback_sector_allocate() {
        let mut m = l2(L2Cfg::base());
        m.event(0, Ev::Read, false, 0x1000, Some(0x41));
        assert_eq!((m.sh.s.misses[KW], m.sh.s.ddr_rd, m.sh.s.ddr_wr), (1, 64, 0));
        m.event(1000, Ev::Read, false, 0x41, None);
        assert_eq!(m.sh.s.hits[KD], 1);
        m.event(2000, Ev::Read, false, 0x40, None);
        assert_eq!((m.sh.s.misses[KD], m.sh.s.sector_misses, m.sh.s.ddr_rd), (2, 1, 96));
    }

    /// Hit-under-miss: an L2 hit proceeds while DDR drains a victim; a
    /// demand miss behind the drain waits for it.
    #[test]
    fn hit_under_drain_and_miss_waits() {
        // 1 KiB, 4-way, 64 B: 4 sets.  L1 lines 0, 8, 16, 24, 32 are L2
        // lines 0, 4, 8, 12, 16, all in set 0.
        let cfg = L2Cfg { kib: 1, ..L2Cfg::base() };
        let mut m = MemSys::new("t".into(), Params { w: 30, ..P }, Some(cfg));
        for (k, v) in [0u32, 8, 16, 24].into_iter().enumerate() {
            m.event(k as u64, Ev::Zero, false, 0x1000 + k as u32, Some(v));
        }
        assert_eq!((m.sh.s.misses[KW], m.stall), (4, 6));
        m.event(100, Ev::Zero, false, 0x1004, Some(32)); // evicts dirty L2 line 0
        assert_eq!(m.l2.as_ref().unwrap().wb.len(), 1);
        // At 110 the drain (started at 107, DDR idle) is running to 137.
        m.event(104, Ev::Read, false, 8, None);
        assert_eq!((m.sh.s.drains, m.sh.ddr_free, m.sh.s.hits[KD]), (1, 137, 1));
        assert_eq!(m.stall, 12); // the hit took h = 6
        // A miss at 117 (parked to 119 for the hit's line): DDR from 137.
        m.event(105, Ev::Read, false, 0x5002, None);
        assert_eq!((m.sh.s.ddr_waits, m.sh.s.ddr_wait_cyc), (1, 137 - 123));
        assert_eq!(m.stall, 12 + (137 + 38 - 117));
    }

    /// A full write buffer stalls the allocation until a drain finishes.
    #[test]
    fn full_write_buffer_stalls() {
        // 1 KiB direct-mapped 64 B (16 sets), a 1-entry buffer.
        let cfg = L2Cfg { kib: 1, ways: 1, wb: 1, ..L2Cfg::base() };
        let mut m = l2(cfg);
        m.event(0, Ev::Zero, false, 0x1000, Some(0)); // L2 line 0 dirty (set 0)
        m.event(1, Ev::Zero, false, 0x1001, Some(2)); // L2 line 1 dirty (set 1)
        // A read miss of L2 line 16 (set 0) at 12 evicts line 0 into the
        // buffer and holds DDR to 16 + 43; its L1 victim (L2 line 17, set 1)
        // evicts line 1 at 16 + 41 into the full buffer: the first drain
        // runs 59..69 and the writeback waits 12.
        m.event(10, Ev::Read, false, 32, Some(34));
        assert_eq!((m.sh.s.wbfull_ev, m.sh.s.wbfull_cyc, m.sh.s.drains, m.sh.s.ddr_wr), (1, 12, 1, 32));
        assert_eq!(m.mu_free, 16 + 41 + 12 + 2);
        assert_eq!(m.l2.as_ref().unwrap().wb.len(), 1);
    }

    /// P: an I miss queues the next line; an idle DDR fetches it; a demand
    /// miss arriving first drops it; a demand for a line still being
    /// prefetched waits for its data.
    #[test]
    fn prefetch_issue_drop_and_late() {
        let cfg = L2Cfg { pf: true, ..L2Cfg::base() };
        let mut m = l2(cfg);
        m.event(0, Ev::IFill, false, 0x40, None); // L2 line 0x20; DDR to 47
        assert_eq!((m.sh.s.pf_queued, m.stall), (1, 47));
        m.event(1000, Ev::IFill, false, 0x42, None); // line 0x21: prefetched at 47
        assert_eq!((m.sh.s.pf_issued, m.sh.s.pf_useful, m.sh.s.hits[KI], m.sh.s.pf_late), (1, 1, 1, 0));
        assert_eq!(m.sh.s.pf_queued, 2); // the hit on a prefetched line queued 0x22
        assert_eq!(m.stall, 47 + 6 + 3 + 2);
        // Drop: a D miss at 47, before the queued prefetch could start.
        let mut m = l2(cfg);
        m.event(0, Ev::IFill, false, 0x40, None);
        m.event(0, Ev::Read, false, 0x9000, None);
        assert_eq!((m.sh.s.pf_dropped, m.sh.s.pf_issued, m.sh.s.ddr_rd), (1, 0, 128));
        // Late: the prefetch started at 47 and has its data at 90.
        let mut m = l2(cfg);
        m.event(0, Ev::IFill, false, 0x40, None);
        m.event(1, Ev::IFill, false, 0x42, None);
        assert_eq!((m.sh.s.pf_issued, m.sh.s.pf_late), (1, 1));
        assert_eq!(m.stall, 47 + (92 + 3 + 2 - 48));
    }
}
