#!/usr/bin/env bash
# clang's TLB behaviour under different link orders (KLAUSSCC_TLBSIM=rtl:
# the main-TLB shapes with their micro-ITLBs and the user text-page counts;
# optionally the KLAUSSCC_PROFILE call edges for a call-graph order) on
# warm clang compiles.  Adapted from tools/cachesim-ic/run.sh (the same
# boot); the SD card holds the board's /opt/llvm layout (runtime
# linux-ws/toolchain-stage: bin/ with the multicall binary and its
# symlinks, the clang resource dir and the sysroot), mounted on /data, so
# the driver finds its sysroot and ld.lld as on the board.  Each workload
# boots its own emulator: WARMUP unmeasured runs, then the same run under
# perfstat (its own window).
#   LLVM         the multicall clang to run (stripped here)
#   TAG          output directory name under $W (default: LLVM's basename)
#   PROFILE=1    also KLAUSSCC_PROFILE=$W/$TAG/prof-<name> with
#                KLAUSSCC_CACHESIM=profile (per-PC counts and call edges)
#   NAMES        workloads (default "deflate cxx"; see cmd() below):
#                preprocessed compiles hello{,-O0}, deflate{,-O0},
#                bzcompress, bzdecompress, cxx{,-O0} (the board's C++
#                hello), eh (libc++ tests/cxx/eh.cpp, -std=c++23),
#                stdlib-O0 (tests/cxx/stdlib.cpp); drv (the driver on
#                hello.c with the sysroot's headers); link (clang hello.o
#                -o hello: the driver and ld.lld).  -O2 unless named.
#   WARMUP       unmeasured runs first (default 1)
#   TOOLCHAIN    the layout to copy (default runtime linux-ws/toolchain-stage)
#   KLAUSSCC, W (default ~/.cache/tlbord), LINUX_BUILD (default
#   linux-ws/build-tlbord): as in tools/cachesim-ic/run.sh
# Output: $W/$TAG/<name>.err (TLBSIM lines), $W/$TAG/<name>.out (perfstat).
# A call-graph link order (CDSX) from PROFILE=1 runs on an unstripped clang:
#   symbolize.py NM prof-<name>.1 OUT/<name>   per workload (NM: llvm-nm
#       --defined-only -n -S of the binary profiled; thunks map to targets)
#   gen-orders.py DIR OUT/<name>...   merged, each workload normalised:
#       DIR/cg-all.txt (caller callee weight), DIR/funcs-hot.txt
#   relink.sh BIN -Wl,--call-graph-ordering-file=DIR/cg-all.txt   (cdsort)
#   cdsx.py DIR/cg-all.txt <nm of BIN> DIR/funcs-hot.txt ORDER
#   relink.sh BIN2 -Wl,--symbol-ordering-file=ORDER -Wl,--no-warn-symbol-ordering
#   report.py A|B TAG...   the tables
# The body is one compound command, read whole before it runs: editing this
# file while a run is in progress cannot change what that run does.
{
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
RT=${RT:-/home/graham/Documents/src/klausscpu-runtime}
LL=${LL:-/home/graham/Documents/src/klausscpu-llvm}
W=${W:-$HOME/.cache/tlbord}
KLAUSSCC=${KLAUSSCC:-$HERE/../../target/release/klausscc}
LLVM=${LLVM:?the clang binary to run}
TAG=${TAG:-$(basename "$LLVM")}
NAMES=${NAMES-deflate cxx}
T=klausscpu-unknown-linux-musl
B=$LL/build/bin
export KLAUSSCPU_LLVM_BIN=$B
export LINUX_BUILD=${LINUX_BUILD:-$RT/linux-ws/build-tlbord}
O=$W/$TAG
mkdir -p $W/in $O/sd
# The inputs and the kernel are shared by every TAG (built once).
if [ ! -f $W/in/done ]; then
  cd $W/in
  P=("$B/clang" --target=$T --sysroot=$RT/linux-ws/dyn/sysroot)
  Z=$(ls -d $RT/linux-ws/br-output-dyn/build/libzlib-*/ | head -1)
  "${P[@]}" -E -DHAVE_UNISTD_H -I$Z $Z/deflate.c -o deflate.i
  BZ=$(ls -d $RT/linux-ws/br-output-dyn/build/bzip2-*/ | head -1)
  "${P[@]}" -E -I$BZ $BZ/compress.c -o bzcompress.i
  "${P[@]}" -E -I$BZ $BZ/decompress.c -o bzdecompress.i
  printf '#include <stdio.h>\nint main(void) { printf("hello\\n"); return 0; }\n' >hello.c
  "${P[@]}" -E hello.c -o hello.i
  "$B/clang++" --target=$T --sysroot=$RT/linux-ws/dyn/sysroot -E "$HERE/../cachesim-ic/cxx-hello.cpp" -o cxx.ii
  CT=$RT/linux/userland/tests/cxx
  "$B/clang++" --target=$T --sysroot=$RT/linux-ws/dyn/sysroot -std=c++23 -I$CT -E $CT/eh.cpp -o eh.ii
  "$B/clang++" --target=$T --sysroot=$RT/linux-ws/dyn/sysroot -std=c++23 -I$CT -E $CT/stdlib.cpp -o stdlib.ii
  "$B/clang" --target=$T --sysroot=$RT/linux-ws/sysroot -static -O2 "$RT/linux/userland/perfstat.c" -o perfstat
  "$B/clang" --target=$T --sysroot=$RT/linux-ws/sysroot -static -O2 "$HERE/../cachesim-ic/mntdata.c" -o mntdata
  cat > initramfs.list <<L
dir /tmp 1777 0 0
dir /w 0755 0 0
dir /data 0755 0 0
file /bin/perfstat $W/in/perfstat 0755 0 0
file /bin/mntdata $W/in/mntdata 0755 0 0
slink /bin/clang /data/bin/llvm 0777 0 0
slink /bin/clang++ /data/bin/llvm 0777 0 0
file /w/hello.c $W/in/hello.c 0644 0 0
file /w/hello.i $W/in/hello.i 0644 0 0
file /w/deflate.i $W/in/deflate.i 0644 0 0
file /w/bzcompress.i $W/in/bzcompress.i 0644 0 0
file /w/bzdecompress.i $W/in/bzdecompress.i 0644 0 0
file /w/cxx.ii $W/in/cxx.ii 0644 0 0
file /w/eh.ii $W/in/eh.ii 0644 0 0
file /w/stdlib.ii $W/in/stdlib.ii 0644 0 0
L
  [ -d $LINUX_BUILD ] || [ ! -d $RT/linux-ws/build-cachesim-ic ] || cp -a $RT/linux-ws/build-cachesim-ic $LINUX_BUILD
  (cd $RT && INITRAMFS_PROFILE=minish INITRAMFS_EXTRA="$W/in/initramfs.list" linux/build.sh defconfig &&
    linux-ws/linux/scripts/config --file $LINUX_BUILD/.config --set-str CMDLINE "console=hvc0 rdinit=/init rootfstype=ramfs" &&
    INITRAMFS_PROFILE=minish INITRAMFS_EXTRA="$W/in/initramfs.list" linux/build.sh vmlinux) > $W/kernel.log 2>&1 || { tail -30 $W/kernel.log; exit 2; }
  touch $W/in/done
fi
TOOLCHAIN=${TOOLCHAIN:-$RT/linux-ws/toolchain-stage}
rm -rf $O/sd; mkdir -p $O/sd
cp -a $TOOLCHAIN/. $O/sd/
rm -f $O/sd/bin/llvm; cp "$LLVM" $O/sd/bin/llvm; "$B/llvm-strip" --strip-all $O/sd/bin/llvm
ls -l $O/sd/bin/llvm
rm -f $O/sd.img
mke2fs -q -t ext4 -d $O/sd -L tlbord $O/sd.img 256M
rm -rf $O/sd
C=/data/bin/clang
cmd() {
  case $1 in
  hello) echo "$C --target=$T -O2 -c /w/hello.i -o /tmp/o.o" ;;
  hello-O0) echo "$C --target=$T -O0 -c /w/hello.i -o /tmp/o.o" ;;
  deflate) echo "$C --target=$T -O2 -c /w/deflate.i -o /tmp/o.o" ;;
  deflate-O0) echo "$C --target=$T -O0 -c /w/deflate.i -o /tmp/o.o" ;;
  bzcompress) echo "$C --target=$T -O2 -c /w/bzcompress.i -o /tmp/o.o" ;;
  bzdecompress) echo "$C --target=$T -O2 -c /w/bzdecompress.i -o /tmp/o.o" ;;
  cxx) echo "$C++ --target=$T -O2 -c /w/cxx.ii -o /tmp/o.o" ;;
  cxx-O0) echo "$C++ --target=$T -O0 -c /w/cxx.ii -o /tmp/o.o" ;;
  eh) echo "$C++ --target=$T -std=c++23 -O2 -c /w/eh.ii -o /tmp/o.o" ;;
  stdlib-O0) echo "$C++ --target=$T -std=c++23 -O0 -c /w/stdlib.ii -o /tmp/o.o" ;;
  drv) echo "$C -O2 -c /w/hello.c -o /tmp/h.o" ;;
  link) echo "$C /tmp/h.o -o /tmp/hello" ;;
  *) echo "unknown workload $1" >&2; exit 1 ;;
  esac
}
for n in $NAMES; do
  cmd $n >/dev/null
  {
    echo mount; echo /bin/mntdata
    [ $n != link ] || cmd drv
    for _ in $(seq ${WARMUP:-1}); do cmd $n; done
    echo "/bin/perfstat $(cmd $n)"
    echo "echo TO-DONE-$n"
  } > $O/$n.in
  cp $O/sd.img $O/sd-$n.img
  (
    start=$(date +%s)
    env KLAUSSCC_TLBSIM=rtl ${PROFILE:+KLAUSSCC_CACHESIM=profile KLAUSSCC_PROFILE=$O/prof-$n} \
      "$KLAUSSCC" -i "$LINUX_BUILD/vmlinux" --emulate --max-instructions 100000000000 \
      --sd-native --sd-image $O/sd-$n.img \
      --uart-in $O/$n.in --uart-prompt '$ ' </dev/null >$O/$n.out 2>$O/$n.err || true
    rm -f $O/sd-$n.img
    echo "$TAG $n: $(( $(date +%s) - start )) s"
  ) &
done
wait
rm -f $O/sd.img
exit
}
