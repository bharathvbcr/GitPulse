#!/bin/sh
# A/B: a `git` that never returns, under the vendor-crates tests.
# Usage: vendor_git_hang.sh <lane worktree> <dir holding a `git` shim that sleeps 3600>
# BASE = scripts/vendor-crates.test.ts at fa89a7e6 (each call is `node vendor-crates.mjs` under
#        spawnSync with timeout 15_000, so a hung git is killed and the test fails, and the next test
#        starts). NEW = the lane's version (main runs in the vitest worker; the script's git calls,
#        execFileSync at vendor-crates.mjs:195/546/575, carry no timeout, so the worker is stuck inside
#        one syscall that vitest's testTimeout cannot interrupt).
# The run is watched for 70 s. Progress is measured as the number of DISTINCT hung `git` shim
# processes (`sleep 3600`) seen under the vitest tree: BASE should show one new one about every
# 15 s (each killed by spawnSync's timeout), NEW exactly one for the whole window. The vitest tree is
# then killed BY PID, including the shim sleepers.
W="$1"; SHIM="$2"
OUT=/Users/bharath/Code/devtools/GitPulse/tmp/build-1026/l4-adv/vendor
mkdir -p "$OUT"
VT="$W/node_modules/.bin/vitest"

descendants() { for c in $(pgrep -P "$1"); do echo "$c"; descendants "$c"; done; }

watch() { # name test-file
  name="$1"; file="$2"
  PATH="$SHIM:$PATH" "$VT" run --root "$W" "$file" > "$OUT/$name.log" 2>&1 &
  pid=$!
  seen=""
  i=0
  while [ $i -lt 35 ] && kill -0 "$pid" 2>/dev/null; do
    for d in $(descendants "$pid"); do
      if ps -o command= -p "$d" 2>/dev/null | rg -q '^/bin/sleep 3600$'; then
        case " $seen " in *" $d "*) ;; *) seen="$seen $d";; esac
      fi
    done
    sleep 2; i=$((i+1))
  done
  alive=no; kill -0 "$pid" 2>/dev/null && alive=yes
  echo "$name: after $((i*2)) s vitest alive=$alive; distinct hung git shims seen: $(echo $seen | wc -w) ($seen)"
  all="$(descendants "$pid") $pid"
  kill $all 2>/dev/null; sleep 2; kill -9 $all 2>/dev/null
  for s in $seen; do kill "$s" 2>/dev/null; done
}

git -C "$W" show fa89a7e6:scripts/vendor-crates.test.ts > "$W/scripts/vendor-crates.base.test.ts"
watch BASE_git_hangs scripts/vendor-crates.base.test.ts
rm -f "$W/scripts/vendor-crates.base.test.ts"
watch NEW_git_hangs scripts/vendor-crates.test.ts
git -C "$W" status --short
