#!/bin/sh
# Probe regression matrix. Usage: run_probe_matrix.sh <lib-mut2 binary> <src-tauri dir>
# lib-mut2 = mutations.patch + apply_probe_mutations.py (three repaired probes put back to the
# session-less form). Each row runs one of those tests, alone, N times, with the lane's drain
# intact (L4_MUT unset) and with the drain disabled (L4_MUT=drain_noop). The lane claims a leftover
# session-less probe "fails every time" with the drain: a test that PASSES is a BREAK of that claim.
BIN="$1"
cd "$2" || exit 2
OUT=/Users/bharath/Code/devtools/GitPulse/tmp/build-1026/l4-adv/matrix
mkdir -p "$OUT"
N="${3:-5}"
for test in \
  alerts::bridge::tests::a_restart_retires_the_old_loop_without_unlinking_the_new_socket \
  alerts::bridge::tests::an_idle_accept_loop_does_not_wake \
  alerts::bridge::tests::a_flood_of_connections_neither_blocks_nor_grows_without_bound
do
  for mut in none drain_noop; do
    pass=0; fail=0
    i=0
    while [ $i -lt "$N" ]; do
      if [ "$mut" = none ]; then
        env -u L4_MUT "$BIN" --exact "$test" --test-threads=1 > "$OUT/probe-$mut-$i.log" 2>&1
      else
        L4_MUT=$mut "$BIN" --exact "$test" --test-threads=1 > "$OUT/probe-$mut-$i.log" 2>&1
      fi
      if [ $? -eq 0 ]; then pass=$((pass+1)); else fail=$((fail+1)); cp "$OUT/probe-$mut-$i.log" "$OUT/probe-fail-$mut-$(echo $test | tr -c 'A-Za-z0-9\n' '_' | cut -c1-40)-$i.log"; fi
      i=$((i+1))
    done
    echo "drain=$mut test=$test pass=$pass fail=$fail of $N"
  done
done
