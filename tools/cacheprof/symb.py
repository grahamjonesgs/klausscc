#!/usr/bin/env python3
"""Symbolize a KLAUSSCC_PROFILE window file and print the top functions.
usage: symb.py prof.N python|sqlite|sshd [topn]"""
import bisect, subprocess, sys, collections

import os
# The checkouts sit side by side (klausscc, klausscpu-llvm, klausscpu-runtime);
# KLAUSSCPU_SRC overrides their parent.  CACHEPROF_VMLINUX: the run's kernel.
SRC = os.environ.get("KLAUSSCPU_SRC", os.path.abspath(os.path.join(os.path.dirname(__file__), "../../..")))
LLVM = os.path.join(SRC, "klausscpu-llvm/build/bin")
WS = os.path.join(SRC, "klausscpu-runtime/linux-ws")
BR = os.path.join(WS, "br-output-dyn")
VMLINUX = os.environ.get("CACHEPROF_VMLINUX", os.path.expanduser("~/.cache/cacheprof/run/build/vmlinux"))

# (name, base, end, file)  vaddr = pc - base
COMMON = [("ld-musl/libc", 0x2aab0000, 0x2ab90000, WS + "/dyn/musl/lib/libc.so")]
MAPS = {
    # sshd started normally (the kernel maps the PIE at 0x55550000 and the
    # libraries follow libc; "ld-musl --list" maps the program itself first,
    # so its addresses are shifted)
    "sshd": COMMON + [
        ("libssl", 0x2ab90000, 0x2aca0000, BR + "/build/libopenssl-3.6.2/libssl.so.3"),
        ("libcrypto", 0x2aca0000, 0x2b0e0000, BR + "/build/libopenssl-3.6.2/libcrypto.so.3"),
        ("libz", 0x2b0e0000, 0x2b120000, BR + "/build/libzlib-1.3.2/libz.so.1.3.2"),
        ("sshd exe", 0x55550000, 0x55600000, BR + "/build/openssh-10.2p1/sshd"),
    ],
    "python": COMMON + [
        ("libpython3.14", 0x2ab90000, 0x2b0a0000, BR + "/build/python3-3.14.5/libpython3.14.so.1.0"),
        ("python3.14 exe", 0x55550000, 0x55590000, BR + "/build/python3-3.14.5/python"),
    ],
    "sqlite": COMMON + [
        ("libz", 0x2ab90000, 0x2abd0000, BR + "/build/libzlib-1.3.2/libz.so.1.3.2"),
        ("sqlite3 exe", 0x55550000, 0x55740000, BR + "/build/sqlite-3.51.3/sqlite3"),
    ],
}


def plt_ranges(path):
    out = subprocess.run([LLVM + "/llvm-readelf", "-S", "-W", path], capture_output=True, text=True).stdout
    r = []
    for l in out.splitlines():
        if ".plt" in l and "rela" not in l:
            f = l.split("]")[1].split()
            if f[0].startswith(".plt"):
                r.append((int(f[2], 16), int(f[2], 16) + int(f[4], 16)))
    return r


def load_syms(path):
    out = subprocess.run([LLVM + "/llvm-nm", "-n", "--defined-only", path], capture_output=True, text=True).stdout
    addrs, names = [], []
    for l in out.splitlines():
        f = l.split()
        if len(f) == 3 and f[1] in "tTwW":
            addrs.append(int(f[0], 16))
            names.append(f[2])
    return addrs, names


def main():
    prof, which = sys.argv[1], sys.argv[2]
    topn = int(sys.argv[3]) if len(sys.argv) > 3 else 20
    regions = [(n, b, e, f, plt_ranges(f)) + load_syms(f) for n, b, e, f in MAPS[which]]
    k_addrs, k_names = load_syms(VMLINUX)
    agg = collections.defaultdict(lambda: [0, 0, 0, 0, 0, 0])  # instr icmiss ndiv steps cyc nmul
    unmapped = collections.Counter()
    for l in open(prof):
        if l.startswith("#"):
            continue
        f = l.split()
        pc = int(f[0], 16)
        v = [int(x) for x in f[1:]]
        name = None
        if pc >= 0x80000000:
            i = bisect.bisect_right(k_addrs, pc) - 1
            name = ("kernel", k_names[i] if i >= 0 else "?")
        else:
            for rn, b, e, fl, plts, addrs, names in regions:
                if b <= pc < e:
                    if any(lo <= pc - b < hi for lo, hi in plts):
                        name = (rn, "<PLT stubs>")
                        break
                    i = bisect.bisect_right(addrs, pc - b) - 1
                    name = (rn, names[i] if i >= 0 else "?")
                    break
        if name is None:
            name = ("unmapped", "%08x" % (pc & 0xFFFF0000))
        a = agg[name]
        for j in range(6):
            a[j] += v[j]
    tot = [sum(a[j] for a in agg.values()) for j in range(6)]
    print("totals: instr %d icmiss %d ndiv %d div_steps %d div_cycles %d nmul %d" % tuple(tot))
    byobj = collections.defaultdict(lambda: [0] * 6)
    for (o, _), a in agg.items():
        for j in range(6):
            byobj[o][j] += a[j]
    print("\nby object: instr icmiss ndiv divcyc nmul")
    for o, a in sorted(byobj.items(), key=lambda x: -x[1][0]):
        print("  %-16s %12d %10d %9d %11d %9d" % (o, a[0], a[1], a[2], a[4], a[5]))

    def table(title, idx):
        print("\ntop %d by %s" % (topn, title))
        print("  %-14s %-38s %12s %10s %9s %11s %9s" % ("object", "function", "instr", "icmiss", "ndiv", "divcyc", "nmul"))
        for (o, n), a in sorted(agg.items(), key=lambda x: -x[1][idx])[:topn]:
            print("  %-14s %-38s %12d %10d %9d %11d %9d" % (o, n[:38], a[0], a[1], a[2], a[4], a[5]))

    table("I-cache misses (board shape)", 1)
    table("DIV cycles", 4)
    table("MUL count", 5)


main()
