#!/usr/bin/env bash
# relink.sh OUT [extra link flags...]: bin/llvm's link from the native build
# dir, without its symbol-ordering flags and dependency file, into OUT.
set -euo pipefail
BD=${NATIVE_BUILD:-/home/graham/Documents/src/klausscpu-llvm/build-native-klausscpu-multi-o2}
OUT=$1; shift
cd $BD
cmd=$(ninja -t commands bin/llvm | tail -1)
cmd=${cmd#: && }
cmd=${cmd%% && *}
args=()
skip=0
read -ra toks <<<"$cmd"
for t in "${toks[@]}"; do
  if [ $skip = 1 ]; then skip=0; continue; fi
  case $t in
  -Wl,--symbol-ordering-file=*|-Wl,--no-warn-symbol-ordering|-Wl,--color-diagnostics) ;;
  -Xlinker) skip=1 ;;   # only --dependency-file follows it
  -o) args+=(-o "$OUT"); skip=1 ;;
  -Wl,-rpath,*) ;;
  *) args+=("$t") ;;
  esac
done
"${args[@]}" "$@"
