//! The per-retired-instruction trace (`doc/EMULATOR_ISA_SEMANTICS.md` "Trace format").

use super::Cpu;

impl Cpu {
    /// Append one trace line for the just-retired instruction.
    ///
    /// Write errors (e.g. a broken pipe on stdout) are intentionally ignored — a
    /// failed trace write must not abort the golden-model run.
    pub(super) fn trace_line(&self, out: &mut dyn std::io::Write, i: u64, pc: u32, word: u32) {
        let _ = write!(out, "i={i} pc={pc:08x} op={word:08x}");
        for (idx, r) in self.regs.iter().enumerate() {
            let _ = write!(out, " r{idx}={r:016x}");
        }
        let f = |b: bool| if b { '1' } else { '0' };
        // Fixed 7-char field `{zero,sign,carry,overflow,equal,less,ult}` — the last
        // three are DERIVED from Z/S/C/V (E/L/U retired as storage), matching the
        // RTL self-trace which now presents the same derived word.
        let _ = write!(
            out,
            " sp={:08x} f={}{}{}{}{}{}{}",
            self.sp,
            f(self.zero),
            f(self.sign),
            f(self.carry),
            f(self.overflow),
            f(self.flag_e()),
            f(self.flag_l()),
            f(self.flag_u()),
        );
        if let Some((addr, be, data)) = self.last_write {
            let _ = write!(out, " wr={addr:08x}/{be:02x}/{data:016x}");
        }
        let _ = out.write_all(b"\n");
    }
}
