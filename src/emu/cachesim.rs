//! Trace-free cache simulation for hardware what-ifs (`KLAUSSCC_CACHESIM=1`).
//!
//! Not a model of the board's timing: it counts demand misses of plain
//! caches fed with every retired instruction's fetch address and every data
//! access, all physical.  Several I-cache shapes run side by side (the
//! board's is 8 KiB direct-mapped with 16-byte lines), so a size change can
//! be judged by its relative miss count.  The D-cache is the board's
//! (64 KiB, 2-way, 32-byte lines, write-allocate, write-back, LRU); for each
//! write miss it watches whether the line is then written whole before
//! anything reads it or it is evicted: those are the line fills a
//! "no fetch on a full-line write" feature would save.  `strict` counts
//! only lines written whole by the next 32/8 data accesses, all stores to
//! that line (a store run, as `memset`, `memcpy` and `copy_page` make).
//!
//! The counts go to stderr when the run ends ([`CacheSim::report`]).
//!
//! Windowed like `KLAUSSCC_TLBSIM`: a `PERF_CTRL` clear zeroes the counts (the
//! cache contents stay), and the `CNT_ZERO_LINES` read that ends perfstat's
//! `/proc` read prints the window (`CACHESIM wN ...` lines: the board-shape
//! [`ICacheD`] set, micro-ITLB variants, `ICACHE_INV` writes, D-cache
//! counts including the I-cache fills).  The run's end prints only when no
//! window was printed.  `KLAUSSCC_PROFILE=prefix` (which also enables this
//! simulator) writes per-virtual-PC counts of each window to `prefix.N`:
//! `pc instr icmiss ndiv div_steps div_cycles nmul`.

/// One set-associative cache with LRU replacement.
struct Cache {
    name: String,
    line_bits: u32,
    sets: u32,
    ways: usize,
    /// `tag | VALID` per way per set.
    tags: Vec<u64>,
    /// Per set, way indexes from most to least recently used.
    lru: Vec<Vec<u8>>,
    accesses: u64,
    misses: u64,
    /// On a demand miss, also install the next line (the board's NLB
    /// prefetch, approximately).
    next_line: bool,
}

const VALID: u64 = 1 << 63;

impl Cache {
    fn new(name: &str, size: u32, ways: usize, line: u32) -> Self {
        let sets = size / line / ways as u32;
        Self {
            name: name.to_string(),
            line_bits: line.trailing_zeros(),
            sets,
            ways,
            tags: vec![0; (sets as usize) * ways],
            lru: (0..sets).map(|_| (0..ways as u8).collect()).collect(),
            accesses: 0,
            misses: 0,
            next_line: false,
        }
    }

    /// The line holding `addr` is in the cache (no LRU update, not counted).
    fn present(&self, addr: u32) -> bool {
        let line = u64::from(addr >> self.line_bits);
        let set = (line % u64::from(self.sets)) as usize;
        self.tags[set * self.ways..(set + 1) * self.ways].contains(&(line | VALID))
    }

    /// Install the line holding `addr` if absent, without counting it.
    fn prefetch(&mut self, addr: u32) {
        let (a, m) = (self.accesses, self.misses);
        self.access(addr);
        (self.accesses, self.misses) = (a, m);
    }

    /// Look up the line holding `addr`; on a miss allocate it (evicting the
    /// LRU way).  Returns (hit, way, evicted tag if a valid line was evicted).
    fn access(&mut self, addr: u32) -> (bool, usize, Option<u64>) {
        self.accesses += 1;
        let line = u64::from(addr >> self.line_bits);
        let set = (line % u64::from(self.sets)) as usize;
        let tag = line | VALID;
        let base = set * self.ways;
        let order = &mut self.lru[set];
        for w in 0..self.ways {
            if self.tags[base + w] == tag {
                let pos = order.iter().position(|&x| x as usize == w).unwrap_or(0);
                let way = order.remove(pos);
                order.insert(0, way);
                return (true, w, None);
            }
        }
        self.misses += 1;
        let victim = order.pop().unwrap_or(0) as usize;
        order.insert(0, victim as u8);
        let old = self.tags[base + victim];
        self.tags[base + victim] = tag;
        (false, victim, (old & VALID != 0).then_some(old))
    }
}

/// A 32-byte-line direct-mapped I-cache filled through the D-cache, as the
/// klaussCPU proposal (2026-11-11): a fill that hits the D-cache brings the
/// whole line; one that misses brings 16 bytes (critical dword plus next)
/// and installs the D-line.  Per-16-byte valid bits; the next-line buffer
/// stays 16 bytes: a miss also fills the following 16-byte block the same
/// way.  Each config has its own D-cache copy, fed every data access.
struct ICache32 {
    name: String,
    sets: u32,
    tags: Vec<u64>,
    halves: Vec<u8>,
    accesses: u64,
    misses: u64,
    /// Its own copy of the board's D-cache, fed every data access too.
    d: Cache,
}

impl ICache32 {
    fn new(size: u32) -> Self {
        let sets = size / 32;
        Self {
            name: format!("I {}K 1-way 32B via D", size >> 10),
            sets,
            tags: vec![0; sets as usize],
            halves: vec![0; sets as usize],
            accesses: 0,
            misses: 0,
            d: Cache::new("D", 64 << 10, 2, DLINE),
        }
    }

    /// Validate the 16-byte block at `pa` (a fill or the NLB's prefetch).
    fn fill(&mut self, pa: u32) {
        let line = u64::from(pa >> 5);
        let set = (line % u64::from(self.sets)) as usize;
        let half = 1u8 << ((pa >> 4) & 1);
        if self.tags[set] != (line | VALID) {
            self.tags[set] = line | VALID;
            self.halves[set] = 0;
        }
        // The fill comes through the D-cache: a hit gives the whole line.
        if self.d.present(pa) {
            self.halves[set] = 3;
        } else {
            self.halves[set] |= half;
        }
        self.d.access(pa);
    }

