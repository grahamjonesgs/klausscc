#!/usr/bin/env python3
"""gen-orders.py OUTDIR PREFIX...: from PREFIX.funcs / PREFIX.edges (one per
workload) write, each workload normalised to the same total:
  cg-all.txt       caller callee weight, every edge
  cg-thr<N>.txt    only edges with weight >= N per million of the merged calls
  funcs-hot.txt    executed functions, hottest first (merged instructions)
  funcs-first.txt  executed functions by first use (merged: the mean over the
                   workloads of first-use position as a fraction of the window,
                   a workload that never runs it counting as 1)"""
import sys, collections
out, prefs = sys.argv[1], sys.argv[2:]
SCALE = 1e9
edges = collections.Counter(); instr = collections.Counter(); first = collections.defaultdict(list)
for p in prefs:
    e = {}
    for line in open(p + '.edges'):
        a, b, c = line.split(); e[(a, b)] = int(c)
    tot = sum(e.values())
    for k, c in e.items(): edges[k] += c / tot * SCALE
    f = {}
    for line in open(p + '.funcs'):
        n, c, t = line.split(); f[n] = (int(c), int(t))
    tot = sum(c for c, _ in f.values())
    t0 = min(t for _, t in f.values()); t1 = max(t for _, t in f.values())
    for n, (c, t) in f.items():
        instr[n] += c / tot * SCALE
        first[n].append((t - t0) / max(1, t1 - t0))
def write_cg(name, thr):
    k = 0
    with open(f'{out}/{name}', 'w') as w:
        for (a, b), c in edges.most_common():
            if c < thr * SCALE / 1e6: break
            if a == b: continue
            w.write(f'{a} {b} {max(1, round(c))}\n'); k += 1
    print(name, k, 'edges', file=sys.stderr)
write_cg('cg-all.txt', 0)
for thr in (1, 10, 100):
    write_cg(f'cg-thr{thr}.txt', thr)
with open(f'{out}/funcs-hot.txt', 'w') as w:
    for n, _ in instr.most_common(): w.write(n + '\n')
nw = len(prefs)
key = {n: (sum(v) + (nw - len(v))) / nw for n, v in first.items()}
with open(f'{out}/funcs-first.txt', 'w') as w:
    for n in sorted(key, key=lambda n: (key[n], -instr[n])): w.write(n + '\n')
print(len(instr), 'executed functions', file=sys.stderr)
