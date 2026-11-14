#!/usr/bin/env python3
"""tlbcost.py PROFILE.N [System.map]: instructions and D-cache misses inside
klausscpu_tlb_refill (to the next symbol) per entry to the handler."""
import sys
prof = sys.argv[1]
smap = sys.argv[2] if len(sys.argv) > 2 else '/home/graham/Documents/src/klausscpu-runtime/linux-ws/build-cachesim/System.map'
syms = sorted((int(l.split()[0], 16), l.split()[2]) for l in open(smap) if len(l.split()) == 3)
start = next(a for a, n in syms if n == 'klausscpu_tlb_refill')
end = next(a for a, n in syms if a > start)  # next symbol (data klausscpu_tlb_rr is far; use nm size below)
end = start + 0xdc  # nm -S size of klausscpu_tlb_refill
tot = dict(instr=0, dmiss=0, ifdmiss=0, icmiss=0)
entry = tlbwi = 0
for l in open(prof):
    if l[0] == '#': continue
    f = l.split()
    pc = int(f[0], 16)
    if start <= pc < end:
        tot['instr'] += int(f[1]); tot['icmiss'] += int(f[2]); tot['dmiss'] += int(f[7]); tot['ifdmiss'] += int(f[8])
        if pc == start: entry = int(f[1])
print(f"handler {start:08x}..{end:08x}: entries {entry}")
print(f"  instr {tot['instr']} ({tot['instr']/max(entry,1):.2f}/entry), D misses {tot['dmiss']} ({tot['dmiss']/max(entry,1):.3f}/entry), "
      f"I fills missing D {tot['ifdmiss']} ({tot['ifdmiss']/max(entry,1):.3f}/entry), board-shape I misses {tot['icmiss']} ({tot['icmiss']/max(entry,1):.3f}/entry)")
