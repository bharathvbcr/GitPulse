#!/bin/sh
# Frontend mutations for L4 (ft-e7c644de). Usage: mutate_frontend.sh <lane worktree>
# Each case edits a tracked production script, runs the test that guards it, records the
# vitest tail in $OUT/<case>.log, and restores the file with git checkout. A case where the
# guarding test PASSES is a BREAK. Needs node_modules in the worktree.
W="$1"
OUT=/Users/bharath/Code/devtools/GitPulse/tmp/build-1026/l4-adv/frontend
mkdir -p "$OUT"
VT="$W/node_modules/.bin/vitest run --root $W"

case_run() { # name file python-expr-old python-new test
  name="$1"; file="$2"; old="$3"; new="$4"; test="$5"
  python3 -I -c "
import sys,pathlib
p=pathlib.Path(sys.argv[1]); s=p.read_text(); assert s.count(sys.argv[2])==1,(sys.argv[2],s.count(sys.argv[2])); p.write_text(s.replace(sys.argv[2],sys.argv[3]))
" "$W/$file" "$old" "$new" || { echo "$name: mutation did not apply"; git -C "$W" checkout -- "$file"; return; }
  start=$(date +%s)
  $VT $test > "$OUT/$name.log" 2>&1
  rc=$?
  end=$(date +%s)
  git -C "$W" checkout -- "$file"
  echo "$name: vitest exit=$rc seconds=$((end-start)) (exit 0 means the guard PASSED under the mutation: BREAK)"
}

# V1: the watcher reloads on a nested worktree's tsconfig (the bug the test guards)
case_run V1_nested_reloads scripts/vite-watch.mjs \
  'return agentLayout(relative) !== null;' 'return false;' scripts/vite-watch.test.ts

# V2: a watcher that ignores everything; the positive control must fail it, bounded by 30 s
case_run V2_ignore_everything scripts/vite-watch.mjs \
  'ignored: [(/** @type {string} */ file) => isNestedAgentWorktree(root, file)]' 'ignored: [() => true]' scripts/vite-watch.test.ts

