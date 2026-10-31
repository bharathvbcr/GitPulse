#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

cd "${REPO_ROOT}"

echo "========================================================"
echo " Running GitPulse & DevCouncil 4-Tier E2E Test Suite"
echo "========================================================"

if command -v bun >/dev/null 2>&1; then
  bun x vitest run --config vitest.e2e.config.ts "$@"
elif command -v npx >/dev/null 2>&1; then
  npx vitest run --config vitest.e2e.config.ts "$@"
else
  echo "Error: Neither bun nor npx is available in PATH." >&2
  exit 1
fi
