#!/bin/bash
# Inside the container AS ROOT (ADV_USER=0 run.sh <src> bash /adv/run-uid.sh):
# start decoys with every mix of real/effective/saved uid, then run the
# uid-model test as the ordinary user (1001) and as root. Build the test binary
# first with `run.sh <src> cargo test --test adv_w4a_signal_guard --no-run`.
trap '' TERM HUP INT QUIT USR1 USR2 ALRM
L=/tmp/adv-lab
rm -rf "$L"; mkdir -p "$L"; chmod 755 "$L"
cc -O1 -o "$L/sink" /work/src-tauri/tests/fixtures/adv_w4a_sink.c || exit 1
: > "$L/sink.log"; chmod 666 "$L/sink.log"
: > "$L/pids"
n=0
start() { # label, how, [env...]
  label="$1"; how="$2"; shift 2
  cp "$L/sink" "$L/$label"
  case "$how" in
    suid) chown root:root "$L/$label"; chmod 4755 "$L/$label" ;;
    *) chmod 755 "$L/$label" ;;
  esac
  n=$((n+1))
  if [ "$how" = "runner" ] || [ "$how" = "suid" ]; then
    SINK_LOG="$L/sink.log" setpriv --reuid=1001 --regid=1001 --clear-groups env "$@" "$L/$label" "marker-uid-$n" &
  else
    SINK_LOG="$L/sink.log" env "$@" "$L/$label" "marker-uid-$n" &
  fi
  echo "$label $!" >> "$L/pids"
}
start gitpulse-uid-1 runner            # ruid=euid=suid=1001
start gitpulse-uid-2 suid              # ruid 1001, euid 0, suid 0 (set-uid root run by the user)
start gitpulse-uid-3 root              # all 0
start gitpulse-uid-4 root SINK_RES=0,1001,0     # ruid 0, euid 1001, suid 0
start gitpulse-uid-5 root SINK_RES=1001,1001,0  # ruid 1001, euid 1001, suid 0
start gitpulse-uid-6 root SINK_RES=0,0,1001     # ruid 0, euid 0, suid 1001
start gitpulse-uid-7 root SINK_RES=1001,0,0     # ruid 1001, euid 0, suid 0
start gitpulse-uid-8 root SINK_RES=1001,0,1001  # ruid 1001, euid 0, suid 1001
start gitpulse-uid-9 root SINK_RES=0,1001,1001  # ruid 0, euid 1001, suid 1001
sleep 1.5
echo "ready lines: $(grep -c '^R ' "$L/sink.log") of $n"
echo "--- credentials as the kernel reports them"
while read -r label pid; do
  printf '%s pid %s: %s | dir owner %s | environ owner %s\n' "$label" "$pid" \
    "$(grep '^Uid:' /proc/$pid/status | tr '\t' ' ')" "$(stat -c %u /proc/$pid)" "$(stat -c %u /proc/$pid/environ)"
done < "$L/pids"
BIN=$(ls -t /target/debug/deps/adv_w4a_signal_guard-* | grep -v '\.d$' | head -1)
echo "--- as 1001 (the ordinary hook)"
setpriv --reuid=1001 --regid=1001 --clear-groups env ADV_UID_LAB="$L" "$BIN" the_kill_rule --nocapture --test-threads=1
echo "--- as root (the hook run as root)"
env ADV_UID_LAB="$L" "$BIN" the_kill_rule --nocapture --test-threads=1
