#!/usr/bin/env bash
# Install DevMap for a Claude Code cloud environment, an SSH host, or the dev
# container. Idempotent. One owner of the pin: DEVMAP_REV in
# .github/workflows/ci.yml (read from there, never copied into this file).
#
# DevMap is the only GitPulse tool this can provide on Linux. `gitpulse-mcp` has
# no Linux build and refuses untrusted repositories, so it is NOT installed here.
#
# Default exit status is 0 even when DevMap could not be installed (or anything
# else went wrong), because a setup script that exits non-zero blocks the whole
# cloud session. The outcome is the last line: "devmap: AVAILABLE ..." or
# "devmap: UNAVAILABLE: <reason>". Only --strict (or DEVMAP_SETUP_STRICT=1)
# turns UNAVAILABLE into exit 1.
#
#   scripts/cloud/setup.sh [--strict] [--allow-stale-release]
#
# Layout: as root (a cloud VM) devmap is a real 0755 copy in /usr/local/bin and
# the stamp is in /usr/local/share/gitpulse-devmap, readable by the session
# user. As a normal user it is $HOME/.local/bin and
# $HOME/.local/share/gitpulse-devmap.
#
# Environment (all optional):
#   DEVMAP_REV                  pinned commit; wins over ci.yml
#   DEVMAP_BIN_DIR              where `devmap` lands
#   DEVMAP_STATE_DIR            stamp + build scratch
#   DEVMAP_SYSROOT              prefix for the root layout (tests)
#   DEVMAP_INSTALL_TIMEOUT      seconds for the cargo build     (1800)
#   DEVMAP_DOWNLOAD_TIMEOUT     seconds for the download        (120)
#   DEVMAP_ALLOW_STALE_RELEASE  1 = fall back to the v0.2.3 release (x86_64 Linux
#                               only). STALE: it writes store schema 22, current is 29.
#   DEVMAP_RELEASE_URL + DEVMAP_RELEASE_SHA256
#                               override the release; the URL is refused without
#                               the matching hash
#   GITPULSE_RAW_BASE           where ci.yml is fetched from when the script is
#                               pasted without the repo
set -Eeuo pipefail

STRICT="${DEVMAP_SETUP_STRICT:-0}"
ALLOW_STALE="${DEVMAP_ALLOW_STALE_RELEASE:-0}"
for arg in "$@"; do
  case "$arg" in
    --strict) STRICT=1 ;;
    --allow-stale-release) ALLOW_STALE=1 ;;
    *) echo "setup.sh: unknown argument: $arg" >&2; exit 2 ;;
  esac
done

NO_MCP="gitpulse-mcp: not available on Linux (no Linux build; desktop-only trust grant) - not installed by this script"

# Last-line outcome. Never prints AVAILABLE for anything that was not verified.
unavailable() {
  echo "$NO_MCP"
  echo "devmap: UNAVAILABLE: $1"
  if [ "$STRICT" = 1 ]; then exit 1; fi
  exit 0
}
# Anything not handled below still ends in the UNAVAILABLE outcome, not a bare non-zero exit.
trap 'unavailable "unexpected failure (status $?) near line $LINENO of setup.sh"' ERR

INSTALL_TIMEOUT="${DEVMAP_INSTALL_TIMEOUT:-1800}"
DOWNLOAD_TIMEOUT="${DEVMAP_DOWNLOAD_TIMEOUT:-120}"
RAW_BASE="${GITPULSE_RAW_BASE:-https://raw.githubusercontent.com/bharathvbcr/GitPulse/main}"
DEVCOUNCIL_GIT="https://github.com/bharathvbcr/DevCouncil"
RELEASE_TAG="v0.2.3"
RELEASE_URL="$DEVCOUNCIL_GIT/releases/download/$RELEASE_TAG/devmap-cli-x86_64-unknown-linux-gnu.tar.xz"
# sha256 of that tarball: read from the published .sha256 and cross-checked
# against the GitHub release asset's own digest (both agree).
RELEASE_SHA256="d3e36e56efbec1841846af50b431ecebd5b78b6a8c2a521fa664a06f62b99f7b"
if [ -n "${DEVMAP_RELEASE_URL:-}" ]; then
  [ -n "${DEVMAP_RELEASE_SHA256:-}" ] || unavailable "DEVMAP_RELEASE_URL is refused without an explicit DEVMAP_RELEASE_SHA256"
  RELEASE_URL="$DEVMAP_RELEASE_URL"
  RELEASE_SHA256="$DEVMAP_RELEASE_SHA256"
fi

# --- layout -----------------------------------------------------------------
uid="$(id -u 2>/dev/null || echo 1)"
SYSROOT="${DEVMAP_SYSROOT:-}"
if [ "$uid" = 0 ]; then
  DEF_BIN="$SYSROOT/usr/local/bin"
  DEF_STATE="$SYSROOT/usr/local/share/gitpulse-devmap"
