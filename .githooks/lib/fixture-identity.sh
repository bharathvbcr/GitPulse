# shellcheck shell=bash
#
# Which git identities are test fixtures. Sourced by the hooks, never run.
#
# Fixture suites commit in throwaway repositories as contract@gitpulse.test,
# gitpulse@test.local, and other reserved addresses. Those are not people. One
# predicate serves every hook that refuses them, so a commit the pre-commit
# hook would refuse cannot be pushed by another route (a merge, a cherry-pick,
# a rebase, `commit-tree`, or a commit made before the hook existed).
#
# Keep the domain rules in lockstep with is_fixture_author_email in
# src-tauri/src/engine/git_writer.rs. scripts/fixture-author-hook-contract.test.ts
# drives the hooks that source this file.

# Prints the lower-cased email of a `Name <email> time tz` identity.
ident_email() {
  local ident=$1 kind=$2
  local email
  email=$(printf '%s\n' "$ident" | sed -n 's/.*<\([^<>]*\)>.*/\1/p' | tr '[:upper:]' '[:lower:]')
  if [ -z "$email" ]; then
    echo "could not read the $kind email from: $ident" >&2
    return 2
  fi
  printf '%s' "$email"
}

# Succeeds when the lower-cased email is a fixture address. An address with no
# local part or no domain is not a person either.
is_fixture_email() {
  local email=$1
  local domain=${email##*@}
  local local=${email%@*}
  if [ -z "$local" ] || [ -z "$domain" ] || [ "$local" = "$email" ]; then
    return 0
  fi
  case "$domain" in
    test | *.test | invalid | *.invalid | example | *.example | example.com | example.org | example.net)
      return 0
      ;;
  esac
  case "$email" in
    gitpulse@test.local | test@gitpulse.local | t@t | t@e.com | test@test.com)
      return 0
      ;;
  esac
  return 1
}
