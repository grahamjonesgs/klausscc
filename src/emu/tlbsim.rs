//! TLB what-if simulator (`KLAUSSCC_TLBSIM=1`).
//!
//! Not a model of the board's timing: it counts the refills (misses) that
//! alternative TLB designs would take on the same stream of translated
//! accesses, so a hardware choice (bigger TLB, set-associative, a
//! page-table walker) can be judged by relative counts.  Every access that
//! goes through the TLB (useg and kmap with `STATUS.TE` = 1) is fed to all
//! configurations side by side; kseg and MMIO never reach it.
//!
//! Kmap entries are global (G); everything else is keyed by (VPN, ASID).
//!
//! Mirroring the kernel's maintenance:
//! * `TLBINV` empties every simulated TLB.
//! * `TLBP` is how the kernel flushes a page (`flush_one`: probe, then write
//!   an invalid entry); every simulated TLB drops the (VPN, ASID) and the
//!   global entry of the probed VPN, whether the real probe hit or not (a
//!   bigger TLB may hold what the real one already evicted).
//! * A new ASID for an mm needs nothing: its old entries are just never
//!   matched again, and occupy the TLB until replaced, as in hardware.
//! * A `TLBWI` with V = 1 is a refill (the software handler's, counted as
//!   `real_refills`, the kernel's `tlb_refills`).
//!
//! The fully associative round-robin configuration of the emulated TLB's
//! size (FA32 RR by default, FA16 RR with `--tlb-entries 16`) is the model
//! of the real TLB under the kernel's round-robin refill: every access is
//! checked against it (`div_real_*`), which must stay 0.
//!
//! A miss in the real TLB is an exception; when the handler finds no PTE it
//! does not install anything and the access faults, to be retried after the
//! page fault is served.  So a simulated miss that coincides with a real
//! miss is held "pending" and counted and installed only when the real
//! refill (`TLBWI` V = 1 for that key) happens; a simulated miss on an
//! access the real TLB served is counted and installed at once.
//!
//! Windowing: a write to the performance counters' `PERF_CTRL` clear zeros
//! the counts (the TLB contents are kept, as the real TLB is not cleared),
//! and a read of `CNT_ZERO_LINES` (the last hardware counter `perf_show`
//! reads) or the end of the run prints the window on stderr.

use std::collections::{HashMap, HashSet};

use super::mmu::Kind;

const VALID: u32 = 1 << 31;
const KMAP: u32 = 0xC000_0000;

/// A (VPN, ASID) key, or (VPN, global): `vpn << 9 | G << 8 | asid`.
fn make_key(va: u32, asid: u8) -> u32 {
    let vpn = va >> 16;
    if va >= KMAP { vpn << 9 | 1 << 8 } else { vpn << 9 | u32::from(asid) }
}