else
  home="${HOME:-}"
  if [ -z "$home" ] && command -v getent >/dev/null 2>&1; then
    home="$(getent passwd "$uid" 2>/dev/null | cut -d: -f6 || true)"
  fi
  if [ -z "$home" ] && { [ -z "${DEVMAP_BIN_DIR:-}" ] || [ -z "${DEVMAP_STATE_DIR:-}" ]; }; then
    unavailable "HOME is not set and could not be looked up; set HOME or DEVMAP_BIN_DIR and DEVMAP_STATE_DIR"
  fi
  DEF_BIN="$home/.local/bin"
  DEF_STATE="$home/.local/share/gitpulse-devmap"
fi
BIN_DIR="${DEVMAP_BIN_DIR:-$DEF_BIN}"
STATE_DIR="${DEVMAP_STATE_DIR:-$DEF_STATE}"
STAMP="$STATE_DIR/stamp"

# run_bounded SECONDS CMD...: kill CMD and its children when it overruns.
# Prefers timeout(1) (which signals its whole process group); otherwise runs the
# command in its own process group (job control) and kills the group.
run_bounded() {
  local secs="$1"; shift
  if command -v timeout >/dev/null 2>&1; then timeout "$secs" "$@"; return; fi
  if command -v gtimeout >/dev/null 2>&1; then gtimeout "$secs" "$@"; return; fi
  if ! set -m 2>/dev/null; then
    echo "setup: no timeout available: this step is NOT time-bounded" >&2
    "$@"; return
  fi
  "$@" &
  local pid=$! waited=0 rc=0
  set +m
  while kill -0 "$pid" 2>/dev/null; do
    if [ "$waited" -ge "$secs" ]; then
      kill -TERM -- "-$pid" 2>/dev/null || kill -TERM "$pid" 2>/dev/null || true
      sleep 1
      kill -KILL -- "-$pid" 2>/dev/null || true
      wait "$pid" 2>/dev/null || true
      return 124
    fi
    sleep 1; waited=$((waited + 1))
  done
  wait "$pid" || rc=$?
  return "$rc"
}

# Pin: env DEVMAP_REV, then ci.yml beside this script, then ci.yml from GitHub.
# Sets REV and PIN_SRC (not run in a subshell).
REV=""; PIN_SRC=""
resolve_rev() {
  local here ci
  if [ -n "${DEVMAP_REV:-}" ]; then REV="$DEVMAP_REV"; PIN_SRC="env DEVMAP_REV"; return; fi
  here="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" 2>/dev/null && pwd || true)"
  ci="$here/../../.github/workflows/ci.yml"
  if [ -n "$here" ] && [ -f "$ci" ]; then
    REV="$(sed -n 's/^  DEVMAP_REV: \([0-9a-f]\{40\}\)[[:space:]]*$/\1/p' "$ci" | head -n 1)"
    PIN_SRC="repo ci.yml"
  elif command -v curl >/dev/null 2>&1; then
    REV="$(curl -fsSL --max-time "$DOWNLOAD_TIMEOUT" "$RAW_BASE/.github/workflows/ci.yml" 2>/dev/null \
      | sed -n 's/^  DEVMAP_REV: \([0-9a-f]\{40\}\)[[:space:]]*$/\1/p' | head -n 1 || true)"
    if [ -n "${GITPULSE_RAW_BASE:-}" ]; then PIN_SRC="fetched ci.yml ($RAW_BASE)"; else PIN_SRC="github main ci.yml"; fi
  fi
}

write_stamp() {
  printf '%s\n' "$1" > "$STAMP" || unavailable "cannot write $STAMP"
  chmod 0644 "$STAMP" || true
}

place_binary() { # $1 = source file
  install -m 0755 "$1" "$BIN_DIR/devmap" || unavailable "cannot install devmap into $BIN_DIR"
}

report_ok() { # $1 = how
  local ver
  ver="$("$BIN_DIR/devmap" --version 2>&1 | head -n 1 || true)"
  [ -n "$ver" ] || unavailable "installed binary failed to run --version"
  echo "$NO_MCP"
  echo "devmap: AVAILABLE $ver at $BIN_DIR/devmap ($1)"
  exit 0
}

resolve_rev
REV_OK=0
if [ "${#REV}" -eq 40 ] && [ -z "${REV//[0-9a-f]/}" ]; then REV_OK=1; fi
PIN_NOTE="pin ${REV:0:12} from ${PIN_SRC:-none}"

# 1. Already installed at the pinned rev.
if [ "$REV_OK" = 1 ] && [ -x "$BIN_DIR/devmap" ] && [ -f "$STAMP" ] && [ "$(cat "$STAMP")" = "source $REV" ] \
  && "$BIN_DIR/devmap" --version >/dev/null 2>&1; then
  report_ok "already installed at pinned rev ${REV:0:12}; $PIN_NOTE"
fi