    /// Fetch the 16-byte block holding `pa`.
    fn access(&mut self, pa: u32) {
        self.accesses += 1;
        let line = u64::from(pa >> 5);
        let set = (line % u64::from(self.sets)) as usize;
        let half = 1u8 << ((pa >> 4) & 1);
        if self.tags[set] != (line | VALID) {
            self.tags[set] = line | VALID;
            self.halves[set] = 0;
        }
        if self.halves[set] & half != 0 {
            return;
        }
        self.misses += 1;
        self.fill(pa);
        // The next-line buffer (16 bytes) prefetches the following block.
        self.fill(pa.wrapping_add(16) & !15);
    }
}


/// The board's I-cache, as `pipeline_core.sv` has it, generalised: tag per
/// line, valid bit per doubleword, LRU.  A demand miss on doubleword `d`
/// fills `d` and, for an even `d` in a 16-byte line, the next one too
/// (`ic_dv <= {1, m_next_valid}`; an odd fill is that doubleword alone), so
/// the other doubleword of a line can miss separately.  `whole` fills the
/// whole line (the 32-byte what-ifs: the D-cache line is fetched anyway).
/// The next-line buffer (NLB) only looks up lines already in the I-cache and
/// never fills (`PIPELINE_M9_FETCH_PREFETCH.md`); `nl_fill` is the other
/// assumption (a miss also installs the whole next line), kept to compare.
/// `PERF_IC_MISS` counts the demand fills issued, i.e. these misses.
/// A CPU store to a cached line drops that line (the M7b snoop); `ICACHE_INV`
/// drops everything.
struct ICacheD {
    name: String,
    line_bits: u32,
    sets: u32,
    ways: usize,
    /// `line | VALID` per way per set (0 = empty).
    tags: Vec<u64>,
    /// Valid doublewords per way per set.
    valid: Vec<u8>,
    stamp: Vec<u64>,
    clock: u64,
    whole: bool,
    nl_fill: bool,
    accesses: u64,
    misses: u64,
    snoop_kills: u64,
}

impl ICacheD {
    fn new(size: u32, ways: usize, line: u32, whole: bool, nl_fill: bool) -> Self {
        let sets = size / line / ways as u32;
        let n = (sets as usize) * ways;
        let mut name = format!("{}K {}-way {}B", size >> 10, ways, line);
        if nl_fill {
            name += " +NL-fill";
        } else if whole {
            name += " whole-line fill";
        }
        Self {
            name,
            line_bits: line.trailing_zeros(),
            sets,
            ways,
            tags: vec![0; n],
            valid: vec![0; n],
            stamp: vec![0; n],
            clock: 0,
            whole,
            nl_fill,
            accesses: 0,
            misses: 0,
            snoop_kills: 0,
        }
    }

    fn all_mask(&self) -> u8 {
        ((1u32 << (1 << (self.line_bits - 3))) - 1) as u8
    }

    fn fill_mask(&self, dw: u32) -> u8 {
        if self.whole || self.line_bits > 4 {
            self.all_mask()
        } else if dw & 1 == 0 {
            3 << dw
        } else {
            1 << dw
        }
    }

    /// The way holding `line` in `set`, or the victim (empty, else LRU).
    fn way_of(&self, set: usize, tag: u64) -> (usize, bool) {
        let base = set * self.ways;
        let mut victim = 0;
        let mut best = u64::MAX;
        for w in 0..self.ways {
            if self.tags[base + w] == tag {
                return (base + w, true);
            }
            let key = if self.tags[base + w] == 0 { 0 } else { self.stamp[base + w] + 1 };
            if key < best {
                best = key;
                victim = base + w;
            }
        }
        (victim, false)
    }

    fn install(&mut self, line: u64, mask: u8) {
        let set = (line % u64::from(self.sets)) as usize;
        let tag = line | VALID;
        self.clock += 1;
        let (i, hit) = self.way_of(set, tag);
        if !hit {
            self.tags[i] = tag;
            self.valid[i] = 0;
        }
        self.valid[i] |= mask;
        self.stamp[i] = self.clock;
    }

    /// A demand lookup of the doubleword at `pa`; true when it missed.
    fn access(&mut self, pa: u32) -> bool {
        self.accesses += 1;
        let line = u64::from(pa >> self.line_bits);
        let dw = (pa >> 3) & ((1 << (self.line_bits - 3)) - 1);
        let set = (line % u64::from(self.sets)) as usize;
        let (i, hit) = self.way_of(set, line | VALID);
        if hit {
            self.clock += 1;
            self.stamp[i] = self.clock;
            if self.valid[i] >> dw & 1 != 0 {
                return false;
            }
        }
        self.misses += 1;
        let mask = self.fill_mask(dw);
        self.install(line, mask);
        if self.nl_fill {
            let m = self.all_mask();
            self.install(line + 1, m);
        }
        true
    }

    /// A CPU store to `pa`: drop the line if cached.
    fn snoop(&mut self, pa: u32) {
        let line = u64::from(pa >> self.line_bits);
        let set = (line % u64::from(self.sets)) as usize;
        let (i, hit) = self.way_of(set, line | VALID);
        if hit {
            self.tags[i] = 0;
            self.valid[i] = 0;
            self.snoop_kills += 1;
        }
    }

    fn flush(&mut self) {
        self.tags.fill(0);
        self.valid.fill(0);
    }

    fn clear(&mut self) {
        self.accesses = 0;
        self.misses = 0;
        self.snoop_kills = 0;
    }
}

/// An N-entry LRU micro-ITLB on (VPN, user mode) of the fetch.
struct MicroItlb {
    entries: usize,
    /// Emptied by TLBWI too (the board's `tlb_discard`).
    on_write: bool,
    /// Most recently used first.
    keys: Vec<(u16, bool)>,
    misses: u64,
}

impl MicroItlb {
    fn lookup(&mut self, key: (u16, bool)) {
        if let Some(i) = self.keys.iter().position(|&k| k == key) {
            if i != 0 {
                self.keys.remove(i);
                self.keys.insert(0, key);
            }
            return;
        }
        self.misses += 1;
        self.keys.insert(0, key);
        self.keys.truncate(self.entries);
    }
}

