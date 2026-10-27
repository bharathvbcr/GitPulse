#!/bin/bash
# W4a adversary: run a command in a podman Linux image against a source tree.
#
#   run.sh <src-dir> <command...>
#   run.sh /path/to/scratch-copy-of-the-repo suite
#   run.sh /path/to/scratch-copy-of-the-repo cargo test --test adv_w4a_signal_guard -- --ignored --test-threads=1
#
# `suite` is the whole differential suite: every test in the file is #[ignore]d
# (an ordinary `cargo test` reports them as not run, never as passed), so it runs
# `--ignored` and skips by name the four `known_hole_*` tests (they assert what
# the guard should do and fail today, each with its own ignore reason) and the
# uid-model test (it needs the lab run-uid.sh builds).
#   ADV_USER=0 run.sh <src-dir> bash /adv/run-uid.sh     # the uid-model lab, as root
#
# The tests in tests/adv_w4a_signal_guard.rs send real signals to every process
# of the user, so they only run where ADV_W4A_CONTAINER is set, which this
# script does inside the container and nowhere else. <src-dir> is a scratch
# copy of the repository: it is mounted read-write at /work. The image needs
# cargo, cc, procps (pkill, pgrep), psmisc (killall) and sysvinit's pidof.
#
# Environment: ADV_IMAGE (default localhost/gitpulse-linux-test), ADV_TARGET_VOL
# and ADV_CARGO_VOL (podman volumes for the cargo target and home; keep them out
# of any worktree), ADV_USER (a uid to run as), ADV_VERBOSE=1 (print every case).
#
# A cargo build compares file mtimes only: after switching <src-dir> between two
# revisions that share the target volume, touch every .rs file first.
SRC="$1"; shift
if [ "${1:-}" = "suite" ]; then
  set -- cargo test --test adv_w4a_signal_guard -- --ignored --test-threads=1 \
    --skip known_hole_ --skip the_kill_rule_over_real
fi
ADV="$(cd "$(dirname "$0")" && pwd)"
EXTRA=(-e ADV_W4A_CONTAINER=1)
[ -n "${ADV_USER:-}" ] && EXTRA+=(--user "$ADV_USER")
[ -n "${ADV_VERBOSE:-}" ] && EXTRA+=(-e ADV_VERBOSE=1)
exec podman run --rm --security-opt label=disable ${EXTRA[@]+"${EXTRA[@]}"} \
  -v "$SRC":/work -v "${ADV_TARGET_VOL:-w4a-target}":/target -v "${ADV_CARGO_VOL:-w4a-cargo}":/cargo-home -v "$ADV":/adv:ro \
  -e CARGO_TARGET_DIR=/target -e CARGO_HOME=/cargo-home -e CARGO_INCREMENTAL=0 \
  -e CARGO_NET_RETRY=5 -e CARGO_BUILD_JOBS=2 \
  "${ADV_IMAGE:-localhost/gitpulse-linux-test}" \
  bash -lc 'bash /adv/run-in.sh "$@"' x "$@"
