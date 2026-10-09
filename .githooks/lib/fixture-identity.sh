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
# GitWriter::is_fixture_author_email in src-tauri/src/engine/git_writer.rs is
# the same rule; both are tested against fixture-identity.cases.
# scripts/fixture-author-hook-contract.test.ts drives the hooks that source
# this file.

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

# Succeeds when the address is a test fixture, or is not an address at all:
# empty, without an @, with nothing on either side of its last @, or longer
# than the 254 bytes an SMTP path allows. Neither kind is a person, so
# both are refused rather than recorded. Case and surrounding space are
# ignored. .githooks/lib/fixture-identity.cases is the table this must agree
# with, and GitWriter::is_fixture_author_email is tested against it too.
is_fixture_email() {
  # Bytes and ASCII case, as GitWriter counts and folds them.
  local LC_ALL=C
  is_fixture_email_folded "$(printf '%s' "$1" | tr '[:upper:]' '[:lower:]')"
}

# The same rule for an address already lower-cased, without starting a
# process: fixture-commits.sh folds a whole history in one `tr` and then asks
# this once per address, which keeps a check of thousands of commits to
# seconds instead of a fork per address.
is_fixture_email_folded() {
  local LC_ALL=C
  local email=$1
  email=${email#"${email%%[![:space:]]*}"}
  email=${email%"${email##*[![:space:]]}"}
  [ "${#email}" -gt 254 ] && return 0
  case "$email" in
    *@*) ;;
    *) return 0 ;;
  esac
  local domain=${email##*@}
  local local=${email%@*}
  if [ -z "$local" ] || [ -z "$domain" ]; then
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
