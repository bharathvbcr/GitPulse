#!/bin/sh
# Mutation matrix for L4 lib tests. Usage: run_matrix.sh <lib-test-binary> <cwd = lane worktree>/src-tauri
# The binary was built with tmp/build-1026/l4-adv/mutations.patch applied (env-gated by L4_MUT, inert
# when unset). Each row runs ONE guard test under ONE mutation and prints pass/fail. A guard that
# should fail and PASSES is a BREAK. Rows marked "ctrl" run with no mutation and must pass.
BIN="$1"
cd "$2" || exit 2
OUT=/Users/bharath/Code/devtools/GitPulse/tmp/build-1026/l4-adv/matrix
mkdir -p "$OUT"

row() { # mutation test-filter expectation
  mut="$1"; filter="$2"; expect="$3"
  tag="$(echo "$mut-$filter" | tr -c 'A-Za-z0-9\n' '_' | cut -c1-90)"
  start=$(date +%s)
  if [ "$mut" = ctrl ]; then
    env -u L4_MUT "$BIN" --exact "$filter" --test-threads=1 --nocapture > "$OUT/$tag.log" 2>&1
  else
    L4_MUT="$mut" "$BIN" --exact "$filter" --test-threads=1 --nocapture > "$OUT/$tag.log" 2>&1
  fi
  rc=$?
  end=$(date +%s)
  verdict=HELD
  if [ "$expect" = fail ] && [ $rc -eq 0 ]; then verdict=BREAK; fi
  if [ "$expect" = pass ] && [ $rc -ne 0 ]; then verdict=UNEXPECTED_FAIL; fi
  echo "$verdict mut=$mut test=$filter rc=$rc secs=$((end-start)) expect=$expect"
}

HOOK=hooks::notify_input_attack_tests::a_five_mib_tool_input_costs_well_under_the_hook_timeout
DOCS=docs::tests::a_docs_query_during_a_rebuild_answers_from_the_previous_snapshot
ORDER=alerts::bridge::tests::order_tests::sequential_hook_reports_from_one_session_are_never_reordered
SESS=alerts::bridge::tests::a_report_that_names_a_session_is_not_limited_by_the_outside_bucket
TIMELINE=adv_w3l2_tests::adv_flake_timeline_time_to_demotion
EXEC=adv_w3l2_tests::adv_band_survives_exec

row ctrl $HOOK pass
row ctrl $DOCS pass
row ctrl $ORDER pass
row ctrl $SESS pass
row ctrl $TIMELINE pass
row ctrl $EXEC pass

row hooks_parse_slow $HOOK fail
row hooks_reader_slow $HOOK fail
row docs_block $DOCS fail
row order_drop $ORDER fail
row outside_all $SESS fail
row drain_noop $SESS fail
row band_skip $TIMELINE fail
row band_skip $EXEC fail
row band_fake $TIMELINE fail
row band_fake $EXEC fail
