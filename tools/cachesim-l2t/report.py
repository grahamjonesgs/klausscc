#!/usr/bin/env python3
"""Summarise the L2T lines of tools/cachesim-l2t/run.sh.

usage: report.py [--base deflate=MILLI,cxx=MILLI] [--ih N] W/deflate.err W/cxx.err

For each workload: the calibration grid (today's system, no L2) against the
board's counters, then every configuration's modelled cycles and the % vs
the no-L2 anchor at the same DDR timing.  The base cost per instruction is
fitted per workload to the board's process cycles (user + sys at 100 MHz)
at the chosen anchor unless --base gives it; --ih re-prices the I fills
that hit the D-cache (the run used KLAUSSCC_L2T_IH).
"""
import re
import sys

# The board (linux-ws/native-perf-icache/perf.txt, 2026-11-15, I-cache
# image f799182, walker on): process cycles = (user + sys) x 100 MHz.
BOARD = {
    "deflate": dict(proc=5.827e9, cycles=5.880e9, instr=1.166e9, ic_miss=58.08e6, ic_fill=2341.1e6,
                    dc_rd_miss=57.95e6, dc_wr_miss=7.286e6, dc_wb=9.756e6, dc_stall=2734.6e6, mem_wait=1116.6e6),
    "cxx": dict(proc=14.849e9, cycles=17.173e9, instr=2.399e9, ic_miss=178.63e6, ic_fill=8317.5e6,
                dc_rd_miss=189.26e6, dc_wr_miss=30.66e6, dc_wb=38.10e6, dc_stall=9179.8e6, mem_wait=3439.5e6),
}

LINE = re.compile(r"L2T w\d+ \[(?P<name>[^\]]+)\] cycles (?P<cycles>\d+) stall (?P<stall>\d+) \| (?P<rest>.*)")
HDR = re.compile(r"L2T w\d+ instructions (\d+) user (\d+) kernel (\d+) kernel_entries (\d+) base_milli (\d+) base_cycles (\d+) icnlb_demand_misses (\d+) uitlb_misses (\d+)")


def parse(path):
    hdr, rows = None, {}
    for line in open(path):
        m = HDR.search(line)
        if m:
            hdr = [int(x) for x in m.groups()]
            rows = {}
            continue
        m = LINE.search(line)
        if not m:
            continue
        rest = m.group("rest")
        d = {"stall": int(m.group("stall"))}
        for k, v in re.findall(r"([a-z_]+) (\d+)(?![/\d])", rest):
            d.setdefault(k, int(v))
        for kind in ("I", "D", "WB"):
            mm = re.search(rf"\b{kind} (\d+)/(\d+)", rest)
            if mm:
                d[f"h{kind}"], d[f"m{kind}"] = int(mm.group(1)), int(mm.group(2))
        mm = re.search(r"ifill dhit (\d+) dmiss (\d+)", rest)
        d["dhit"], d["dmiss"] = int(mm.group(1)), int(mm.group(2))
        mm = re.search(r"wbfull (\d+) cyc (\d+) wb_fwd (\d+) ddr_waits (\d+) cyc (\d+)", rest)
        d.update(zip(["wbfull_ev", "wbfull_cyc", "wb_fwd", "ddr_waits", "ddr_wait_cyc"], map(int, mm.groups())))
        mm = re.search(r"pf queued (\d+) issued (\d+) useful (\d+) unused (\d+) dropped (\d+) late (\d+)", rest)
        d.update(zip(["pfq", "pfi", "pfu", "pfun", "pfd", "pfl"], map(int, mm.groups())))
        rows[m.group("name")] = d
    return hdr, rows


