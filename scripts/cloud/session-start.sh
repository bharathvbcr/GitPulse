#!/usr/bin/env bash
# Claude Code SessionStart hook for cloud / remote sessions. Never downloads or
# installs anything (scripts/cloud/setup.sh does that, before the session).
# Silent unless CLAUDE_CODE_REMOTE=true. Always exits 0.
# Output: SessionStart JSON with hookSpecificOutput.additionalContext (plain
# stdout is also added as context, used when jq is missing).
set -uo pipefail

# Cloud sessions set CLAUDE_CODE_REMOTE=true. SSH hosts opt in explicitly with
# GITPULSE_SESSION_BOOTSTRAP=1 (deliberately not keyed off SSH_CONNECTION).
[ "${CLAUDE_CODE_REMOTE:-}" = "true" ] || [ "${GITPULSE_SESSION_BOOTSTRAP:-}" = "1" ] || exit 0

emit() {
  local msg="${1:0:6000}"
  if command -v jq >/dev/null 2>&1; then
    jq -cn --arg c "$msg" '{hookSpecificOutput:{hookEventName:"SessionStart",additionalContext:$c}}'
  else
    printf '%s\n' "$msg"
  fi
  exit 0
}

# bound SECONDS CMD...: prefers timeout(1); otherwise its own process group
# (job control), killed whole on overrun. Sets UNBOUNDED=1 if neither works.
UNBOUNDED=0
bound() {
  local secs="$1"; shift
  if command -v timeout >/dev/null 2>&1; then timeout "$secs" "$@"; return; fi
  if command -v gtimeout >/dev/null 2>&1; then gtimeout "$secs" "$@"; return; fi
  if ! set -m 2>/dev/null; then UNBOUNDED=1; "$@"; return; fi
  "$@" &
  local pid=$! waited=0 rc=0
  set +m
  while kill -0 "$pid" 2>/dev/null; do
    if [ "$waited" -ge "$secs" ]; then
      kill -TERM -- "-$pid" 2>/dev/null || kill -TERM "$pid" 2>/dev/null
      sleep 1
      kill -KILL -- "-$pid" 2>/dev/null
      wait "$pid" 2>/dev/null
      return 124
    fi
    sleep 1; waited=$((waited + 1))
  done
  wait "$pid" || rc=$?
  return "$rc"
}

NOTE="gitpulse-mcp (gitpulse_* tools) is not available in remote sessions."
SYSROOT="${DEVMAP_SYSROOT:-}"

# Where setup.sh may have put devmap, each with its stamp directory.
BINS=(); STATES=()
if [ -n "${DEVMAP_BIN_DIR:-}" ] || [ -n "${DEVMAP_STATE_DIR:-}" ]; then
  BINS+=("${DEVMAP_BIN_DIR:-${HOME:-}/.local/bin}")
  STATES+=("${DEVMAP_STATE_DIR:-${HOME:-}/.local/share/gitpulse-devmap}")
fi
if [ -n "${HOME:-}" ]; then
  BINS+=("$HOME/.local/bin"); STATES+=("$HOME/.local/share/gitpulse-devmap")
fi
BINS+=("$SYSROOT/usr/local/bin"); STATES+=("$SYSROOT/usr/local/share/gitpulse-devmap")
for b in "${BINS[@]}"; do PATH="$PATH:$b"; done

dm="$(command -v devmap 2>/dev/null || true)"
if [ -z "$dm" ]; then
  emit "DevMap and GitPulse tools are UNAVAILABLE in this session (devmap is not installed; $NOTE). Use source inspection (rg, Read) and say so instead of reporting graph answers. scripts/cloud/setup.sh installs devmap."
fi

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

# The pin the repo builds against, from the local ci.yml only (no network).
pin=""
if [ -f "$root/.github/workflows/ci.yml" ]; then
  pin="$(sed -n 's/^  DEVMAP_REV: \([0-9a-f]\{40\}\)[[:space:]]*$/\1/p' "$root/.github/workflows/ci.yml" | head -n 1)"
fi

# The stamp belonging to the devmap that will actually run.
stamp=""
dm_dir="$(dirname "$dm")"
for i in "${!BINS[@]}"; do
  if [ "$dm_dir" = "${BINS[$i]}" ] && [ -f "${STATES[$i]}/stamp" ]; then stamp="$(cat "${STATES[$i]}/stamp")"; break; fi
done

provenance=""
case "$stamp" in
  "release "*) provenance=" WARNING: devmap is the STALE v0.2.3 release (store schema 22, current is 29), NOT the repo's pin; graph answers may be incomplete." ;;
  "source "*)
    built="${stamp#source }"
    if [ -z "$pin" ]; then provenance=" Could not read the repo's pin from .github/workflows/ci.yml, so whether this build matches it is UNVERIFIED."
    elif [ "$built" = "$pin" ]; then provenance=" Build is at the repo's pin (${pin:0:12})."
    else provenance=" WARNING: this devmap is NOT at the repo's pin (built from ${built:0:12}, repo pins ${pin:0:12}); the store schema may differ, treat graph answers as unverified. Re-run scripts/cloud/setup.sh."; fi ;;
  *) provenance=" WARNING: no install stamp for $dm, so it is NOT at the repo's pin as far as can be verified${pin:+ (${pin:0:12})}; treat graph answers as unverified." ;;
esac

ver="$(bound 10 devmap --version 2>&1 | head -n 1)"
out="$(cd "$root" && bound "${DEVMAP_BUILD_TIMEOUT:-120}" devmap build --manifest 2>&1)"
rc=$?
unb=""
[ "$UNBOUNDED" = 1 ] && unb=" (no timeout available: the build was not time-bounded)"
tail_out="$(printf '%s' "$out" | tail -n 5 | cut -c1-600)"
if [ "$rc" -eq 124 ]; then
  emit "DevMap ($ver) is installed but 'devmap build --manifest' TIMED OUT after ${DEVMAP_BUILD_TIMEOUT:-120}s. Treat DevMap answers as UNAVAILABLE and use source inspection; say so. $NOTE$provenance"
fi
if [ "$rc" -ne 0 ]; then
  emit "DevMap is installed but 'devmap build --manifest' FAILED (exit $rc): $tail_out. Treat DevMap answers as UNAVAILABLE and use source inspection; say so. $NOTE$provenance"
fi
case "$provenance" in
  *WARNING*|*UNVERIFIED*) lead="DevMap is present ($ver) and the index was built for $root, but it is not verified to match the repo." ;;
  *) lead="DevMap is available ($ver); index built for $root with 'devmap build --manifest'." ;;
esac
emit "$lead$provenance Use 'devmap search|explore|impact <name> --json' from the repo root and check truncated/walk_incomplete. $NOTE Use rg/source inspection for anything the graph cannot answer, and say so.$unb"
