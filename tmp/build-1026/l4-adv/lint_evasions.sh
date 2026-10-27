#!/bin/sh
# Spawn-gate lint evasion matrix. Usage: lint_evasions.sh <lane worktree>
# For each fixture in evasions/, copies it into <worktree>/src-tauri/tests/, runs the lane's lint
# (tests/process_admission.rs :: every_integration_suite_that_calls_the_library_lifts_the_spawn_rate_cap_or_says_why_not)
# and reports whether the lint FAILED (it caught the suite) or PASSED. The lint reads tests/ at run time.
# Expected: control_unswitched -> caught; ok_control -> passes; every other fixture reaches the gate
# without lifting the cap, so a sound lint would catch it. A fixture the lint passes is a BREAK.
W="$1"
SRC=/Users/bharath/Code/devtools/GitPulse/tmp/build-1026/l4-adv/evasions
OUT=/Users/bharath/Code/devtools/GitPulse/tmp/build-1026/l4-adv/lint
T=/Users/bharath/Code/devtools/GitPulse/tmp/build-1026/target
mkdir -p "$OUT"
LINT=every_integration_suite_that_calls_the_library_lifts_the_spawn_rate_cap_or_says_why_not
for f in "$SRC"/zz_adv_*.rs; do
  name=$(basename "$f" .rs)
  cp "$f" "$W/src-tauri/tests/$name.rs"
  if [ "$name" = zz_adv_path_module ]; then
    mkdir -p "$W/src-tauri/tests/zz_adv_inner"; cp "$SRC/zz_adv_inner/helper.rs" "$W/src-tauri/tests/zz_adv_inner/helper.rs"
  fi
  CARGO_INCREMENTAL=0 CARGO_TARGET_DIR="$T" cargo test --manifest-path "$W/src-tauri/Cargo.toml" \
    --test process_admission -- "$LINT" > "$OUT/$name.log" 2>&1
  rc=$?
  rm -f "$W/src-tauri/tests/$name.rs"
  rm -rf "$W/src-tauri/tests/zz_adv_inner"
  if [ $rc -eq 0 ]; then echo "$name: lint PASSED"; else echo "$name: lint FAILED rc=$rc ($(rg -o 'zz_adv_[a-z_]+\.rs' "$OUT/$name.log" | sort -u | tr '\n' ' '))"; fi
done
git -C "$W" status --short