def main():
    args = sys.argv[1:]
    bases, ih_new = {}, None
    while args and args[0].startswith("--"):
        if args[0] == "--base":
            bases = {k: int(v) for k, v in (x.split("=") for x in args[1].split(","))}
        elif args[0] == "--ih":
            ih_new = int(args[1])
        args = args[2:]
    ih_run = 10
    for path in args:
        wl = "cxx" if "cxx" in path else "deflate"
        b = BOARD[wl]
        hdr, rows = parse(path)
        n, user, kern, kentries, base_milli = hdr[:5]
        print(f"=== {wl}: emulator {n/1e9:.3f} G instructions (user {user/1e9:.3f} G, kernel {kern/1e9:.3f} G, "
              f"{kentries} kernel entries), I-cache demand misses {hdr[6]/1e6:.2f} M; "
              f"board {b['instr']/1e9:.3f} G instr, process {b['proc']/1e9:.2f} G cycles ({b['cycles']/1e9:.2f} G global)")

        def stall(d):
            s = d["stall"]
            if ih_new is not None:
                s += d["dhit"] * (ih_new - ih_run)
            return s

        print(f"{'calibration (no L2)':<22} {'ic_fill/fill':>12} {'dc_stall/rdmiss':>15} {'d_stall':>8} "
              f"{'rd_miss':>8} {'wr+z':>7} {'wb':>7} {'stall G':>8} {'base fit':>8}")
        print(f"{'board':<22} {b['ic_fill']/b['ic_miss']:>12.1f} {b['dc_stall']/b['dc_rd_miss']:>15.1f} {'':>8} "
              f"{b['dc_rd_miss']/1e6:>8.2f} {b['dc_wr_miss']/1e6:>7.2f} {b['dc_wb']/1e6:>7.2f}")
        for name, d in rows.items():
            if not name.endswith("noL2"):
                continue
            fills = d["dhit"] + d["dmiss"]
            icf = d["ic_fill"] + (d["dhit"] * (ih_new - ih_run) if ih_new is not None else 0)
            rdm = d["dmiss"] + d["dread_miss"]
            print(f"{name:<22} {icf/fills:>12.1f} {d['dc_stall']/rdm:>15.1f} {d['d_stall']/1e9:>8.3f} "
                  f"{rdm/1e6:>8.2f} {(d['dwrite_miss']+d['zero_miss'])/1e6:>7.2f} {d['wbacks']/1e6:>7.2f} "
                  f"{stall(d)/1e9:>8.3f} {(b['proc']-stall(d))/n:>8.3f}")
        base = bases.get(wl)
        print(f"\n{'config':<26} {'cycles G':>9} {'vs noL2':>8} {'L2 hit I':>8} {'D':>6} {'WB':>6} "
              f"{'DDR rd MB':>9} {'wr MB':>7} {'wbfull Mc':>9} {'ddrwait Mc':>10} pf issued/useful/unused/dropped/late")
        for name, d in rows.items():
            tag = " ".join(name.split()[:2])
            anchor = rows[f"{tag} noL2"]
            bm = base if base is not None else round(1000 * (b["proc"] - stall(rows[f"{tag} noL2"])) / n)
            tot = n * bm / 1000 + stall(d)
            atot = n * bm / 1000 + stall(anchor)
            rate = lambda k: f"{100*d['h'+k]/max(1,d['h'+k]+d['m'+k]):.1f}" if f"h{k}" in d else "-"
            pf = f"{d['pfi']/1e6:.2f}/{d['pfu']/1e6:.2f}/{d['pfun']/1e6:.2f}/{d['pfd']/1e6:.2f}/{d['pfl']/1e6:.2f} M" if d["pfq"] else ""
            print(f"{name:<26} {tot/1e9:>9.3f} {100*(tot/atot-1):>+7.1f}% {rate('I'):>8} {rate('D'):>6} {rate('WB'):>6} "
                  f"{d['ddr_rd']/1e6:>9.0f} {d['ddr_wr']/1e6:>7.0f} {d['wbfull_cyc']/1e6:>9.2f} "
                  f"{d['ddr_wait_cyc']/1e6:>10.1f} {pf}")
        print()


if __name__ == "__main__":
    main()