/// Per-PC counts (`KLAUSSCC_PROFILE=prefix`).
#[derive(Default, Clone, Copy)]
struct Ent {
    instr: u64,
    /// Misses in the board-shape I-cache ([`ICacheD`] 0).
    icmiss: u64,
    ndiv: u64,
    div_steps: u64,
    div_cycles: u64,
    nmul: u64,
    /// D-cache misses (loads and stores) at this PC.
    dmiss: u64,
    /// Board-shape I fills at this PC that also missed the D-cache.
    ifdmiss: u64,
}

/// Cycles a divide holds EX besides its steps: the issue cycle, the
/// leading-zero skip (`MD_PREP`) and the finishing cycle (`pipeline_core.sv`
/// `div_ph`).
const DIV_SETUP: u64 = 3;

#[derive(Default)]
struct Hasher32(u64);
impl std::hash::Hasher for Hasher32 {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, b: &[u8]) {
        for &x in b {
            self.0 = (self.0 ^ u64::from(x)).wrapping_mul(0x100_0000_01b3);
        }
    }
    fn write_u32(&mut self, v: u32) {
        self.0 = u64::from(v).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    }
}

struct Profile {
    prefix: String,
    map: std::collections::HashMap<u32, Ent, std::hash::BuildHasherDefault<Hasher32>>,
    pc: u32,
}

impl Profile {
    fn dump(&self, window: u64) {
        use std::io::Write;
        let path = format!("{}.{}", self.prefix, window);
        let Ok(f) = std::fs::File::create(&path) else {
            eprintln!("cachesim: cannot write {path}");
            return;
        };
        let mut w = std::io::BufWriter::new(f);
        let mut v: Vec<_> = self.map.iter().collect();
        v.sort_by_key(|(pc, _)| **pc);
        let _ = writeln!(w, "# pc instr icmiss ndiv div_steps div_cycles nmul dmiss ifdmiss");
        for (pc, e) in v {
            let _ = writeln!(w, "{:08x} {} {} {} {} {} {} {} {}", pc, e.instr, e.icmiss, e.ndiv, e.div_steps, e.div_cycles, e.nmul, e.dmiss, e.ifdmiss);
        }
    }
}

/// A unified L2 behind both L1s: LRU, write-allocate, write-back.  Fed by
/// the L1 line fills (I through the D-cache, so only those that miss it) and
/// the D-cache's dirty evictions.
struct L2 {
    name: String,
    line_bits: u32,
    sets: u32,
    ways: usize,
    tags: Vec<u64>,
    dirty: Vec<bool>,
    stamp: Vec<u64>,
    clock: u64,
    /// By kind: 0 I fill, 1 D fill, 2 writeback.
    hits: [u64; 3],
    misses: [u64; 3],
    dirty_evicts: u64,
}

impl L2 {
    fn new(size: u32, ways: usize, line: u32) -> Self {
        let sets = size / line / ways as u32;
        let n = sets as usize * ways;
        Self {
            name: format!("L2 {}K {}-way {}B", size >> 10, ways, line),
            line_bits: line.trailing_zeros(),
            sets,
            ways,
            tags: vec![0; n],
            dirty: vec![false; n],
            stamp: vec![0; n],
            clock: 0,
            hits: [0; 3],
            misses: [0; 3],
            dirty_evicts: 0,
        }
    }

    fn access(&mut self, pa: u32, kind: usize) {
        let line = u64::from(pa >> self.line_bits);
        let set = (line % u64::from(self.sets)) as usize;
        let tag = line | VALID;
        let base = set * self.ways;
        self.clock += 1;
        let mut victim = base;
        let mut best = u64::MAX;
        for i in base..base + self.ways {
            if self.tags[i] == tag {
                self.hits[kind] += 1;
                self.stamp[i] = self.clock;
                self.dirty[i] |= kind == 2;
                return;
            }
            let key = if self.tags[i] == 0 { 0 } else { self.stamp[i] + 1 };
            if key < best {
                best = key;
                victim = i;
            }
        }
        self.misses[kind] += 1;
        if self.tags[victim] != 0 && self.dirty[victim] {
            self.dirty_evicts += 1;
        }
        self.tags[victim] = tag;
        self.dirty[victim] = kind == 2;
        self.stamp[victim] = self.clock;
    }

    fn clear(&mut self) {
        self.hits = [0; 3];
        self.misses = [0; 3];
        self.dirty_evicts = 0;
    }
}

/// A return-address stack of `depth` entries, circular: a push over a full
/// stack overwrites the oldest.
struct Ras {
    depth: usize,
    stack: Vec<u32>,
    top: usize,
    count: usize,
    mispredicts: u64,
}

impl Ras {
    fn push(&mut self, ra: u32) {
        self.stack[self.top] = ra;
        self.top = (self.top + 1) % self.depth;
        self.count = (self.count + 1).min(self.depth);
    }

    fn pop(&mut self, target: u32) {
        if self.count == 0 {
            self.mispredicts += 1;
            return;
        }
        self.top = (self.top + self.depth - 1) % self.depth;
        self.count -= 1;
        if self.stack[self.top] != target {
            self.mispredicts += 1;
        }
    }
}

/// Bucket labels of [`Hist`].
const BUCKETS: [&str; 10] = ["0", "1", "2", "3", "4-7", "8-15", "16-31", "32-63", "64-127", "128+"];

/// A histogram of distances in the [`BUCKETS`].
#[derive(Clone, Copy, Default)]
struct Hist([u64; 10]);

impl Hist {
    fn bucket(d: u64) -> usize {
        match d {
            0..=3 => d as usize,
            4..=7 => 4,
            8..=15 => 5,
            16..=31 => 6,
            32..=63 => 7,
            64..=127 => 8,
            _ => 9,
        }
    }

    fn add(&mut self, d: u64) {
        self.0[Self::bucket(d)] += 1;
    }

