#!/bin/sh
# Runs integration suites against a library whose spawn gate logs every spawn made under the
# production rate (apply_metered_log.py). Usage: metered_run.sh <lane worktree> <suite list file> <batch size>
# Each batch is built, run, and its test binaries removed (disk is tight). The log is
# tmp/build-1026/l4-adv/metered.log; one line per metered spawn: suite exe | test thread | class.
W="$1"; LIST="$2"; BATCH="${3:-10}"
L=/Users/bharath/Code/devtools/GitPulse/tmp/build-1026/l4-adv
T=/Users/bharath/Code/devtools/GitPulse/tmp/build-1026/target
OUT="$L/metered"
mkdir -p "$OUT"
set -- $(cat "$LIST")
while [ $# -gt 0 ]; do
  flags=""; names=""; n=0
  while [ $# -gt 0 ] && [ $n -lt "$BATCH" ]; do
    flags="$flags --test $1"; names="$names $1"; n=$((n+1)); shift
  done
  tag=$(echo $names | cut -d' ' -f1)
  df -h / | tail -1 | awk '{print "free: " $4}'
  CARGO_INCREMENTAL=0 CARGO_TARGET_DIR="$T" cargo test --manifest-path "$W/src-tauri/Cargo.toml" $flags --no-fail-fast > "$OUT/batch-$tag.log" 2>&1
  echo "batch $tag: rc=$? suites:$names"
  rg -N "^test result|Running tests/" "$OUT/batch-$tag.log" | paste - - | awk '{print "  " $0}' | cut -c1-150
  for s in $names; do
    for f in "$T"/debug/deps/$s-*; do
      case "$f" in *.d|*.rmeta|*.rlib) ;; *) rm -rf "$f" ;; esac
    done
  done
done
