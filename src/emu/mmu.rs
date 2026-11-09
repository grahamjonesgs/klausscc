//! MMU stages 2 and 3 (`klaussCPU/MMU_PLAN.md` §10-§11): the MPU, the TLB
//! and the address map, and [`Cpu::translate`], the one function every
//! memory access an instruction makes goes through.
//!
//! An instruction asks for its accesses before it changes anything:
//! [`Cpu::map_data`] for each data doubleword (both, for a two-access
//! instruction) and [`Cpu::fetch`] for its words.  What they return, a
//! [`Mapped`] access or the instruction words, is the only way to reach
//! memory: the memory helpers in `mod.rs` take a [`Mapped`], which only
//! this module builds.  So no access bypasses the stage 1 user-mode MMIO
//! rule, the MPU or the TLB, and a fault is known before the first side
//! effect (`MMU_PLAN.md` §4.2).  The exceptions are the ones the spec makes:
//! the frame push of an interrupt or exception entry ([`Cpu::frame_slot`],
//! never checked; with `TE` = 1 a kseg address or a double fault) and the
//! debugger's accesses ([`Cpu::debug_translate`]).
//!
//! With `STATUS.TE` = 0 every address is physical, and with `MPU_EN` = 1
//! the MPU decides (§10.2).  With `TE` = 1 the MPU is ignored and the
//! address map of §11.2 applies: useg and kmap through the TLB, kseg direct
//! (PA = VA - `0x8000_0000`), MMIO identity, and the rest a fault.  The
//! emulator has no caches, fetch window, NLB or BTIC, so what the spec says
//! about discarding the fetch side's virtual copies on a TLB change has
//! nothing to act on here.

use super::Cpu;
use super::StopReason;
use super::system::Cause;
use crate::layout::is_mmio;

/// The number of MPU regions (`CPU_FEATURES[7:4]` + 1, §10.1).
pub(super) const MPU_REGIONS: usize = 16;
/// The number of TLB entries (`CPU_FEATURES[15:8]`, §11.3): 16 on the
/// Nexys A7 build (klaussCPU mmu daee899, parameter `TLB_N`).  A power of
/// two; `TLB_INDEX` keeps only the bits that address an entry.
pub(super) const TLB_ENTRIES: usize = 16;

/// `MPU_BASEn[0]`: the region is enabled.
const MPU_ENABLE: u32 = 1 << 0;
/// `MPU_BASEn[4]`: a GUARD (deny) region; its R/W/X bits are ignored.
const MPU_GUARD: u32 = 1 << 4;
/// `MPU_BASEn` / `MPU_LIMITn` `[31:5]`: a 32-byte granule.
const GRANULE: u32 = !0x1F;

/// `TLB_LO[0]` (and an entry's V): the entry is valid.
const TLB_V: u32 = 1 << 0;
/// `TLB_HI[8]` (and an entry's G): the entry matches every ASID.
const TLB_G: u32 = 1 << 8;
/// `TLB_INDEX[31]` P: the last TLBP found no match.
const TLB_INDEX_P: u32 = 1 << 31;
/// The bits `TLB_HI` has: VPN `[31:16]`, G `[8]`, ASID `[7:0]`.
pub(super) const TLB_HI_BITS: u32 = 0xFFFF_01FF;
/// The bits `TLB_LO` has: PFN `[26:16]`, X `[3]`, W `[2]`, R `[1]`, V `[0]`.
pub(super) const TLB_LO_BITS: u32 = 0x07FF_000F;
/// The entry-index bits of `TLB_INDEX` (`[3:0]` with 16 entries; the
/// higher index bits read 0 and are ignored on write).
const TLB_INDEX_MASK: u32 = TLB_ENTRIES as u32 - 1;
/// The bits `TLB_INDEX` has: P `[31]` and the entry index.
pub(super) const TLB_INDEX_BITS: u32 = TLB_INDEX_P | TLB_INDEX_MASK;
/// The page size is 64 KiB (§11.1): VPN = VA`[31:16]`.
const PAGE_SHIFT: u32 = 16;
/// The offset within a page.
const PAGE_OFFSET: u32 = (1 << PAGE_SHIFT) - 1;