    fn total(&self) -> u64 {
        self.0.iter().sum()
    }

    /// `n=T 0:c/cum% 1:c/cum% ...`
    fn line(&self) -> String {
        let t = self.total();
        let mut s = format!("n={t}");
        let mut cum = 0;
        for (b, &c) in BUCKETS.iter().zip(&self.0) {
            cum += c;
            s += &format!(" {b}:{c}/{:.1}%", 100.0 * cum as f64 / t.max(1) as f64);
        }
        s
    }
}

/// L1 miss events (D fills, I fills that miss the D-cache, D writebacks) and
/// the distance between consecutive ones, in retired instructions (index 0)
/// and modelled cycles (index 1).
#[derive(Default)]
struct MissDist {
    /// (instructions, cycles) of the last D fill / of any event.
    last_d: Option<(u64, u64)>,
    last_any: Option<(u64, u64)>,
    /// A fill at `.0` saw `.1` as the previous event of any kind: its own
    /// writeback (same instant) measures from that, so a fill's victim
    /// writeback is not "close" to the fill that caused it.
    wb_ref: Option<((u64, u64), Option<(u64, u64)>)>,
    /// Counts: D fills, I fills, writebacks.
    n: [u64; 3],
    /// D fill to previous D fill.
    d_d: [Hist; 2],
    /// D fill to previous event of any kind.
    d_any: [Hist; 2],
    /// Any event to previous event of any kind.
    any_any: [Hist; 2],
    /// Writeback to previous event of any kind (before its own fill).
    wb_any: [Hist; 2],
}

impl MissDist {
    /// An event of `kind` (0 D fill, 1 I fill, 2 writeback) at `now`.
    fn event(&mut self, kind: usize, now: (u64, u64)) {
        self.n[kind] += 1;
        fn dist(now: (u64, u64), prev: Option<(u64, u64)>, h: &mut [Hist; 2]) {
            if let Some(p) = prev {
                h[0].add(now.0 - p.0);
                h[1].add(now.1 - p.1);
            }
        }
        let prev_any = match self.wb_ref {
            Some((at, r)) if kind == 2 && at == now => r,
            _ => self.last_any,
        };
        if kind == 0 {
            dist(now, self.last_d, &mut self.d_d);
            dist(now, prev_any, &mut self.d_any);
        }
        if kind == 2 {
            dist(now, prev_any, &mut self.wb_any);
        }
        dist(now, prev_any, &mut self.any_any);
        if kind != 2 {
            self.wb_ref = Some((now, self.last_any));
        }
        if kind == 0 {
            self.last_d = Some(now);
        }
        self.last_any = Some(now);
    }

    fn clear(&mut self) {
        *self = Self::default();
    }
}

/// A write-missed line being watched for a whole-line write.
#[derive(Clone, Copy)]
struct Pending {
    /// Bytes written since the miss (bit per byte of the 32-byte line).
    mask: u32,
    /// Data accesses since the miss (for `strict`).
    seen: u32,
    /// Every access since the miss was a store to this line.
    run: bool,
}

pub(super) struct CacheSim {
    icaches: Vec<Cache>,
    icaches32: Vec<ICache32>,
    /// The per-doubleword-valid I-caches; 0 is the board's.
    icd: Vec<ICacheD>,
    profile: Option<Profile>,
    l2: Vec<L2>,
    /// Per D-cache slot: the line is dirty.
    ddirty: Vec<bool>,
    ras: Vec<Ras>,
    calls: u64,
    rets: u64,
    /// Entries to the TLB-refill vector this window (the software
    /// handler's refills and page faults).
    tlb_entries: u64,
    /// Hardware TLB walker refills / walks that fell back to the exception,
    /// this window.
    walker_refills: u64,
    walker_fallbacks: u64,
    /// DIV / MOD (index 0 / 1): counts, divisor-zero counts and histograms
    /// of steps (64 - clz|dividend|) and qwidth by value 0..=64.
    div_n: [u64; 2],
    div_zero: [u64; 2],
    div_steps_hist: [[u64; 65]; 2],
    div_qw_hist: [[u64; 65]; 2],
    /// `ICACHE_INV` writes this window.
    icache_inv: u64,
    /// The 1-entry micro-ITLB (`pipeline_core.sv` `ut_*`): the fetch's
    /// (VPN, user mode).  A lookup of any other page refills it; while the
    /// refill runs `PERF_IC_MISS`'s condition holds (~3 cycles) without any
    /// I-cache fill.  Emptied by TLBWI, TLBINV, ASID and STATUS writes
    /// (`tlb_discard`).
    uit: Vec<MicroItlb>,
    /// Board-shape I-cache fills that missed / hit the D-cache (fills go
    /// through it; the board counts the misses in `dc_read_misses`).
    ifill_dmiss: u64,
    ifill_dhit: u64,
    /// TLBWI, TLBINV, MTSR ASID, MTSR STATUS, walker refills this window.
    uit_discards: [u64; 5],
    window: u64,
    /// (instructions retired, modelled cycles) as of the current fetch.
    now: (u64, u64),
    dist: MissDist,
    /// A window has been printed since the last clear.
    reported: bool,
    ever_reported: bool,
    dcache: Cache,
    /// Per D-cache slot (set * ways + way): a pending write-missed line.
    pending: Vec<Option<Pending>>,
    /// The slot of the most recent write miss still in a store run.
    run_slot: Option<usize>,
    loads: u64,
    stores: u64,
    read_misses: u64,
    write_misses: u64,
    full_line: u64,
    full_line_strict: u64,
    /// Lines installed by DCBZ (no fetch, no miss counted).
    zero_lines: u64,
}

const DLINE: u32 = 32;

