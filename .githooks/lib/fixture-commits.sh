#!/usr/bin/env bash
#
# Lists commits authored or committed as a test fixture.
#
#   fixture-commits.sh [<tip> [<exclude>...]]
#
# Checks every commit reachable from <tip> (default HEAD) and not from any
# <exclude>; with no <exclude>, every commit no remote-tracking ref holds, which
# is what this checkout has not published. `--remotes` may be given as an
# <exclude> itself. Merges are checked like any other commit.
#
# Prints one line per offending commit: `<short sha> <author email> <committer
# email>`. Exit 0 when there are none, 1 when there are, 2 when the range could
# not be listed — never 0 for a range it did not read.
#
# The pre-push hook runs this for every ref a push would update, CI runs it over
# the pushed or pull-request range (`bun run check:fixture-authors`), and so does
# `ci:local`. Which addresses are fixtures is decided in fixture-identity.sh.

set -euo pipefail

# shellcheck source-path=SCRIPTDIR source=fixture-identity.sh
. "$(dirname "$0")/fixture-identity.sh"

tip=${1:-HEAD}
[ $# -gt 0 ] && shift
exclude=("$@")
[ ${#exclude[@]} -eq 0 ] && exclude=(--remotes)

# Every revision is resolved before git log sees it, so an argument can never
# be read as an option.
resolve() {
  git rev-parse --verify --quiet --end-of-options "$1^{commit}" ||
    { echo "fixture-commits: '$1' is not a commit in this repository" >&2; exit 2; }
}
# One --not covers every exclusion after it; a second would flip back.
revs=("$(resolve "$tip")" --not)
for rev in "${exclude[@]}"; do
  if [ "$rev" = "--remotes" ]; then
    revs+=(--remotes)
  else
    revs+=("$(resolve "$rev")")
  fi
done

# %x1f separates the fields, so no character an address may hold can split one.
# The whole listing is ASCII-folded by one `tr`, so the rule below runs without
# a process per address.
if ! listing=$(git log --format='%h%x1f%ae%x1f%ce' "${revs[@]}" --); then
  echo "fixture-commits: could not list the commits to check" >&2
  exit 2
fi
listing=$(printf '%s' "$listing" | LC_ALL=C tr '[:upper:]' '[:lower:]')

found=0
while IFS=$'\x1f' read -r sha author committer; do
  [ -n "$sha" ] || continue
  if is_fixture_email_folded "$author" || is_fixture_email_folded "$committer"; then
    printf '%s <%s> <%s>\n' "$sha" "$author" "$committer"
    found=$((found + 1))
  fi
done <<<"$listing"

[ "$found" -eq 0 ] && exit 0
echo "fixture-commits: $found commit(s) authored or committed as a test fixture." >&2
exit 1
