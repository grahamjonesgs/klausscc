//! The per-retired-instruction trace (`doc/EMULATOR_ISA_SEMANTICS.md` "Trace format").

use super::Cpu;

impl Cpu {
    /// Append one trace line for the just-retired instruction.
    pub(super) fn trace_line(&self, out: &mut dyn std::io::Write, i: u64, pc: u32, word: u32) -> std::io::Result<()> {
        write!(out, "i={i} pc={pc:08x} op={word:08x}")?;
        for (idx, r) in self.regs.iter().enumerate() {
            write!(out, " r{idx}={r:016x}")?;
        }
        // Fixed 7-char field `{zero,sign,carry,overflow,equal,less,ult}` — the last
        // three are DERIVED from Z/S/C/V (E/L/U retired as storage), matching the
        // RTL self-trace which now presents the same derived word.
        write!(out, " sp={:08x} f=", self.sp)?;
        out.write_all(&self.flags.trace_field())?;
        if let Some(w) = self.last_write {
            write!(out, " wr={:08x}/{:02x}/{:016x}", w.addr, w.byte_enable, w.data)?;
        }
        out.write_all(b"\n")
    }
}