fn key_from_hi(hi: u32) -> u32 {
    let vpn = hi >> 16;
    if hi & 0x100 != 0 { vpn << 9 | 1 << 8 } else { vpn << 9 | (hi & 0xFF) }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Policy {
    RoundRobin,
    Lru,
    Random,
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

#[derive(Default, Clone)]
struct Stats {
    miss: [u64; 3],
    kmap_miss: u64,
    remiss_16: u64,
    remiss_size: u64,
    compulsory: u64,
}

struct Tlb {
    name: String,
    size: usize,
    sets: usize,
    ways: usize,
    policy: Policy,
    xor_asid: bool,
    /// `key | VALID` per slot, set-major.
    slots: Vec<u32>,
    stamps: Vec<u64>,
    clock: u64,
    rr: u64,
    rng: Rng,
    installs: u64,
    /// Key -> install count when it was evicted by replacement.
    evicted: HashMap<u32, u64>,
    pending: Option<(u32, usize, bool)>,
    stats: Stats,
}

fn kind_index(kind: Kind) -> usize {
    match kind {
        Kind::Fetch => 0,
        Kind::Load => 1,
        Kind::Store => 2,
    }
}

impl Tlb {
    fn new(name: String, size: usize, ways: usize, policy: Policy, xor_asid: bool) -> Self {
        Self {
            name,
            size,
            sets: size / ways,
            ways,
            policy,
            xor_asid,
            slots: vec![0; size],
            stamps: vec![0; size],
            clock: 0,
            rr: 0,
            rng: Rng(0x2545_F491_4F6C_DD1D),
            installs: 0,
            evicted: HashMap::new(),
            pending: None,
            stats: Stats::default(),
        }
    }

    fn set_of(&self, key: u32) -> usize {
        if self.sets == 1 {
            return 0;
        }
        let vpn = key >> 9;
        let asid = if self.xor_asid && key & 0x100 == 0 { key & 0xFF } else { 0 };
        ((vpn ^ asid) as usize) & (self.sets - 1)
    }

    fn lookup(&mut self, key: u32) -> bool {
        let base = self.set_of(key) * self.ways;
        for w in 0..self.ways {
            if self.slots[base + w] == key | VALID {
                self.clock += 1;
                self.stamps[base + w] = self.clock;
                return true;
            }
        }
        false
    }

    fn install(&mut self, key: u32) {
        let base = self.set_of(key) * self.ways;
        let way = match self.policy {
            Policy::RoundRobin => {
                self.rr += 1;
                self.rr as usize % self.ways
            }
            Policy::Lru => (0..self.ways).min_by_key(|&w| {
                let s = self.slots[base + w];
                (s & VALID != 0, self.stamps[base + w])
            })
            .unwrap_or(0),
            Policy::Random => (self.rng.next() >> 16) as usize % self.ways,
        };
        let old = self.slots[base + way];
        self.installs += 1;
        if old & VALID != 0 {
            self.evicted.insert(old & !VALID, self.installs);
        }
        self.slots[base + way] = key | VALID;
        self.clock += 1;
        self.stamps[base + way] = self.clock;
        self.evicted.remove(&key);
    }

    /// Count the miss on `key` and install it.
    fn miss(&mut self, key: u32, kind: usize, compulsory: bool) {
        let s = &mut self.stats;
        s.miss[kind] += 1;
        if key & 0x100 != 0 {
            s.kmap_miss += 1;
        }
        if compulsory {
            s.compulsory += 1;
        }
        if let Some(&at) = self.evicted.get(&key) {
            let age = self.installs - at;
            if age <= 16 {
                s.remiss_16 += 1;
            }
            if age <= self.size as u64 {
                s.remiss_size += 1;
            }
        }
        self.install(key);
    }

    fn drop_key(&mut self, key: u32) {
        let base = self.set_of(key) * self.ways;
        for w in 0..self.ways {
            if self.slots[base + w] == key | VALID {
                self.slots[base + w] = 0;
            }
        }
        self.evicted.remove(&key);
    }

    fn flush_all(&mut self) {
        self.slots.iter_mut().for_each(|s| *s = 0);
        self.evicted.clear();
        self.pending = None;
    }
}

/// A 1-entry micro-ITLB in front of the main TLB.
struct MicroItlb {
    name: &'static str,
    /// Invalidated by every TLB write (else only by `TLBINV` and a flush of
    /// its own page).
    inval_on_write: bool,
    key: Option<u32>,
    misses: u64,
}

pub(super) struct TlbSim {
    tlbs: Vec<Tlb>,
    itlbs: Vec<MicroItlb>,
    seen: HashSet<u32>,
    lookups: [u64; 3],
    fetches: u64,
    last_fetch_instret: u64,
    real_refills: u64,
    real_flush_probes: u64,
    real_invalidates: u64,
    /// The index in `tlbs` of the model of the real TLB: FA`n` RR for the
    /// emulated TLB's `n` entries ([`TlbSim::set_real_entries`]).
    real: usize,
    /// Accesses the real TLB served but its model missed.
    div_real_hit: u64,
    /// Accesses the real TLB missed but its model hit.
    div_real_miss: u64,
    /// Real misses that did not end in a refill (the page-fault path).
    nonrefill_misses: u64,
    reported: bool,
    /// A window has been printed (the run-end report is then redundant).
    ever_reported: bool,
}

impl TlbSim {
    pub(super) fn from_env() -> Option<Box<Self>> {
        std::env::var_os("KLAUSSCC_TLBSIM")?;
        Some(Self::build())
    }

    #[cfg(test)]
    fn from_env_forced() -> Box<Self> {
        Self::build()
    }

    fn build() -> Box<Self> {
        let mut tlbs = Vec::new();
        for &size in &[16usize, 32, 64] {
            for (p, n) in [(Policy::RoundRobin, "RR"), (Policy::Lru, "LRU"), (Policy::Random, "RAND")] {
                tlbs.push(Tlb::new(format!("FA{size} {n}"), size, size, p, false));
            }
        }
        for &size in &[64usize, 128] {
            for &ways in &[2usize, 4] {
                for xor in [false, true] {
                    for (p, n) in [(Policy::Lru, "LRU"), (Policy::Random, "RAND")] {
                        let idx = if xor { "vpn^asid" } else { "vpn" };
                        tlbs.push(Tlb::new(format!("SA{size} {ways}w {idx} {n}"), size, ways, p, xor));
                    }
                }
            }
        }
        let itlbs = vec![
            MicroItlb { name: "uITLB (cleared by every TLB write)", inval_on_write: true, key: None, misses: 0 },
            MicroItlb { name: "uITLB (cleared by flush only)", inval_on_write: false, key: None, misses: 0 },
        ];
        let mut sim = Box::new(Self {
            tlbs,
            itlbs,
            seen: HashSet::new(),
            lookups: [0; 3],
            fetches: 0,
            last_fetch_instret: u64::MAX,
            real_refills: 0,
            real_flush_probes: 0,
            real_invalidates: 0,
            real: 0,
            div_real_hit: 0,
            div_real_miss: 0,
            nonrefill_misses: 0,
            reported: true,
            ever_reported: false,
        });
        sim.set_real_entries(super::mmu::TLB_ENTRIES_DEFAULT);
        sim
    }

    /// The emulated TLB has `n` entries: FA`n` RR becomes the model of it.
    pub(super) fn set_real_entries(&mut self, n: u32) {
        let name = format!("FA{n} RR");
        self.real = self.tlbs.iter().position(|t| t.name == name).unwrap_or(0);
    }

    /// One translation through the TLB.  `real_miss`: the real TLB has no
    /// entry (the access raises `TLB_MISS_*`).
    pub(super) fn access(&mut self, va: u32, asid: u8, kind: Kind, real_miss: bool, instret: u64) {
        self.reported = false;
        let k = kind_index(kind);
        let key = make_key(va, asid);
        self.lookups[k] += 1;
        let compulsory = !self.seen.contains(&key);
        let mut any_miss = false;
        let real = self.real;
        for (i, t) in self.tlbs.iter_mut().enumerate() {
            let hit = t.lookup(key);
            if i == real {
                // The model of the real TLB.
                if real_miss && hit {
                    self.div_real_miss += 1;
                } else if !real_miss && !hit {
                    self.div_real_hit += 1;
                }
            }
            if hit {
                continue;
            }
            any_miss = true;
            if real_miss {
                if i == real && t.pending.is_some() {
                    self.nonrefill_misses += 1;
                }
                t.pending = Some((key, k, compulsory));
            } else {
                t.miss(key, k, compulsory);
            }
        }
        if !real_miss || !any_miss {
            self.seen.insert(key);
        }
        if k == 0 && instret != self.last_fetch_instret {
            self.last_fetch_instret = instret;
            self.fetches += 1;
            for u in &mut self.itlbs {
                if u.key != Some(key) {
                    u.misses += 1;
                    u.key = if real_miss { None } else { Some(key) };
                }
            }
        }
    }

    /// `TLBWI`: a write with V = 1 is a refill.
    pub(super) fn tlbwi(&mut self, hi: u32, lo: u32) {
        for u in &mut self.itlbs {
            if u.inval_on_write {
                u.key = None;
            }
        }
        if lo & 1 == 0 {
            return;
        }
        self.real_refills += 1;
        let key = key_from_hi(hi);
        let mut done = false;
        for (i, t) in self.tlbs.iter_mut().enumerate() {
            let pending = t.pending.take();
            if i == 0 && pending.is_some_and(|p| p.0 != key) {
                self.nonrefill_misses += 1;
            }
            if let Some((pk, kind, comp)) = pending
                && pk == key
            {
                t.miss(key, kind, comp);
                done = true;
            }
        }
        if done {
            self.seen.insert(key);
        }
    }

    /// `TLBP` (the kernel's page flush) for `hi` = VPN and ASID.
    pub(super) fn tlbp(&mut self, hi: u32) {
        self.real_flush_probes += 1;
        let vpn = hi >> 16;
        let keys = [vpn << 9 | (hi & 0xFF), vpn << 9 | 1 << 8];
        for &key in &keys {
            for t in &mut self.tlbs {
                t.drop_key(key);
            }
            self.seen.remove(&key);
            for u in &mut self.itlbs {
                if u.key == Some(key) {
                    u.key = None;
                }
            }
        }
    }

    pub(super) fn tlbinv(&mut self) {
        self.real_invalidates += 1;
        for t in &mut self.tlbs {
            t.flush_all();
        }
        self.seen.clear();
        for u in &mut self.itlbs {
            u.key = None;
        }
    }

    /// Zero the counts (`PERF_CTRL` clear); the TLB contents stay.
    pub(super) fn clear(&mut self) {
        for t in &mut self.tlbs {
            t.stats = Stats::default();
        }
        for u in &mut self.itlbs {
            u.misses = 0;
        }
        self.lookups = [0; 3];
        self.fetches = 0;
        self.real_refills = 0;
        self.real_flush_probes = 0;
        self.real_invalidates = 0;
        self.div_real_hit = 0;
        self.div_real_miss = 0;
        self.nonrefill_misses = 0;
        self.reported = true;
    }

    /// Print the window (once per window).  At the end of the run
    /// (`at_end`) only when no window was printed before.
    pub(super) fn report(&mut self, instructions: u64, at_end: bool) {
        if self.reported || (at_end && self.ever_reported) {
            return;
        }
        self.reported = true;
        self.ever_reported = true;
        eprintln!(
            "TLBSIM window instructions={} lookups_fetch={} lookups_load={} lookups_store={} fetches={} real_refills={} flush_probes={} tlbinv={} div_real_hit_sim_miss={} div_real_miss_sim_hit={} nonrefill_misses={}",
            instructions, self.lookups[0], self.lookups[1], self.lookups[2], self.fetches, self.real_refills,
            self.real_flush_probes, self.real_invalidates, self.div_real_hit, self.div_real_miss, self.nonrefill_misses
        );
        for t in &self.tlbs {
            let s = &t.stats;
            eprintln!(
                "TLBSIM cfg=\"{}\" misses={} fetch={} load={} store={} kmap={} remiss16={} remiss_size={} compulsory={}",
                t.name,
                s.miss.iter().sum::<u64>(),
                s.miss[0],
                s.miss[1],
                s.miss[2],
                s.kmap_miss,
                s.remiss_16,
                s.remiss_size,
                s.compulsory
            );
        }
        for u in &self.itlbs {
            eprintln!("TLBSIM uitlb=\"{}\" misses={} fetches={}", u.name, u.misses, self.fetches);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Access `pages` pages round and round (a pointer chase), real TLB
    /// serving everything (`real_miss` false).
    fn chase(sim: &mut TlbSim, pages: u32, rounds: u32) {
        for _ in 0..rounds {
            for p in 0..pages {
                sim.access(p << 16, 1, Kind::Load, false, 0);
            }
        }
    }

    fn misses(sim: &TlbSim, name: &str) -> u64 {
        sim.tlbs.iter().find(|t| t.name == name).map_or(0, |t| t.stats.miss.iter().sum())
    }

    #[test]
    fn thrash_and_capacity() {
        let mut sim = TlbSim::from_env_forced();
        chase(&mut sim, 17, 10);
        // 17 pages round robin in 16 entries: every access misses; LRU too.
        assert_eq!(misses(&sim, "FA16 RR"), 170);
        assert_eq!(misses(&sim, "FA16 LRU"), 170);
        // 32 entries hold them: compulsory only.
        assert_eq!(misses(&sim, "FA32 RR"), 17);
        assert_eq!(misses(&sim, "SA128 4w vpn LRU"), 17);
    }

    #[test]
    fn flush_costs_a_miss_everywhere() {
        let mut sim = TlbSim::from_env_forced();
        chase(&mut sim, 4, 1);
        sim.tlbp(2 << 16 | 1);
        chase(&mut sim, 4, 1);
        assert_eq!(misses(&sim, "FA64 LRU"), 5);
        sim.tlbinv();
        chase(&mut sim, 4, 1);
        assert_eq!(misses(&sim, "FA64 LRU"), 9);
    }

    #[test]
    fn real_miss_without_refill_installs_nothing() {
        let mut sim = TlbSim::from_env_forced();
        // A miss that page-faults: no refill; the retry misses again and is refilled.
        sim.access(5 << 16, 1, Kind::Load, true, 0);
        assert_eq!(misses(&sim, "FA16 RR"), 0);
        sim.access(5 << 16, 1, Kind::Load, true, 1);
        sim.tlbwi(5 << 16 | 1, 1);
        assert_eq!(misses(&sim, "FA16 RR"), 1);
        assert_eq!(sim.nonrefill_misses, 1);
        sim.access(5 << 16, 1, Kind::Load, false, 2);
        assert_eq!(misses(&sim, "FA16 RR"), 1);
    }
}
