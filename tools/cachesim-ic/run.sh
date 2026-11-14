#!/usr/bin/env bash
# I-cache shape sweep (KLAUSSCC_CACHESIM: the CACHESIM ... ICNLB lines) on
# warm clang compiles: zlib deflate.c -O2 and a C++ hello -O2.  Each
# workload boots its own emulator; in it one unmeasured warm-up compile,
# then the same compile under perfstat (its own window).
#
# The compiler lives on an ext4 image in the emulator's SD slot, mounted
# read-only on /data and paged in through the page cache as on the board
# (/data on the SD card): the 90 MB Release binary does not fit a ramfs
# initramfs in the guest's 128 MiB.  The inputs are the board's
# (runtime linux/userland/tests/native-clang/perf.sh): cxx-hello.cpp is its
# cxx-hello (~/.cache/clang-board/m2/hello.cpp), not tools/cachesim-l2's.
# Adapted from tools/cachesim-l2/run.sh.
# Output: $W/<name>.err (CACHESIM lines), $W/<name>.out
#   KLAUSSCC     emulator binary (default: this worktree's target/release)
#   LLVM         the klausscpu multicall clang to run (default: the
#                Release build, build-native-klausscpu-multi-o2); stripped
#   W            scratch (default ~/.cache/cachesim-ic)
#   NAMES        subset of "deflate cxx" (empty: build only)
#   LINUX_BUILD  kernel build dir (default linux-ws/build-cachesim-ic; a
#                minish kernel, rootfstype=ramfs, from $RT)
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
RT=${RT:-/home/graham/Documents/src/klausscpu-runtime}
LL=${LL:-/home/graham/Documents/src/klausscpu-llvm}
W=${W:-$HOME/.cache/cachesim-ic}
KLAUSSCC=${KLAUSSCC:-$HERE/../../target/release/klausscc}
LLVM=${LLVM:-$LL/build-native-klausscpu-multi-o2/bin/llvm}
NAMES=${NAMES-deflate cxx}
T=klausscpu-unknown-linux-musl
B=$LL/build/bin
export KLAUSSCPU_LLVM_BIN=$B
export LINUX_BUILD=${LINUX_BUILD:-$RT/linux-ws/build-cachesim-ic}
mkdir -p $W/in $W/sd; cd $W/in
P=("$B/clang" --target=$T --sysroot=$RT/linux-ws/dyn/sysroot)
Z=$(ls -d $RT/linux-ws/br-output-dyn/build/libzlib-*/ | head -1)
"${P[@]}" -E -DHAVE_UNISTD_H -I$Z $Z/deflate.c -o deflate.i
"$B/clang++" --target=$T --sysroot=$RT/linux-ws/dyn/sysroot -E "$HERE/cxx-hello.cpp" -o cxx.ii
cp "$LLVM" $W/sd/llvm; "$B/llvm-strip" --strip-all $W/sd/llvm
ls -l $W/sd/llvm
rm -f $W/sd.img
mke2fs -q -t ext4 -d $W/sd -L cachesim $W/sd.img 192M
"$B/clang" --target=$T --sysroot=$RT/linux-ws/sysroot -static -O2 "$RT/linux/userland/perfstat.c" -o perfstat
"$B/clang" --target=$T --sysroot=$RT/linux-ws/sysroot -static -O2 "$HERE/mntdata.c" -o mntdata
cat > initramfs.list <<L
dir /tmp 1777 0 0
dir /w 0755 0 0
dir /data 0755 0 0
file /bin/perfstat $W/in/perfstat 0755 0 0
file /bin/mntdata $W/in/mntdata 0755 0 0
slink /bin/clang /data/llvm 0777 0 0
slink /bin/clang++ /data/llvm 0777 0 0
file /w/deflate.i $W/in/deflate.i 0644 0 0
file /w/cxx.ii $W/in/cxx.ii 0644 0 0
L
# (a first build starts from a copy of tools/cachesim-l2's kernel tree)
[ -d $LINUX_BUILD ] || [ ! -d $RT/linux-ws/build-cachesim ] || cp -a $RT/linux-ws/build-cachesim $LINUX_BUILD
(cd $RT && INITRAMFS_PROFILE=minish INITRAMFS_EXTRA="$W/in/initramfs.list" linux/build.sh defconfig &&
  linux-ws/linux/scripts/config --file $LINUX_BUILD/.config --set-str CMDLINE "console=hvc0 rdinit=/init rootfstype=ramfs" &&
  INITRAMFS_PROFILE=minish INITRAMFS_EXTRA="$W/in/initramfs.list" linux/build.sh vmlinux) > $W/kernel.log 2>&1 || { tail -30 $W/kernel.log; exit 2; }
cmd() {
  case $1 in
  deflate) echo "/bin/clang --target=$T -O2 -c /w/deflate.i -o /tmp/o.o" ;;
  cxx) echo "/bin/clang++ --target=$T -O2 -c /w/cxx.ii -o /tmp/o.o" ;;
  esac
}
for n in $NAMES; do
  { echo mount; echo /bin/mntdata; cmd $n; echo "/bin/perfstat $(cmd $n)"; echo "echo CS-DONE-$n"; } > $W/$n.in
  cp $W/sd.img $W/sd-$n.img
  (
    start=$(date +%s)
    KLAUSSCC_CACHESIM=1 "$KLAUSSCC" -i "$LINUX_BUILD/vmlinux" --emulate --max-instructions 100000000000 \
      --sd-native --sd-image $W/sd-$n.img \
      --uart-in $W/$n.in --uart-prompt '$ ' </dev/null >$W/$n.out 2>$W/$n.err || true
    echo "$n: $(( $(date +%s) - start )) s"
  ) &
done
wait
