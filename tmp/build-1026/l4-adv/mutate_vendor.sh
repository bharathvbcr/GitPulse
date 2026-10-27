#!/bin/sh
# vendor-crates.test.ts mutations. Usage: mutate_vendor.sh <lane worktree>
# W1: scripts/vendor-crates.mjs `check` spins forever. Before the lane's change each call was a
#     `node` child under spawnSync(timeout: 15_000), so a hung script failed the test in 15 s. The
#     lane runs `main` in the vitest worker itself, where a synchronous loop cannot be interrupted by
#     vitest's testTimeout. The run is watched for 90 s; if it has not finished by then the case is a
#     BREAK (a hang where there used to be a timely failure) and the worker tree is killed BY PID.
# W2: main returns 0 at once (a script that does nothing and succeeds): every behavioural test in the
#     file must fail.
W="$1"
OUT=/Users/bharath/Code/devtools/GitPulse/tmp/build-1026/l4-adv/vendor
mkdir -p "$OUT"
VT="$W/node_modules/.bin/vitest"
F=scripts/vendor-crates.mjs

mutate() { # old new
  python3 -I -c "
import sys,pathlib
p=pathlib.Path(sys.argv[1]); s=p.read_text(); assert s.count(sys.argv[2])==1,(sys.argv[2],s.count(sys.argv[2])); p.write_text(s.replace(sys.argv[2],sys.argv[3]))
" "$W/$F" "$1" "$2"
}

descendants() { # pid -> all descendant pids
  for c in $(pgrep -P "$1"); do echo "$c"; descendants "$c"; done
}

# ---- W1
mutate 'export function check(env = process.env) {' 'export function check(env = process.env) { for (;;) {}'
"$VT" run --root "$W" scripts/vendor-crates.test.ts > "$OUT/W1.log" 2>&1 &
pid=$!
i=0
while [ $i -lt 90 ] && kill -0 "$pid" 2>/dev/null; do sleep 1; i=$((i+1)); done
if kill -0 "$pid" 2>/dev/null; then
  echo "W1_check_spins: STILL RUNNING after 90 s -> BREAK (no timely failure)"
  all="$(descendants "$pid") $pid"
  kill $all 2>/dev/null; sleep 2; kill -9 $all 2>/dev/null
else
  wait "$pid"; echo "W1_check_spins: finished rc=$? after ${i}s -> HELD (tail: $(tail -3 "$OUT/W1.log" | tr '\n' ' '))"
fi
git -C "$W" checkout -- "$F"

# ---- W2
mutate 'export function main(argv = process.argv.slice(2)) {' 'export function main(argv = process.argv.slice(2)) { return 0;'
"$VT" run --root "$W" scripts/vendor-crates.test.ts > "$OUT/W2.log" 2>&1
echo "W2_main_noop: vitest rc=$? ($(rg 'Tests ' "$OUT/W2.log" | tr -s ' '))"
git -C "$W" checkout -- "$F"
git -C "$W" status --short