/// kseg (§11.2): `0x8000_0000-0x87FF_FFFF`, direct, PA = VA - `KSEG`.
const KSEG: u32 = 0x8000_0000;
/// The first address past kseg (all 128 MiB of DDR are mapped).
const KSEG_END: u32 = 0x8800_0000;
/// kmap (§11.2): `0xC000_0000-0xEFFF_FFFF`, TLB, supervisor only.
const KMAP: u32 = 0xC000_0000;
/// MMIO (§11.2): `0xF000_0000` up, identity, supervisor only.
const MMIO: u32 = 0xF000_0000;

/// What an access is for: the permission it needs and the cause it raises.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    /// A load (stack pops, LR and IRET included); needs R.
    Load,
    /// A store (stack pushes, CALL and SC included); needs W.
    Store,
    /// An instruction fetch; needs X.
    Fetch,
}

impl Kind {
    /// The permission bit: R `[1]`, W `[2]` or X `[3]`, in `MPU_BASEn` and
    /// `TLB_LO` alike.
    const fn permission(self) -> u32 {
        match self {
            Self::Load => 1 << 1,
            Self::Store => 1 << 2,
            Self::Fetch => 1 << 3,
        }
    }

    /// `ACCESS_LOAD`, `ACCESS_STORE` or `ACCESS_FETCH`.
    const fn access(self) -> Cause {
        match self {
            Self::Load => Cause::AccessLoad,
            Self::Store => Cause::AccessStore,
            Self::Fetch => Cause::AccessFetch,
        }
    }

    /// `TLB_MISS_LOAD`, `TLB_MISS_STORE` or `TLB_MISS_FETCH`.
    const fn tlb_miss(self) -> Cause {
        match self {
            Self::Load => Cause::TlbMissLoad,
            Self::Store => Cause::TlbMissStore,
            Self::Fetch => Cause::TlbMissFetch,
        }
    }
}

/// Why an access did not translate: the exception it raises.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct XlateFault {
    /// `ACCESS_*` or `TLB_MISS_*`.
    pub(super) cause: Cause,
    /// `EADDR`: the address as computed, or the start of the doubleword
    /// (the instruction word, for a fetch) that faulted.
    pub(super) eaddr: u32,
    /// `ECAUSE[8]`: an MPU GUARD region denied it.
    pub(super) guard: bool,
    /// A TLB miss or a TLB permission fault, which load `TLB_HI` (§11.3).
    pub(super) tlb: bool,
}

impl XlateFault {
    /// The plain `ACCESS_*` fault of a `kind` access at `va`.
    const fn access(kind: Kind, va: u32) -> Self {
        Self {
            cause: kind.access(),
            eaddr: va,
            guard: false,
            tlb: false,
        }
    }
}

/// A data access that has been checked and translated: the virtual address
/// as computed, the physical address of its first byte and, for an access
/// that runs into the next doubleword (a spanning `MEMGET32`), that
/// doubleword's physical start.  Only this module builds one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Mapped {
    /// The virtual address (what the trace sees; watchpoints see `pa`).
    va: u32,
    /// The physical address of the byte at `va`.
    pa: u32,
    /// The physical address of the next doubleword, `(va & !7) + 8`.
    pa_next: u32,
}

impl Mapped {
    /// An untranslated access: PA = VA.
    const fn identity(va: u32) -> Self {
        Self {
            va,
            pa: va,
            pa_next: (va & !7).wrapping_add(8),
        }
    }

    /// The virtual address.
    pub(super) const fn va(self) -> u32 {
        self.va
    }

    /// The physical address of the first byte.
    pub(super) const fn pa(self) -> u32 {
        self.pa
    }

    /// The physical address of byte `va + i`, for `i` < 8.
    pub(super) const fn pa_at(self, i: u32) -> u32 {
        let lane = (self.va & 7) + i;
        if lane < 8 {
            self.pa.wrapping_add(i)
        } else {
            self.pa_next.wrapping_add(lane - 8)
        }
    }

    /// The same access at `va & mask` (a 64-bit A = 1 access aligned down),
    /// which stays within the doubleword and so within the page.
    pub(super) const fn masked(self, mask: u32) -> Self {
        Self {
            va: self.va & mask,
            pa: self.pa & mask,
            pa_next: self.pa_next,
        }
    }
}

/// An instruction's fetch: its words, or why it cannot run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Fetch {
    /// Word 0 and the immediate words (0 past the instruction's length).
    Words([u32; 3]),
    /// A fetch fault, with word 0 when it was fetched (else 0), for a park.
    Fault(XlateFault, u32),
    /// Word 0's physical address is outside RAM.
    OutOfRange,
}