impl CacheSim {
    /// The simulator if `KLAUSSCC_CACHESIM` is set.
    pub(super) fn from_env() -> Option<Box<Self>> {
        let profile = std::env::var("KLAUSSCC_PROFILE").ok().map(|prefix| Profile { prefix, map: Default::default(), pc: 0 });
        if profile.is_none() {
            std::env::var_os("KLAUSSCC_CACHESIM")?;
        }
        let mut icaches = Vec::new();
        for &(size, ways, line, nl) in &[
            (8u32 << 10, 1usize, 16u32, false),
            (8 << 10, 1, 16, true),
            (16 << 10, 1, 16, true),
            (32 << 10, 1, 16, true),
            (64 << 10, 1, 16, true),
            (16 << 10, 2, 16, true),
            (8 << 10, 1, 32, true),
            (16 << 10, 1, 32, true),
        ] {
            let mut c = Cache::new(&format!("I {}K {}-way {}B{}", size >> 10, ways, line, if nl { "+NL" } else { "" }), size, ways, line);
            c.next_line = nl;
            icaches.push(c);
        }
        let dcache = Cache::new("D 64K 2-way 32B", 64 << 10, 2, DLINE);
        let slots = (dcache.sets as usize) * dcache.ways;
        Some(Box::new(Self {
            icaches,
            icaches32: vec![ICache32::new(8 << 10), ICache32::new(16 << 10), ICache32::new(32 << 10)],
            icd: [
                (16u32 << 10, 1usize, 16u32, false, false),
                (16 << 10, 1, 16, false, true),
                (8 << 10, 1, 16, false, false),
                (16 << 10, 2, 16, false, false),
                (16 << 10, 4, 16, false, false),
                (32 << 10, 1, 16, false, false),
                (32 << 10, 2, 16, false, false),
                (32 << 10, 4, 16, false, false),
                (64 << 10, 2, 16, false, false),
                (16 << 10, 1, 32, true, false),
                (16 << 10, 2, 32, true, false),
                (32 << 10, 2, 32, true, false),
                (64 << 10, 1, 16, false, false),
                (32 << 10, 1, 32, true, false),
                (64 << 10, 1, 32, true, false),
                (64 << 10, 2, 32, true, false),
            ]
            .iter()
            .map(|&(sz, w, l, whole, nl)| ICacheD::new(sz, w, l, whole, nl))
            .collect(),
            profile,
            l2: [(64u32, 32u32), (128, 32), (192, 32), (64, 64), (128, 64), (192, 64)]
                .iter()
                .map(|&(kb, line)| L2::new(kb << 10, 4, line))
                .collect(),
            ddirty: vec![false; slots],
            ras: [4usize, 8, 16].iter().map(|&d| Ras { depth: d, stack: vec![0; d], top: 0, count: 0, mispredicts: 0 }).collect(),
            calls: 0,
            rets: 0,
            tlb_entries: 0,
            walker_refills: 0,
            walker_fallbacks: 0,
            div_n: [0; 2],
            div_zero: [0; 2],
            div_steps_hist: [[0; 65]; 2],
            div_qw_hist: [[0; 65]; 2],
            icache_inv: 0,
            uit: [(1usize, true), (2, true), (4, true), (8, true), (1, false), (2, false), (4, false)]
                .iter()
                .map(|&(entries, on_write)| MicroItlb { entries, on_write, keys: Vec::new(), misses: 0 })
                .collect(),
            ifill_dmiss: 0,
            ifill_dhit: 0,
            uit_discards: [0; 5],
            window: 0,
            now: (0, 0),
            dist: MissDist::default(),
            reported: true,
            ever_reported: false,
            dcache,
            pending: vec![None; slots],
            run_slot: None,
            loads: 0,
            stores: 0,
            read_misses: 0,
            write_misses: 0,
            full_line: 0,
            full_line_strict: 0,
            zero_lines: 0,
        }))
    }

    /// A D-cache victim leaves `slot`: a dirty one is written back into the L2;
    /// the slot's new line starts clean.
    fn evict(&mut self, slot: usize, evicted: Option<u64>) {
        if let Some(tag) = evicted
            && self.ddirty[slot]
        {
            let addr = ((tag & !VALID) as u32) << 5;
            self.dist.event(2, self.now);
            for l in &mut self.l2 {
                l.access(addr, 2);
            }
        }
        self.ddirty[slot] = false;
    }

    /// A taken CALL pushes its return address.
    pub(super) fn call(&mut self, ra: u32) {
        self.calls += 1;
        for r in &mut self.ras {
            r.push(ra);
        }
    }

    /// A RET or LEAVERET to `target`.
    pub(super) fn ret(&mut self, target: u32) {
        self.rets += 1;
        for r in &mut self.ras {
            r.pop(target);
        }
    }

    /// An exception entered the TLB-refill vector.
    pub(super) fn tlb_enter(&mut self) {
        self.tlb_entries += 1;
    }

    /// The hardware TLB walker refilled the TLB, or fell back to the
    /// exception (its page-table reads are [`CacheSim::data`] loads).
    pub(super) fn tlb_walk(&mut self, refilled: bool) {
        if refilled {
            self.walker_refills += 1;
        } else {
            self.walker_fallbacks += 1;
        }
    }

