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
        std::env::var_os("KLAUSSCC_CACHESIM")?;
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

    /// An instruction fetch of `len` words at physical `pa`.
    pub(super) fn fetch(&mut self, pa: u32, len: u32) {
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
        for c in &mut self.icaches32 {
            c.d.access(line_addr);
        }
        let (hit, way, evicted) = self.dcache.access(line_addr);
        let slot = set * self.dcache.ways + way;
        if evicted.is_some() || !hit {
            // Whatever was pending in this slot is gone: it needed its fill.
            self.pending[slot] = None;
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
        let (_, way, _) = self.dcache.access(line_addr);
        (self.dcache.accesses, self.dcache.misses) = (a, m);
        let set = (u64::from(pa >> 5) % u64::from(self.dcache.sets)) as usize;
        self.pending[set * self.dcache.ways + way] = None;
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
