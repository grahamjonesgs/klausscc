#!/usr/bin/env bash
# clang's TLB behaviour under different link orders (KLAUSSCC_TLBSIM=rtl:
# the main-TLB shapes with their micro-ITLBs and the user text-page counts;
# optionally the KLAUSSCC_PROFILE call edges for a call-graph order) on
# warm clang compiles: zlib deflate.c -O2 and the board's C++ hello -O2.
# Adapted from tools/cachesim-ic/run.sh (same inputs, same boot from an
# emulated SD card); each workload boots its own emulator: one unmeasured
# warm-up compile, then the same compile under perfstat (its own window).
#   LLVM         the multicall clang to run (stripped here)
#   TAG          output directory name under $W (default: LLVM's basename)
#   PROFILE=1    also KLAUSSCC_PROFILE=$W/$TAG/prof-<name> with
#                KLAUSSCC_CACHESIM=profile (per-PC counts and call edges)
#   NAMES        subset of "deflate cxx"
#   KLAUSSCC, W (default ~/.cache/tlbord), LINUX_BUILD (default
#   linux-ws/build-tlbord): as in tools/cachesim-ic/run.sh
# Output: $W/$TAG/<name>.err (TLBSIM lines), $W/$TAG/<name>.out (perfstat).
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
  "$B/clang++" --target=$T --sysroot=$RT/linux-ws/dyn/sysroot -E "$HERE/../cachesim-ic/cxx-hello.cpp" -o cxx.ii
  "$B/clang" --target=$T --sysroot=$RT/linux-ws/sysroot -static -O2 "$RT/linux/userland/perfstat.c" -o perfstat
  "$B/clang" --target=$T --sysroot=$RT/linux-ws/sysroot -static -O2 "$HERE/../cachesim-ic/mntdata.c" -o mntdata
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
  [ -d $LINUX_BUILD ] || [ ! -d $RT/linux-ws/build-cachesim-ic ] || cp -a $RT/linux-ws/build-cachesim-ic $LINUX_BUILD
  (cd $RT && INITRAMFS_PROFILE=minish INITRAMFS_EXTRA="$W/in/initramfs.list" linux/build.sh defconfig &&
    linux-ws/linux/scripts/config --file $LINUX_BUILD/.config --set-str CMDLINE "console=hvc0 rdinit=/init rootfstype=ramfs" &&
    INITRAMFS_PROFILE=minish INITRAMFS_EXTRA="$W/in/initramfs.list" linux/build.sh vmlinux) > $W/kernel.log 2>&1 || { tail -30 $W/kernel.log; exit 2; }
  touch $W/in/done
fi
cp "$LLVM" $O/sd/llvm; "$B/llvm-strip" --strip-all $O/sd/llvm
ls -l $O/sd/llvm
rm -f $O/sd.img
mke2fs -q -t ext4 -d $O/sd -L tlbord $O/sd.img 192M
rm -f $O/sd/llvm
cmd() {
  case $1 in
  deflate) echo "/bin/clang --target=$T -O2 -c /w/deflate.i -o /tmp/o.o" ;;
  cxx) echo "/bin/clang++ --target=$T -O2 -c /w/cxx.ii -o /tmp/o.o" ;;
  esac
}
for n in $NAMES; do
  { echo mount; echo /bin/mntdata; cmd $n; echo "/bin/perfstat $(cmd $n)"; echo "echo TO-DONE-$n"; } > $O/$n.in
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
