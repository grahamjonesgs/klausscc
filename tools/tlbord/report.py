#!/usr/bin/env python3
"""report.py A|B TAG... : tables from run.sh's $W/TAG/<workload>.{err,out}.
A: the TLB shapes (refills per 1k, estimated cycles at 27 per main-TLB and 3
per micro-ITLB refill); B: one row per variant and workload (the real
FA32 RR TLB, uITLB 4, the user text pages)."""
import sys, re, os
W = os.environ.get('W', os.path.expanduser('~/.cache/tlbord'))
NAMES = ['hello', 'hello-O0', 'deflate', 'deflate-O0', 'bzcompress', 'bzdecompress', 'cxx', 'cxx-O0',
         'eh', 'stdlib-O0', 'terminate', 'drv', 'link']
def parse(tag, n):
    d = {'cfg': {}}
    try:
        err = open(f'{W}/{tag}/{n}.err').read()
        out = open(f'{W}/{tag}/{n}.out', errors='replace').read()
    except OSError:
        return None
    m = re.search(r'TLBSIM window instructions=(\d+)', err)
    if not m: return None
    d['instr'] = int(m.group(1))
    for m in re.finditer(r'TLBSIM cfg="([^"]+)" misses=(\d+).*?((?: uitlb\d+=\d+)+)', err):
        d['cfg'][m.group(1)] = (int(m.group(2)), {int(a): int(b) for a, b in re.findall(r'uitlb(\d+)=(\d+)', m.group(3))})
    m = re.search(r'TLBSIM pages user_instr=(\d+) distinct=(\d+) cover50=(\d+) cover90=(\d+) cover99=(\d+) (w1M .*?) (w10M .*)', err)
    d['user'] = int(m.group(1)); d['distinct'] = int(m.group(2)); d['cover'] = tuple(map(int, m.group(3, 4, 5)))
    d['w1'] = dict(re.findall(r'(\w+)=([\d.]+)', m.group(6))); d['w10'] = dict(re.findall(r'(\w+)=([\d.]+)', m.group(7)))
    ps = out[out.find('perfstat: '):]
    for k in ['instructions', 'tlb_refills', 'tlb_walks']:
        m = re.search(rf'^\s*{k}\s+([\d.]+)', ps, re.M)
        d[k] = float(m.group(1)) if m else None
    m = re.search(r'user ([\d.]+) s, sys ([\d.]+) s', ps); d['us'] = m.groups() if m else None
    m = re.search(r'faults (\d+) minor (\d+) major', ps); d['majflt'] = int(m.group(2)) if m else -1
    return d
mode = sys.argv[1]; tags = sys.argv[2:]
if mode == 'A':
    cfgs = ['FA32 RR', 'FA48 RR', 'FA64 RR', 'FA128 RR', 'SA128 2w vpn LRU', 'SA128 2w vpn RR', 'SA128 4w vpn LRU']
    for n in NAMES:
        for tag in tags:
            d = parse(tag, n)
            if not d: continue
            I = d['instr']
            print(f'\n### {n} {tag}: {I/1e9:.3f} G instr in the window, user {d["user"]/1e9:.3f} G; perfstat tlb_refills {d["tlb_refills"]/1e6:.2f} M')
            print('| main TLB | refills | /1k | uITLB 4 /1k | uITLB 8 /1k | uITLB 16 /1k | cycles/1k: 27*main + 3*uITLB(4 / 8 / 16) |')
            print('|---|---|---|---|---|---|---|')
            for c in cfgs:
                if c not in d['cfg']: continue
                m, u = d['cfg'][c]
                k = 1000 / I
                cyc = [27 * m * k + 3 * u[s] * k for s in (4, 8, 16)]
                print(f'| {c} | {m/1e6:.2f} M | {m*k:.2f} | {u[4]*k:.2f} | {u[8]*k:.2f} | {u[16]*k:.2f} | {cyc[0]:.0f} / {cyc[1]:.0f} / {cyc[2]:.0f} |')
else:
    print('| variant | workload | window instr | user instr | main-TLB refills (/1k) | uITLB4 refills (/1k) | est. TLB cycles (M) | distinct pages | 50/90/99% pages | per-10M mean/max | per-1M mean/max | major faults |')
    print('|---|---|---|---|---|---|---|---|---|---|---|---|')
    for tag in tags:
        for n in NAMES:
            d = parse(tag, n)
            if not d: continue
            I = d['instr']; m, u = d['cfg']['FA32 RR']; k = 1000 / I
            print(f'| {tag} | {n} | {I/1e9:.3f} G | {d["user"]/1e9:.3f} G | {m/1e6:.2f} M ({m*k:.2f}) | {u[4]/1e6:.2f} M ({u[4]*k:.2f}) | {(27*m+3*u[4])/1e6:.0f} | {d["distinct"]} | {"/".join(map(str, d["cover"]))} | {d["w10"]["mean"]}/{d["w10"]["max"]} | {d["w1"]["mean"]}/{d["w1"]["max"]} | {d["majflt"]} |')