# 1b. Stale release already installed and sha256-stamped: only when opted in.
if [ "$ALLOW_STALE" = 1 ] && [ -x "$BIN_DIR/devmap" ] && [ -f "$STAMP" ] \
  && [ "$(cat "$STAMP")" = "release $RELEASE_TAG $RELEASE_SHA256" ] && "$BIN_DIR/devmap" --version >/dev/null 2>&1; then
  report_ok "STALE release $RELEASE_TAG already installed, sha256 matched the pinned value, store schema 22 (current is 29)"
fi

mkdir -p "$BIN_DIR" "$STATE_DIR" 2>/dev/null || unavailable "cannot create $BIN_DIR or $STATE_DIR (not writable?)"
chmod 0755 "$STATE_DIR" 2>/dev/null || true
reason=""

# 2. Pinned source build.
if [ "$REV_OK" != 1 ]; then
  reason="could not resolve a 40-character DEVMAP_REV (set DEVMAP_REV, or run from a GitPulse checkout)"
elif ! command -v cargo >/dev/null 2>&1; then
  reason="cargo not found"
else
  echo "setup: cargo install devmap-cli at ${REV:0:12} ($PIN_NOTE; timeout ${INSTALL_TIMEOUT}s)" >&2
  cargo_log="$(mktemp)" || unavailable "cannot create a temp file"
  cargo_rc=0
  run_bounded "$INSTALL_TIMEOUT" cargo install --git "$DEVCOUNCIL_GIT" --rev "$REV" devmap-cli --locked --root "$STATE_DIR/cargo" --force >"$cargo_log" 2>&1 || cargo_rc=$?
  tail -n 40 "$cargo_log" >&2
  if [ "$cargo_rc" -eq 0 ]; then
    rm -f "$cargo_log"
    place_binary "$STATE_DIR/cargo/bin/devmap"
    rm -rf "$STATE_DIR/cargo"
    write_stamp "source $REV"
    report_ok "built from source at pinned rev ${REV:0:12}; $PIN_NOTE"
  else
    # Report what happened: a timeout only on the timeout exit codes, the real
    # status otherwise, a bounded stderr tail, and a network hint only when the
    # output shows a network error.
    if [ "$cargo_rc" -eq 124 ] || [ "$cargo_rc" -eq 137 ]; then
      what="timed out after ${INSTALL_TIMEOUT}s (exit $cargo_rc)"
    else
      what="failed with exit status $cargo_rc"
    fi
    tail_txt="$(tail -n 5 "$cargo_log" | tr '\n' '|' | cut -c1-600)"
    hint=""
    if grep -Eqi 'could not resolve|couldn.t connect|failed to connect|unable to access|network|ssl|connection (refused|reset|timed out)' "$cargo_log"; then
      hint=" (output shows a network error: is github.com reachable?)"
    fi
    rm -f "$cargo_log"
    reason="cargo install at ${REV:0:12} $what$hint; last output: ${tail_txt:-<none>}"
  fi
fi
echo "setup: ERROR: $reason" >&2

# 3. Opt-in stale release.
if [ "$ALLOW_STALE" != 1 ]; then
  unavailable "$reason; the stale $RELEASE_TAG release is not used unless --allow-stale-release (it writes store schema 22, current is 29)"
fi
if [ "$(uname -s)" != Linux ] || [ "$(uname -m)" != x86_64 ]; then
  unavailable "$reason; the $RELEASE_TAG release exists only for x86_64 Linux (this is $(uname -s) $(uname -m))"
fi
for t in curl tar xz; do
  command -v "$t" >/dev/null 2>&1 || unavailable "$reason; release fallback needs $t"
done
work="$(mktemp -d)" || unavailable "$reason; cannot create a temp dir"
trap 'rm -rf "$work"' EXIT
echo "setup: downloading STALE release $RELEASE_TAG (store schema 22; current is 29)" >&2
curl -fsSL --max-time "$DOWNLOAD_TIMEOUT" -o "$work/devmap.tar.xz" "$RELEASE_URL" \
  || unavailable "$reason; release download failed (network blocked?)"
if command -v sha256sum >/dev/null 2>&1; then got="$(sha256sum "$work/devmap.tar.xz" | awk '{print $1}')"
else got="$(shasum -a 256 "$work/devmap.tar.xz" | awk '{print $1}')"; fi
if [ "$got" != "$RELEASE_SHA256" ]; then
  echo "setup: ERROR: SHA256 MISMATCH for $RELEASE_URL (pinned $RELEASE_SHA256, got '${got}'); download discarded" >&2
  unavailable "release sha256 does not match the pinned value; nothing installed"
fi
mkdir -p "$work/x"
tar -xJf "$work/devmap.tar.xz" -C "$work/x" || unavailable "release archive did not extract"
found="$(find "$work/x" -type f -name devmap | head -n 1)"
[ -n "$found" ] || unavailable "release archive contains no devmap binary"
place_binary "$found"
write_stamp "release $RELEASE_TAG $RELEASE_SHA256"
report_ok "STALE release $RELEASE_TAG, sha256 matches the pinned value, store schema 22 (current is 29)"
