#!/usr/bin/env bash
# Hardware what-if runs (L2, I-cache shapes, RAS, DIV/MOD, TLB refill cost) on
# clang compiling hello.c -O0, zlib deflate.c -O2 and a C++ hello -O0, in the
# emulator with KLAUSSCC_CACHESIM/PROFILE.  Each compile is its own perfstat
# window.  Output: $W/<name>.err (CACHESIM lines), $W/<name>.out, $W/prof-<name>.N
#   KLAUSSCC   emulator binary (default: this worktree's target/release)
#   W          scratch (default ~/.cache/cachesim)
#   NAMES      subset of "hello deflate cxx"
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
RT=${RT:-/home/graham/Documents/src/klausscpu-runtime}
LL=${LL:-/home/graham/Documents/src/klausscpu-llvm}
W=${W:-$HOME/.cache/cachesim}
KLAUSSCC=${KLAUSSCC:-$HERE/../../target/release/klausscc}
NAMES=${NAMES:-hello deflate cxx}
T=klausscpu-unknown-linux-musl
B=$LL/build/bin
export KLAUSSCPU_LLVM_BIN=$B
export LINUX_BUILD=${LINUX_BUILD:-$RT/linux-ws/build-cachesim}
mkdir -p $W/in; cd $W/in
cat > hello.c <<'E'
#include <stdio.h>
int main(void){printf("hello\n");return 0;}
E
cat > hello.cpp <<'E'
#include <cstdio>
#include <stdexcept>
#include <string>
#include <vector>
int main(){std::vector<std::string> v{"a","b"};std::printf("%zu\n",v.size());try{throw std::runtime_error("x");}catch(const std::exception&){}return 0;}
E
P=("$B/clang" --target=$T --sysroot=$RT/linux-ws/dyn/sysroot)
Z=$(ls -d $RT/linux-ws/br-output-dyn/build/libzlib-*/ | head -1)
"${P[@]}" -E hello.c -o hello.i
"${P[@]}" -E -DHAVE_UNISTD_H -I$Z $Z/deflate.c -o deflate.i
"$B/clang++" --target=$T --sysroot=$RT/linux-ws/dyn/sysroot -E hello.cpp -o cxx.ii
[ -f llvm ] || { cp $LL/build-native-klausscpu-multi/bin/llvm llvm; "$B/llvm-strip" --strip-all llvm; }
"$B/clang" --target=$T --sysroot=$RT/linux-ws/sysroot -static -O2 "$RT/linux/userland/perfstat.c" -o perfstat
{
cat <<L
dir /tmp 1777 0 0
dir /w 0755 0 0
file /bin/perfstat $W/in/perfstat 0755 0 0
file /bin/llvm $W/in/llvm 0755 0 0
slink /bin/clang llvm 0777 0 0
slink /bin/clang++ llvm 0777 0 0
file /w/hello.i $W/in/hello.i 0644 0 0
file /w/deflate.i $W/in/deflate.i 0644 0 0
file /w/cxx.ii $W/in/cxx.ii 0644 0 0
L
} > initramfs.list
(cd $RT && INITRAMFS_PROFILE=minish INITRAMFS_EXTRA="$W/in/initramfs.list" linux/build.sh defconfig &&
  INITRAMFS_PROFILE=minish INITRAMFS_EXTRA="$W/in/initramfs.list" linux/build.sh vmlinux) > $W/kernel.log 2>&1 || { tail -30 $W/kernel.log; exit 2; }
cmd() {
  case $1 in
  hello) echo "/bin/perfstat /bin/clang --target=$T -O0 -c /w/hello.i -o /tmp/o.o" ;;
  deflate) echo "/bin/perfstat /bin/clang --target=$T -O2 -c /w/deflate.i -o /tmp/o.o" ;;
  cxx) echo "/bin/perfstat /bin/clang++ --target=$T -O0 -c /w/cxx.ii -o /tmp/o.o" ;;
  esac
}
for n in $NAMES; do
  { echo mount; cmd $n; echo "echo CS-DONE-$n"; } > $W/$n.in
  (
    start=$(date +%s)
    KLAUSSCC_CACHESIM=1 KLAUSSCC_PROFILE=$W/prof-$n "$KLAUSSCC" -i "$LINUX_BUILD/vmlinux" --emulate --max-instructions 100000000000 \
      --uart-in $W/$n.in --uart-prompt '$ ' </dev/null >$W/$n.out 2>$W/$n.err || true
    echo "$n: $(( $(date +%s) - start )) s"
  ) &
done
wait