/// One MPU region as its two system registers hold it (§10.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MpuRegion {
    /// `MPU_BASEn`: `[31:5]` the first granule, `[4]` GUARD, `[3]` X,
    /// `[2]` W, `[1]` R, `[0]` EN.
    pub base: u32,
    /// `MPU_LIMITn`: `[31:5]` the last granule (inclusive).
    pub limit: u32,
}

impl MpuRegion {
    /// EN.
    #[must_use]
    pub const fn enabled(self) -> bool {
        self.base & MPU_ENABLE != 0
    }

    /// GUARD.
    #[must_use]
    pub const fn guard(self) -> bool {
        self.base & MPU_GUARD != 0
    }

    /// The first byte the region covers.
    #[must_use]
    pub const fn first(self) -> u32 {
        self.base & GRANULE
    }

    /// The last byte the region covers (the end of its last granule).
    #[must_use]
    pub const fn last(self) -> u32 {
        self.limit | !GRANULE
    }

    /// The R/W/X bits as `"rwx"`, a `-` for each one clear.
    #[must_use]
    pub fn permissions(self) -> String {
        permissions(self.base)
    }

    /// Whether the region covers byte `a`: enabled, and `a`'s granule is
    /// between the base's and the limit's, both inclusive.
    const fn covers(self, a: u32) -> bool {
        self.enabled() && self.base & GRANULE <= a & GRANULE && a & GRANULE <= self.limit & GRANULE
    }
}

/// One TLB entry, in the `TLB_HI` / `TLB_LO` layout TLBWI and TLBR move
/// (§11.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TlbEntry {
    /// VPN `[31:16]`, G `[8]`, ASID `[7:0]`.
    pub hi: u32,
    /// PFN `[26:16]`, X `[3]`, W `[2]`, R `[1]`, V `[0]`.
    pub lo: u32,
}

impl TlbEntry {
    /// V.
    #[must_use]
    pub const fn valid(self) -> bool {
        self.lo & TLB_V != 0
    }

    /// G: matches every ASID.
    #[must_use]
    pub const fn global(self) -> bool {
        self.hi & TLB_G != 0
    }

    /// The virtual page number, VA`[31:16]`.
    #[must_use]
    pub const fn vpn(self) -> u32 {
        self.hi >> PAGE_SHIFT
    }

    /// The ASID.
    #[must_use]
    pub const fn asid(self) -> u8 {
        self.hi as u8
    }

    /// The physical frame number, PA`[26:16]`.
    #[must_use]
    pub const fn pfn(self) -> u32 {
        (self.lo >> PAGE_SHIFT) & 0x7FF
    }

    /// The R/W/X bits as `"rwx"`, a `-` for each one clear.
    #[must_use]
    pub fn permissions(self) -> String {
        permissions(self.lo)
    }

    /// Whether the entry maps virtual page `vpn` in address space `asid`
    /// (§11.3): valid, the VPN equal, and global or the ASID equal.
    const fn matches(self, vpn: u32, asid: u8) -> bool {
        self.valid() && self.vpn() == vpn && (self.global() || self.asid() == asid)
    }
}

/// R `[1]`, W `[2]`, X `[3]` of `bits` as `"rwx"`.
fn permissions(bits: u32) -> String {
    [(1 << 1, 'r'), (1 << 2, 'w'), (1 << 3, 'x')]
        .iter()
        .map(|&(bit, c)| if bits & bit != 0 { c } else { '-' })
        .collect()
}

