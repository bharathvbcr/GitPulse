#!/bin/sh
# Second stress matrix: the bridge module and the two vitest files under 36 `yes` hogs.
# Usage: hog_runs2.sh <clean lib binary> <lane worktree> <hogs> <label>
# Hog PIDs are recorded and killed by PID. The clean binary is the lane tip + the adversarial test
# (which is skipped here; it is the committed failure, not a flake).
BIN="$1"; W="$2"; HOGS="$3"; LABEL="$4"
OUT=/Users/bharath/Code/devtools/GitPulse/tmp/build-1026/l4-adv/hog
mkdir -p "$OUT"
PIDS="$OUT/$LABEL.pids"
: > "$PIDS"
i=0
while [ $i -lt "$HOGS" ]; do
  yes > /dev/null &
  echo $! >> "$PIDS"
  i=$((i+1))
done
trap 'xargs kill < "$PIDS" 2>/dev/null; rm -f "$PIDS"' EXIT INT TERM
sleep 5
echo "== $LABEL: $HOGS hogs, $(uptime | sed 's/.*load averages*: //')"

rep() { # name N cmd...
  name="$1"; n="$2"; shift 2
  ok=0; bad=0; k=0
  while [ $k -lt "$n" ]; do
    "$@" > "$OUT/$LABEL-$name-$k.log" 2>&1
    if [ $? -eq 0 ]; then ok=$((ok+1)); rm -f "$OUT/$LABEL-$name-$k.log"; else bad=$((bad+1)); fi
    k=$((k+1))
  done
  echo "$name: pass=$ok fail=$bad of $n  load=$(uptime | sed 's/.*load averages*: //')"
}

cd "$W/src-tauri" || exit 2
rep order_seq 20 env -u L4_MUT "$BIN" --exact alerts::bridge::tests::order_tests::sequential_hook_reports_from_one_session_are_never_reordered --test-threads=1
rep alerts_module 5 env -u L4_MUT "$BIN" alerts:: --skip adv_l4 --test-threads=16
cd "$W" || exit 2
rep vite_watch 10 "$W/node_modules/.bin/vitest" run --root "$W" scripts/vite-watch.test.ts
rep vendor_crates 10 "$W/node_modules/.bin/vitest" run --root "$W" scripts/vendor-crates.test.ts