    /// An instruction fetch of `len` words at physical `pa`.
    pub(super) fn fetch(&mut self, pc: u32, um: bool, pa: u32, len: u32, now: (u64, u64)) {
        self.now = now;
        let key = ((pc >> 16) as u16, um);
        for u in &mut self.uit {
            u.lookup(key);
        }
        let mut board_miss = false;
        for d in (pa >> 3)..=((pa + 4 * len.max(1) - 1) >> 3) {
            for (k, c) in self.icd.iter_mut().enumerate() {
                let m = c.access(d << 3);
                board_miss |= m && k == 0;
            }
        }
        if board_miss {
            // The fill goes through the D-cache (one 32-byte line).
            let line_addr = pa & !(DLINE - 1);
            let set = (u64::from(pa >> 5) % u64::from(self.dcache.sets)) as usize;
            let (a, m) = (self.dcache.accesses, self.dcache.misses);
            let (hit, way, evicted) = self.dcache.access(line_addr);
            (self.dcache.accesses, self.dcache.misses) = (a, m);
            if hit {
                self.ifill_dhit += 1;
            } else {
                self.ifill_dmiss += 1;
                let slot = set * self.dcache.ways + way;
                self.pending[slot] = None;
                self.dist.event(1, self.now);
                self.evict(slot, evicted);
                for l in &mut self.l2 {
                    l.access(line_addr, 0);
                }
                if let Some(p) = self.profile.as_mut() {
                    p.map.entry(pc).or_default().ifdmiss += 1;
                }
            }
        }
        if let Some(p) = self.profile.as_mut() {
            p.pc = pc;
            if board_miss {
                p.map.entry(pc).or_default().icmiss += 1;
            }
        }
        self.reported = false;
        for c in &mut self.icaches32 {
            let first = pa >> 4;
            let last = (pa + 4 * len.max(1) - 1) >> 4;
            for b in first..=last {
                c.access(b << 4);
            }
        }
        for c in &mut self.icaches {
            let first = pa >> c.line_bits;
            let last = (pa + 4 * len.max(1) - 1) >> c.line_bits;
            for l in first..=last {
                let (hit, _, _) = c.access(l << c.line_bits);
                if !hit && c.next_line {
                    c.prefetch((l + 1) << c.line_bits);
                }
            }
        }
    }


    /// The instruction fetched by the last [`CacheSim::fetch`] retired.
    pub(super) fn retire(&mut self) {
        if let Some(p) = self.profile.as_mut() {
            p.map.entry(p.pc).or_default().instr += 1;
        }
    }

    /// A MUL (any form) executes.
    pub(super) fn mul(&mut self) {
        if let Some(p) = self.profile.as_mut() {
            p.map.entry(p.pc).or_default().nmul += 1;
        }
    }

    /// A DIV/MOD executes: `steps` = significant bits of the magnitude of
    /// the dividend (leading zeros are skipped); a zero divisor takes one
    /// cycle.
    pub(super) fn div(&mut self, a: u64, b: u64, signed: bool, is_mod: bool) {
        let k = usize::from(is_mod);
        self.div_n[k] += 1;
        if b == 0 {
            self.div_zero[k] += 1;
        } else {
            let mag = |x: u64| if signed && (x as i64) < 0 { x.wrapping_neg() } else { x };
            let (ma, mb) = (mag(a), mag(b));
            let steps = 64 - ma.leading_zeros();
            let qw = (mb.leading_zeros() + 1).saturating_sub(ma.leading_zeros());
            self.div_steps_hist[k][steps as usize] += 1;
            self.div_qw_hist[k][qw as usize] += 1;
        }
        if let Some(p) = self.profile.as_mut() {
            let e = p.map.entry(p.pc).or_default();
            e.ndiv += 1;
            if b == 0 {
                e.div_cycles += 1;
            } else {
                let mag = if signed && (a as i64) < 0 { a.wrapping_neg() } else { a };
                let steps = u64::from(64 - mag.leading_zeros());
                e.div_steps += steps;
                e.div_cycles += steps + DIV_SETUP;
            }
        }
    }

    /// A write to `CACHE_CTRL` with `ICACHE_INV` (bit 3): the core drops its
    /// whole I-cache.
    pub(super) fn icache_inv(&mut self) {
        self.icache_inv += 1;
        for c in &mut self.icd {
            c.flush();
        }
    }

    /// The micro-ITLB is emptied (`kind`: 0 TLBWI, 1 TLBINV, 2 ASID, 3
    /// STATUS, 4 a walker refill, which is a TLB write as TLBWI's is).
    pub(super) fn uitlb_discard(&mut self, kind: usize) {
        for u in &mut self.uit {
            if !matches!(kind, 0 | 4) || u.on_write {
                u.keys.clear();
            }
        }
        self.uit_discards[kind] += 1;
    }

    /// `PERF_CTRL` clear: zero the window's counts (the contents stay).
    pub(super) fn clear(&mut self) {
        for c in &mut self.icd {
            c.clear();
        }
        for c in &mut self.icaches {
            c.accesses = 0;
            c.misses = 0;
        }
        for c in &mut self.icaches32 {
            c.accesses = 0;
            c.misses = 0;
        }
        for l in &mut self.l2 {
            l.clear();
        }
        for r in &mut self.ras {
            r.mispredicts = 0;
        }
        self.calls = 0;
        self.rets = 0;
        self.tlb_entries = 0;
        self.walker_refills = 0;
        self.walker_fallbacks = 0;
        self.div_n = [0; 2];
        self.div_zero = [0; 2];
        self.div_steps_hist = [[0; 65]; 2];
        self.div_qw_hist = [[0; 65]; 2];
        self.icache_inv = 0;
        for u in &mut self.uit {
            u.misses = 0;
        }
        self.ifill_dmiss = 0;
        self.ifill_dhit = 0;
        self.uit_discards = [0; 5];
        self.loads = 0;
        self.stores = 0;
        self.read_misses = 0;
        self.write_misses = 0;
        self.full_line = 0;
        self.full_line_strict = 0;
        self.zero_lines = 0;
        self.dist.clear();
        if let Some(p) = self.profile.as_mut() {
            p.map.clear();
        }
        self.reported = true;
    }