impl Cpu {
    /// Translate a `kind` access to `va` by the current mode, or say which
    /// exception it raises with `EADDR` = `va`.  This is the one place the
    /// emulator decides whether an instruction may touch an address.
    ///
    /// With `TE` = 0 (PA = VA):
    /// 1. user mode, MMIO (`0xF000_0000` up): `ACCESS_*` (stage 1, §3.6);
    /// 2. with `MPU_EN`, the MPU rule of §10.2 ([`Cpu::mpu_allows`]).
    ///
    /// With `TE` = 1 (§11.2), in priority order (§11.3): a user access at
    /// or above `0x8000_0000`, or any access to `0x8800_0000-0xBFFF_FFFF`,
    /// is `ACCESS_*`; kseg is PA = VA - `0x8000_0000` and MMIO is identity;
    /// useg and kmap go through the TLB, where no matching entry is
    /// `TLB_MISS_*` and a matching one without the permission `ACCESS_*`.
    // Spec (confirmed in MMU_PLAN 424e236): a user MMIO access that a user GUARD region also covers is
    // the stage 1 MMIO fault (§10.2: that rule "still applies first"), so
    // ECAUSE[8] GUARD is clear for it.
    // Spec (confirmed in MMU_PLAN 424e236): §11.3's per-entry R/W/X is checked in supervisor mode too
    // (the spec names no mode exemption), so a kernel access to a W = 0 page
    // faults as a user one does.
    pub(super) fn translate(&self, va: u32, kind: Kind) -> Result<u32, XlateFault> {
        if !self.te {
            if self.um && is_mmio(va) {
                return Err(XlateFault::access(kind, va));
            }
            if self.mpu_en
                && let Err(guard) = self.mpu_allows(va, kind)
            {
                return Err(XlateFault {
                    guard,
                    ..XlateFault::access(kind, va)
                });
            }
            return Ok(va);
        }
        if self.um && va >= KSEG {
            return Err(XlateFault::access(kind, va));
        }
        match va {
            KSEG..KSEG_END => Ok(va - KSEG),
            MMIO.. => Ok(va),
            KSEG_END..KMAP => Err(XlateFault::access(kind, va)),
            _ => self.tlb_translate(va, kind),
        }
    }

    /// The MPU rule (§10.2) for a `kind` access to byte `a` by the current
    /// mode: denied when an enabled GUARD region covers it (`Err(true)`, in
    /// either mode); otherwise allowed in supervisor mode, and in user mode
    /// only when an enabled non-GUARD region covering it has the permission
    /// (`Err(false)` when none does).
    fn mpu_allows(&self, a: u32, kind: Kind) -> Result<(), bool> {
        let mut covering = self.mpu.iter().filter(|r| r.covers(a));
        if covering.clone().any(|r| r.guard()) {
            return Err(true);
        }
        if !self.um || covering.any(|r| r.base & kind.permission() != 0) {
            Ok(())
        } else {
            Err(false)
        }
    }

    /// The TLB entry that maps `va` in the current address space (the
    /// `ASID` SR), if any.  With more than one match (a kernel bug, §11.3)
    /// the lowest-numbered entry wins.
    fn tlb_lookup(&self, va: u32, asid: u8) -> Option<TlbEntry> {
        let vpn = va >> PAGE_SHIFT;
        self.tlb.iter().copied().find(|e| e.matches(vpn, asid))
    }

    /// A useg or kmap access through the TLB.
    fn tlb_translate(&self, va: u32, kind: Kind) -> Result<u32, XlateFault> {
        let fault = |cause| XlateFault {
            cause,
            eaddr: va,
            guard: false,
            tlb: true,
        };
        let entry = self.tlb_lookup(va, self.asid).ok_or_else(|| fault(kind.tlb_miss()))?;
        if entry.lo & kind.permission() == 0 {
            return Err(fault(kind.access()));
        }
        Ok((entry.pfn() << PAGE_SHIFT) | (va & PAGE_OFFSET))
    }

    /// Check and translate a data access of `len` bytes at the effective
    /// address `ea` (as computed, before any A = 1 alignment, so it is the
    /// `EADDR`).  An access that runs into the next doubleword (only a
    /// `MEMGET32` can: a 64-bit RAM access reaches just the aligned
    /// doubleword, so its callers pass `len` 1) translates that doubleword
    /// too, before either is touched; when only the second faults, `EADDR`
    /// is its start, the lowest faulting byte (§4.1, §11.3).
    // Spec (confirmed in MMU_PLAN 424e236): when both doublewords fault, the first (lower) one's fault
    // is reported, whatever its cause, even if the second's ranks higher in
    // the §11.3 order (e.g. a TLB miss on the first and an address-map
    // ACCESS on the second): "report the lowest faulting byte".
    pub(super) fn map_data(&self, ea: u32, len: u32, store: bool) -> Result<Mapped, XlateFault> {
        let kind = if store { Kind::Store } else { Kind::Load };
        let pa = self.translate(ea, kind)?;
        let next = (ea & !7).wrapping_add(8);
        let last = ea.wrapping_add(len - 1);
        let pa_next = if last & !7 == ea & !7 {
            (pa & !7).wrapping_add(8)
        } else {
            self.translate(next, kind)?
        };
        Ok(Mapped { va: ea, pa, pa_next })
    }

