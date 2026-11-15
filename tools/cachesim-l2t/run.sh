#!/usr/bin/env bash
# Timed L2 sweep (KLAUSSCC_L2T, src/emu/cachesim/l2t.rs: the L2T lines) on
# warm clang compiles: zlib deflate.c -O2 and the board's C++ hello -O2.
# The workloads, inputs and kernel are tools/cachesim-ic's (run.sh there):
# the Release clang on an emulated SD card, one unmeasured warm-up compile,
# then the same compile under perfstat (the window).  This script only sets
# the scratch directory and the model's environment:
#   KLAUSSCC_L2T          "only" (default): just the timed model, without
#                         the untimed what-ifs; "1": both
#   KLAUSSCC_L2T_RCS/_WS  the calibration grid (DDR critical latency rc,
#                         32 B write occupancy w), default 36,40,44,48 / 10,16
#   KLAUSSCC_L2T_RC/_W    the timing the full sweep runs at (default 44 / 10)
#   KLAUSSCC_L2T_BASE     base cycles per instruction x1000 (default 2470)
#   KLAUSSCC_L2T_IH/_IX   I-fill costs (D-cache hit; after the line), 10 / 2
# Output: $W/<name>.err (L2T lines), $W/<name>.out.  report.py summarises.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
export W=${W:-$HOME/.cache/cachesim-l2t}
export KLAUSSCC_L2T=${KLAUSSCC_L2T:-only}
export LINUX_BUILD=${LINUX_BUILD:-/home/graham/Documents/src/klausscpu-runtime/linux-ws/build-cachesim-l2t}
RT=${RT:-/home/graham/Documents/src/klausscpu-runtime}
[ -d "$LINUX_BUILD" ] || [ ! -d "$RT/linux-ws/build-cachesim-ic" ] || cp -a "$RT/linux-ws/build-cachesim-ic" "$LINUX_BUILD"
exec "$HERE/../cachesim-ic/run.sh"