    /// Print the window (once per window; at the end of the run only when no
    /// window was printed before) and dump the profile.
    pub(super) fn report_window(&mut self, instr: u64, at_end: bool) {
        if self.reported || (at_end && self.ever_reported) {
            return;
        }
        self.reported = true;
        self.ever_reported = true;
        self.window += 1;
        let w = self.window;
        eprintln!("CACHESIM window {w}: {instr} instructions, {} ICACHE_INV writes", self.icache_inv);
        eprintln!(
            "CACHESIM w{w} uITLB discards: tlbwi {} tlbinv {} asid {} status {} walker {}",
            self.uit_discards[0], self.uit_discards[1], self.uit_discards[2], self.uit_discards[3], self.uit_discards[4]
        );
        for u in &self.uit {
            eprintln!(
                "CACHESIM w{w} uITLB {} entries, {} misses {:>10} per1k {:.2}",
                u.entries,
                if u.on_write { "cleared by TLBWI," } else { "kept over TLBWI," },
                u.misses,
                1000.0 * u.misses as f64 / instr.max(1) as f64
            );
        }
        eprintln!(
            "CACHESIM w{w} board-shape I fills through the D-cache: {} miss, {} hit; D read misses incl. I fills {}",
            self.ifill_dmiss,
            self.ifill_dhit,
            self.read_misses + self.ifill_dmiss
        );
        for (k, c) in self.icd.iter().enumerate() {
            eprintln!(
                "CACHESIM w{w} I {:<26} {}misses {:>10} accesses {:>11} per1k {:>8.2} snoop_kills {}",
                c.name,
                if k == 0 { "(board) " } else { "" },
                c.misses,
                c.accesses,
                1000.0 * c.misses as f64 / instr.max(1) as f64,
                c.snoop_kills
            );
        }
        eprintln!(
            "CACHESIM w{w} D 64K 2-way 32B: {} loads {} stores {} read_misses {} write_misses",
            self.loads, self.stores, self.read_misses, self.write_misses
        );
        let per1k = |n: u64| 1000.0 * n as f64 / instr.max(1) as f64;
        for l in &self.l2 {
            let (h, m) = (l.hits, l.misses);
            eprintln!(
                "CACHESIM w{w} {:<17} I hit {} miss {} | D hit {} miss {} | WB hit {} miss {} | dirty_evicts {} | misses per1k: I {:.2} D {:.2} WB {:.2} all {:.2}",
                l.name, h[0], m[0], h[1], m[1], h[2], m[2], l.dirty_evicts,
                per1k(m[0]), per1k(m[1]), per1k(m[2]), per1k(m[0] + m[1] + m[2])
            );
        }
        for r in &self.ras {
            eprintln!(
                "CACHESIM w{w} RAS depth {:>2}: calls {} rets {} mispredicts {} per1k {:.2}",
                r.depth, self.calls, self.rets, r.mispredicts, per1k(r.mispredicts)
            );
        }
        for (k, name) in ["DIV", "MOD"].iter().enumerate() {
            eprintln!("CACHESIM w{w} {name}: {} ops, {} by zero", self.div_n[k], self.div_zero[k]);
            for (what, h) in [("steps", &self.div_steps_hist[k]), ("qwidth", &self.div_qw_hist[k])] {
                let mut line = format!("CACHESIM w{w} {name} {what} hist:");
                for (v, &n) in h.iter().enumerate() {
                    if n != 0 {
                        line += &format!(" {v}:{n}");
                    }
                }
                eprintln!("{line}");
            }
        }
        let d = &self.dist;
        eprintln!(
            "CACHESIM w{w} MISSDIST events: D fills {} ({:.2} per1k), I fills (missing the D-cache) {} ({:.2} per1k), D writebacks {} ({:.2} per1k)",
            d.n[0], per1k(d.n[0]), d.n[1], per1k(d.n[1]), d.n[2], per1k(d.n[2])
        );
        for (u, unit) in ["instr", "cycles"].iter().enumerate() {
            for (name, h) in [
                ("D fill -> prev D fill", &d.d_d),
                ("D fill -> prev any event", &d.d_any),
                ("any event -> prev any event", &d.any_any),
                ("writeback -> prev any event (before its own fill)", &d.wb_any),
            ] {
                eprintln!("CACHESIM w{w} MISSDIST {unit} [{name}]: {}", h[u].line());
            }
        }
        eprintln!("CACHESIM w{w} TLB refill vector entries {}", self.tlb_entries);
        eprintln!(
            "CACHESIM w{w} TLB walker refills {} fallbacks {}",
            self.walker_refills, self.walker_fallbacks
        );
        if let Some(p) = &self.profile {
            p.dump(w);
        }
    }

    /// A data access of `n` bytes at physical `pa` (RAM only).
    pub(super) fn data(&mut self, pa: u32, n: u32, store: bool) {
        let line_addr = pa & !(DLINE - 1);
        let line = u64::from(pa >> 5);
        let set = (line % u64::from(self.dcache.sets)) as usize;
        if store {
            self.stores += 1;
        } else {
            self.loads += 1;
        }
        // A store run ends at any access that isn't a store to its line.
        if let Some(slot) = self.run_slot
            && !(store && self.dcache.tags[slot] == (line | VALID))
        {
            if let Some(p) = self.pending[slot].as_mut() {
                p.run = false;
            }
            self.run_slot = None;
        }
        if store {
            for c in &mut self.icd {
                c.snoop(pa);
            }
        }
        for c in &mut self.icaches32 {
            c.d.access(line_addr);
        }
        let (hit, way, evicted) = self.dcache.access(line_addr);
        let slot = set * self.dcache.ways + way;
        if evicted.is_some() || !hit {
            // Whatever was pending in this slot is gone: it needed its fill.
            self.pending[slot] = None;
        }
        if !hit {
            self.dist.event(0, self.now);
            self.evict(slot, evicted);
            for l in &mut self.l2 {
                l.access(line_addr, 1);
            }
            if let Some(p) = self.profile.as_mut() {
                p.map.entry(p.pc).or_default().dmiss += 1;
            }
        }
        if store {
            self.ddirty[slot] = true;
        }
        if hit {
            if let Some(p) = self.pending[slot].as_mut() {
                if store {
                    p.mask |= byte_mask(pa, n);
                    p.seen += 1;
                    if p.mask == u32::MAX {
                        self.full_line += 1;
                        if p.run && p.seen <= DLINE / 8 + 1 {
                            self.full_line_strict += 1;
                        }
                        self.pending[slot] = None;
                    }
                } else {
                    // Read before written whole: the fill was needed.
                    self.pending[slot] = None;
                }
            }
            return;
        }
        if store {
            self.write_misses += 1;
            self.pending[slot] = Some(Pending {
                mask: byte_mask(pa, n),
                seen: 1,
                run: true,
            });
            self.run_slot = Some(slot);
        } else {
            self.read_misses += 1;
        }
    }