    /// Fetch the instruction at `pc`: every word it occupies is checked
    /// and translated (the length is in word 0), and the first one that
    /// faults is the `ACCESS_FETCH` / `TLB_MISS_FETCH` with `EADDR` = its
    /// address (§4.1).  In user mode a PC with `[1:0]` != 0 is
    /// `MISALIGNED_FETCH` first; supervisor mode ignores those bits, as
    /// before stage 1.  Words past the end of RAM read 0, and word 0 there
    /// is [`Fetch::OutOfRange`], except that with `MPU_EN` or `TE` an MMIO
    /// word 0 fetches as 0 (ILLEGAL).
    pub(super) fn fetch(&self, pc: u32) -> Fetch {
        if self.um && pc & 3 != 0 {
            let word = self.translate(pc, Kind::Fetch).map_or(0, |pa| self.read32(pa));
            return Fetch::Fault(
                XlateFault {
                    cause: Cause::MisalignedFetch,
                    eaddr: pc,
                    guard: false,
                    tlb: false,
                },
                word,
            );
        }
        let pa = match self.translate(pc, Kind::Fetch) {
            Ok(pa) => pa,
            Err(fault) => return Fetch::Fault(fault, 0),
        };
        // With MPU_EN or TE set, a fetch fill from MMIO is a fake fill of
        // zeros (§10.2, klaussCPU mmu 64bacdf), so no stray fetch reads a
        // device: the zero word is LEN 00, ILLEGAL.  With both off it is
        // outside RAM, as before stage 1.
        if (self.mpu_en || self.te) && is_mmio(pa) {
            return Fetch::Words([0; 3]);
        }
        if (pa as usize) + 4 > self.mem.len() {
            return Fetch::OutOfRange;
        }
        let mut words = [self.read32(pa), 0, 0];
        // LEN 00 (illegal) is one word.
        let len = (words[0] >> 30).max(1) as usize;
        for (k, slot) in words.iter_mut().enumerate().take(len).skip(1) {
            match self.translate(pc.wrapping_add(4 * k as u32), Kind::Fetch) {
                Ok(pa) => *slot = self.read32(pa),
                Err(fault) => return Fetch::Fault(fault, words[0]),
            }
        }
        Fetch::Words(words)
    }

    /// Where an interrupt or exception entry pushes its frame for the
    /// kernel SP `sp`: SP - 8, never checked (§10.2) or translated (§11.3).
    /// With `TE` = 0 that is the physical address.  With `TE` = 1 it must
    /// be in kseg, and the frame goes to PA = SP - 8 - `0x8000_0000`;
    /// anywhere else is a double fault (`None`).
    // Spec (confirmed in MMU_PLAN 424e236): §11.3 says the kernel SP must be a kseg address; the
    // emulator checks the push address SP - 8, so SP = 0x8800_0000 (the
    // top of DDR) is fine and SP = 0x8000_0000 double-faults.
    pub(super) fn frame_slot(&self, sp: u32) -> Option<Mapped> {
        let va = sp.wrapping_sub(8);
        if !self.te {
            return Some(Mapped::identity(va));
        }
        (KSEG..KSEG_END).contains(&va).then(|| Mapped {
            va,
            pa: va - KSEG,
            pa_next: va - KSEG + 8,
        })
    }

    /// Stop on a double fault: an entry whose frame push has no kseg
    /// address under `TE` = 1 (§11.3, `park_kind` 6, a debug halt with a
    /// debugger attached).  Nothing of the entry has happened.
    pub(super) fn double_fault(&mut self, kernel_sp: u32) {
        self.stop = Some(StopReason::DoubleFault(kernel_sp));
    }

    /// `TLBWI`: entry[`TLB_INDEX` index] = {`TLB_HI`, `TLB_LO`}.  Clears the
    /// reservation (§11.3 side effects).
    pub(super) fn tlbwi(&mut self) {
        self.tlb[(self.tlb_index & TLB_INDEX_MASK) as usize] = TlbEntry {
            hi: self.tlb_hi,
            lo: self.tlb_lo,
        };
        self.reservation = None;
    }

    /// `TLBR`: {`TLB_HI`, `TLB_LO`} = entry[`TLB_INDEX` index].
    pub(super) fn tlbr(&mut self) {
        let entry = self.tlb[(self.tlb_index & TLB_INDEX_MASK) as usize];
        self.tlb_hi = entry.hi;
        self.tlb_lo = entry.lo;
    }

