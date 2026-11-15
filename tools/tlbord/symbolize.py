#!/usr/bin/env python3
"""symbolize.py NM PROF OUTPREFIX: per-function instr/first-touch and call edges.
NM: llvm-nm --defined-only -n -S of the unstripped binary run.
PROF: KLAUSSCC_PROFILE file prefix.N (PROF.calls next to it).
Writes OUTPREFIX.funcs (name instr first) and OUTPREFIX.edges (caller callee count)."""
import sys, bisect, collections
nm, prof, out = sys.argv[1:4]
TH = '__KlaussCPUThunk_'
starts, ends, names = [], [], []
for line in open(nm):
    f = line.split()
    if len(f) == 4 and f[2] in 'tTwW':
        a, sz = int(f[0], 16), int(f[1], 16)
        if sz == 0: continue
        starts.append(a); ends.append(a + sz); names.append(f[3])
def func(pc):
    i = bisect.bisect_right(starts, pc) - 1
    if i >= 0 and pc < ends[i]:
        n = names[i]
        if n.startswith(TH):
            n = n[len(TH):]
            # a thunk to a local function names its section: .text.<sym>+0x0
            if n.startswith('.text.') and n.endswith('+0x0'):
                n = n[len('.text.'):-len('+0x0')]
        return n
    return None
instr = collections.Counter(); first = {}
miss = 0
for line in open(prof):
    if line.startswith('#'): continue
    f = line.split()
    pc = int(f[0], 16)
    if pc >= 0x80000000: continue
    n = func(pc)
    if n is None: miss += int(f[1]); continue
    instr[n] += int(f[1])
    fs = int(f[9]) if len(f) > 9 else 0
    if n not in first or fs < first[n]: first[n] = fs
edges = collections.Counter(); emiss = 0
for line in open(prof + '.calls'):
    if line.startswith('#'): continue
    s, t, c = line.split()
    s, t = int(s, 16), int(t, 16)
    if s >= 0x80000000: continue
    a, b = func(s), func(t)
    if a is None or b is None: emiss += int(c); continue
    edges[(a, b)] += int(c)
with open(out + '.funcs', 'w') as w:
    for n, c in instr.most_common():
        w.write(f'{n} {c} {first[n]}\n')
with open(out + '.edges', 'w') as w:
    for (a, b), c in edges.most_common():
        w.write(f'{a} {b} {c}\n')
print(f'{prof}: {len(instr)} functions, {sum(instr.values())} user instr ({miss} unsymbolized), '
      f'{len(edges)} edges, {sum(edges.values())} calls ({emiss} unsymbolized)', file=sys.stderr)