    /// DCBZ of the line at physical `pa`: installed (zeroed) without a
    /// fetch, so the stores into it that follow hit.
    pub(super) fn zero_line(&mut self, pa: u32) {
        let line_addr = pa & !(DLINE - 1);
        for c in &mut self.icaches32 {
            c.d.access(line_addr);
        }
        let (a, m) = (self.dcache.accesses, self.dcache.misses);
        let (_, way, evicted) = self.dcache.access(line_addr);
        (self.dcache.accesses, self.dcache.misses) = (a, m);
        let set = (u64::from(pa >> 5) % u64::from(self.dcache.sets)) as usize;
        let slot = set * self.dcache.ways + way;
        self.pending[slot] = None;
        self.evict(slot, evicted);
        self.ddirty[slot] = true;
        self.zero_lines += 1;
    }

    pub(super) fn report(&self) {
        eprintln!("cachesim: I-cache misses (demand, no prefetch)");
        for c in &self.icaches {
            eprintln!(
                "  {:<22} {:>12} misses / {:>12} accesses ({:.3}%)",
                c.name,
                c.misses,
                c.accesses,
                100.0 * c.misses as f64 / c.accesses.max(1) as f64
            );
        }
        for c in &self.icaches32 {
            eprintln!(
                "  {:<22} {:>12} misses / {:>12} accesses ({:.3}%)",
                c.name,
                c.misses,
                c.accesses,
                100.0 * c.misses as f64 / c.accesses.max(1) as f64
            );
        }
        eprintln!(
            "cachesim: {}: {} loads, {} stores, {} read misses, {} write misses",
            self.dcache.name, self.loads, self.stores, self.read_misses, self.write_misses
        );
        eprintln!("cachesim: lines zeroed by DCBZ (no fetch): {}", self.zero_lines);
        eprintln!(
            "cachesim: write misses then written whole before a read or eviction: {} ({:.1}%), as a store run: {} ({:.1}%)",
            self.full_line,
            100.0 * self.full_line as f64 / self.write_misses.max(1) as f64,
            self.full_line_strict,
            100.0 * self.full_line_strict as f64 / self.write_misses.max(1) as f64
        );
    }
}

/// The bytes `[pa, pa + n)` within their 32-byte line, as a bit mask.
fn byte_mask(pa: u32, n: u32) -> u32 {
    let off = pa & (DLINE - 1);
    let n = n.min(DLINE - off);
    if n >= 32 {
        u32::MAX
    } else {
        ((1u32 << n) - 1) << off
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The board's fill rule: an even doubleword fills itself and the next;
    /// an odd one only itself; the NLB never fills.
    #[test]
    fn board_icache_fills_per_doubleword() {
        let mut c = ICacheD::new(16 << 10, 1, 16, false, false);
        assert!(c.access(0x1000)); // dw0 misses, fills dw0+dw1
        assert!(!c.access(0x1008));
        assert!(!c.access(0x1000));
        assert!(c.access(0x1018)); // odd dw of the next line: itself only
        assert!(c.access(0x1010)); // ... so the even one still misses
        assert!(!c.access(0x1018));
        assert!(c.access(0x1000 + (16 << 10))); // conflict in a direct-mapped cache
        assert!(c.access(0x1000));
        c.snoop(0x1000);
        assert!(c.access(0x1000)); // a store dropped the line
        c.flush();
        assert!(c.access(0x1000));
    }

    #[test]
    fn two_way_keeps_both_conflicting_lines() {
        let mut c = ICacheD::new(16 << 10, 2, 16, false, false);
        assert!(c.access(0x1000));
        assert!(c.access(0x1000 + (8 << 10)));
        assert!(!c.access(0x1000));
        assert!(!c.access(0x1000 + (8 << 10)));
    }

    #[test]
    fn ras_is_circular() {
        let mut r = Ras { depth: 2, stack: vec![0; 2], top: 0, count: 0, mispredicts: 0 };
        for ra in [1, 2, 3] {
            r.push(ra);
        }
        r.pop(3);
        r.pop(2);
        assert_eq!(r.mispredicts, 0);
        r.pop(1); // overwritten by 3
        assert_eq!(r.mispredicts, 1);
    }

    #[test]
    fn l2_writeback_allocates_and_evicts_dirty() {
        let mut l = L2::new(128, 2, 32); // 2 sets of 2 ways
        l.access(0x00, 2);
        l.access(0x40, 0);
        l.access(0x80, 1); // evicts the dirty 0x00
        assert_eq!((l.misses, l.hits, l.dirty_evicts), ([1, 1, 1], [0, 0, 0], 1));
    }

    #[test]
    fn miss_distance_buckets() {
        let b = Hist::bucket;
        assert_eq!([b(0), b(1), b(2), b(3), b(4), b(7), b(8), b(15), b(16)], [0, 1, 2, 3, 4, 4, 5, 5, 6]);
        assert_eq!([b(31), b(32), b(63), b(64), b(127), b(128), b(u64::MAX)], [6, 7, 7, 8, 8, 9, 9]);
        let mut m = MissDist::default();
        m.event(0, (10, 20));
        m.event(2, (10, 20)); // its own fill's writeback: nothing before the fill
        m.event(1, (12, 24));
        m.event(0, (20, 40));
        assert_eq!(m.n, [2, 1, 1]);
        assert_eq!(m.d_d[0].0[5], 1); // 20 - 10
        assert_eq!(m.d_any[0].0[5], 1); // 20 - 12 = 8
        assert_eq!(m.wb_any[0].total(), 0);
        assert_eq!(m.any_any[0].total(), 2);
    }

    #[test]
    fn micro_itlb_is_lru() {
        let mut u = MicroItlb { entries: 2, on_write: true, keys: Vec::new(), misses: 0 };
        for k in [1u16, 2, 1, 2, 3, 1] {
            u.lookup((k, false));
        }
        assert_eq!(u.misses, 4); // 1, 2 miss; 3 evicts 1; 1 misses
    }
}