    /// `TLBP`: look `TLB_HI`'s VPN up with `TLB_HI`'s ASID (G-aware).  A
    /// hit sets `TLB_INDEX` = {P = 0, index}; a miss sets P and leaves the
    /// index bits alone (`MMU_PLAN.md` 12b3783).
    pub(super) fn tlbp(&mut self) {
        let vpn = self.tlb_hi >> PAGE_SHIFT;
        match self.tlb.iter().position(|e| e.matches(vpn, self.tlb_hi as u8)) {
            Some(index) => self.tlb_index = index as u32,
            None => self.tlb_index |= TLB_INDEX_P,
        }
    }

    /// `TLBINV`: V = 0 in every entry.  Clears the reservation.
    pub(super) fn tlbinv(&mut self) {
        for entry in &mut self.tlb {
            entry.lo &= !TLB_V;
        }
        self.reservation = None;
    }

    /// The physical address a debugger's access to `va` reaches.  With
    /// `TE` = 0 that is `va`.  With `TE` = 1 it is translated as a
    /// supervisor data access by the address map and the current `ASID`:
    /// kseg direct, MMIO identity, useg and kmap through the TLB; `None`
    /// for the hole and for a page no entry maps.  The entry's R/W/X bits
    /// are not checked, so a debugger can read execute-only text and write
    /// a read-only page.
    // klausscc choice (not the spec): "as a supervisor data access would"
    // is applied to the address map and the TLB match only; the entry's R/W/X
    // bits are not checked, so the debugger can read X-only text and write
    // W = 0 pages (and the MPU, as §10.2 says, does not apply to it).
    #[must_use]
    pub fn debug_translate(&self, va: u32) -> Option<u32> {
        if !self.te {
            return Some(va);
        }
        match va {
            KSEG..KSEG_END => Some(va - KSEG),
            MMIO.. => Some(va),
            KSEG_END..KMAP => None,
            _ => self.tlb_lookup(va, self.asid).map(|e| (e.pfn() << PAGE_SHIFT) | (va & PAGE_OFFSET)),
        }
    }

    /// Read RAM at a virtual address for a debugger, page by page through
    /// [`Cpu::debug_translate`], without side effects (MMIO is not read).
    /// Returns the number of bytes read, which stops short at the first
    /// byte that does not translate or is not RAM.
    pub fn debug_read_virtual(&self, addr: u32, buf: &mut [u8]) -> usize {
        let mut done = 0;
        while done < buf.len() {
            let va = addr.wrapping_add(done as u32);
            let chunk = (PAGE_OFFSET - (va & PAGE_OFFSET)) as usize + 1;
            let end = buf.len().min(done + chunk);
            let Some(pa) = self.debug_translate(va) else { break };
            let n = self.debug_read(pa, &mut buf[done..end]);
            done += n;
            if done < end {
                break;
            }
        }
        done
    }

    /// Write RAM at a virtual address for a debugger: every page the range
    /// touches must translate to RAM ([`Cpu::debug_translate`]) before any
    /// byte is written.  Returns false (writing nothing) if one does not.
    pub fn debug_write_virtual(&mut self, addr: u32, data: &[u8]) -> bool {
        let mut chunks = Vec::new();
        let mut done = 0;
        while done < data.len() {
            let va = addr.wrapping_add(done as u32);
            let chunk = (PAGE_OFFSET - (va & PAGE_OFFSET)) as usize + 1;
            let end = data.len().min(done + chunk);
            let Some(pa) = self.debug_translate(va) else { return false };
            if is_mmio(pa) || pa as usize + (end - done) > self.mem.len() {
                return false;
            }
            chunks.push((pa, done..end));
            done = end;
        }
        chunks.into_iter().all(|(pa, range)| self.debug_write(pa, &data[range]))
    }

    /// The MPU regions, for a debugger.
    #[must_use]
    pub const fn mpu_regions(&self) -> &[MpuRegion; MPU_REGIONS] {
        &self.mpu
    }

    /// The TLB entries, for a debugger.
    #[must_use]
    pub const fn tlb_entries(&self) -> &[TlbEntry; TLB_ENTRIES] {
        &self.tlb
    }

    /// Whether translation is on (`STATUS.TE`), so a debugger's addresses
    /// are virtual.
    #[must_use]
    pub const fn translation_on(&self) -> bool {
        self.te
    }
}
