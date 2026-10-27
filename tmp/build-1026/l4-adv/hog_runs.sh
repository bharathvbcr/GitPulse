#!/bin/sh
# Stress matrix for L4 under a CPU hog. Usage: hog_runs.sh <lib binary> <src-tauri dir> <hogs> <label>
# Starts <hogs> `yes` processes (PIDs recorded in $PIDS, killed by PID at the end, never by name),
# then runs each guarded test N times and prints pass/fail counts with uptime.
BIN="$1"; cd "$2" || exit 2
HOGS="$3"; LABEL="$4"
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

rep() { # name N filter...
  name="$1"; n="$2"; shift 2
  ok=0; bad=0; k=0
  while [ $k -lt "$n" ]; do
    env -u L4_MUT "$BIN" "$@" > "$OUT/$LABEL-$name-$k.log" 2>&1
    if [ $? -eq 0 ]; then ok=$((ok+1)); rm -f "$OUT/$LABEL-$name-$k.log"; else bad=$((bad+1)); fi
    k=$((k+1))
  done
  echo "$name: pass=$ok fail=$bad of $n  load=$(uptime | sed 's/.*load averages*: //')"
}

rep hooks_5mib 10 --exact hooks::notify_input_attack_tests::a_five_mib_tool_input_costs_well_under_the_hook_timeout --test-threads=1
rep docs_rebuild 10 --exact docs::tests::a_docs_query_during_a_rebuild_answers_from_the_previous_snapshot --test-threads=1
rep w3l2_module 30 adv_w3l2_tests:: --test-threads=16
