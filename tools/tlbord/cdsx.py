#!/usr/bin/env python3
"""cdsx.py CG NM ORDER_IN OUT: the CDSX symbol order.
CG: the call-graph file (caller callee weight) a binary was linked with
(lld --call-graph-ordering-file, default cdsort); NM: llvm-nm --defined-only
-n -S of that binary; ORDER_IN: every executed function, hottest first
(gen-orders.py funcs-hot.txt).  OUT: the graph's functions in the order
cdsort placed them, then the executed functions the graph lacks (no calls
into or out of them seen: entry points, tail-called code), hottest first.
Link with --symbol-ordering-file=OUT (a plain cdsort link leaves those in
link order, scattered over the text)."""
import sys
cg, nm, hot_in, out = sys.argv[1:5]
graph = set()
for line in open(cg):
    a, b, _ = line.split()
    graph.add(a); graph.add(b)
addr = {}
for line in open(nm):
    f = line.split()
    if len(f) == 4 and f[2] in 'tTwW' and f[3] not in addr:
        addr[f[3]] = int(f[0], 16)
placed = sorted((n for n in graph if n in addr), key=lambda n: (addr[n], n))
seen = set(placed)
hot = [l.strip() for l in open(hot_in)]
rest = [n for n in hot if n not in seen]
with open(out, 'w') as w:
    w.write('\n'.join(placed + rest) + '\n')
print(f'{len(placed)} graph functions in cdsort order, {len(rest)} executed ones appended', file=sys.stderr)
