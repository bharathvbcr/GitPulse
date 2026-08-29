# Curated Contributor Backlog

A staging ground for scoped, self-contained work. Each entry names the files to
touch, what "done" means, and how to verify it — enough that a contributor can start
without a design conversation first.

> **Status: draft.** These are not yet open GitHub issues. A maintainer files them
> (title, body, and labels are ready to copy) and applies the label set in
> [`.github/labels.yml`](../.github/labels.yml). Until then, comment on an existing
> issue or open one referencing the entry ID below.

Every entry assumes you have read [CONTRIBUTING.md](../CONTRIBUTING.md) and can get
`npm run ci:local` green.

**Difficulty:** 🟢 first issue · 🟡 needs orientation · 🔴 needs design discussion

---

## A. Developer tooling & CLI ergonomics

### A1 · Add `--json` output to the contract checkers 🟢
**Labels:** `good first issue`, `documentation`, `area: ci`

`scripts/check-ipc-contract.mjs`, `check-coverage-types.mjs`, and
`check-release-version.mjs` print a human-readable report through `formatReport()`
and signal outcome via exit code. That is right for a terminal and useless for a
machine — CI cannot annotate a pull request with which handler drifted without
re-parsing prose.

Add a `--json` flag that emits the result object the checker already builds
(`runContractCheck()` returns exactly the right shape) and suppresses the text
report. Keep exit codes identical: `0` holds, `1` violated, `2` internal error.

- **Touch:** `scripts/check-*.mjs`, plus the matching `scripts/*.test.ts`
- **Done when:** `node scripts/check-ipc-contract.mjs --json | jq .ok` prints a
  boolean, the text mode is byte-for-byte unchanged, and exit codes are covered by a
  test for both modes.

### A2 · Add `--help` to every script entry point 🟢
**Labels:** `good first issue`, `area: ci`

`parseArgs()` in the checker scripts rejects an unknown argument with
`unknown argument: --foo` and exit code 2. There is no way to discover the valid
ones short of reading the source. `scripts/dev-port.mjs:226` already anticipates
`--help`/`-h` but nothing implements it.

Add a shared usage printer: flag list, one-line descriptions, exit `0` for an
explicit `--help` (asking for help is not an error).

- **Touch:** `scripts/check-ipc-contract.mjs`, `check-coverage-types.mjs`,
  `check-release-version.mjs`, `dev-port.mjs`, and their tests
- **Done when:** every script answers `--help` with usage and exit `0`, and a test
  asserts the exit code — the distinction between "helped" and "failed" is the
  point.

### A3 · Aligned column output for the contract report 🟢
**Labels:** `good first issue`, `area: ci`

`formatReport()` pads labels by hand with fixed-width string literals. Adding a
metric means re-counting spaces, and a long command name breaks the alignment.

Extract a small column formatter (compute the widest label, pad to it). No
dependency — this is a `Math.max` over label lengths and `padEnd`.

- **Touch:** `scripts/check-ipc-contract.mjs`, `scripts/check-ipc-contract.test.ts`
- **Done when:** a report with a very long command name stays aligned, and a test
  pins the alignment so it cannot silently regress.

---

## B. CI/CD automation

### B1 · Run the contract checkers in CI 🟢 — *highest value in this list*
**Labels:** `good first issue`, `area: ci`, `bug`

`.github/workflows/ci.yml` runs the type check, Vitest, the Vite build, `cargo fmt`,
`cargo clippy`, and `cargo test`. It **never runs** `check:ipc`, `check:types`, or
`check:release`.

So the three checks that exist specifically to catch cross-language drift are
advisory: a pull request that orphans a handler or desynchronises a coverage struct
passes CI. Only `release.yml` runs `check:release`, and only at tag time — meaning
version drift is found after a release has been cut, not before it is merged.

Add the three steps to `ci.yml`. They are fast (pure Node, no network) and need no
new setup beyond the `npm ci` already present.

- **Touch:** `.github/workflows/ci.yml`
- **Done when:** all three run on every push and pull request, and a deliberately
  drifted branch fails CI. Verify the failure locally first — `npm run check:ipc`
  after removing an `invoke` call site should exit `1`.
- **Note:** `check:release` compares manifests against each other; on a non-tag build
  invoke it without `--tag`. Confirm the no-tag path behaves before wiring it up.

### B2 · Cache the Vite build across CI jobs 🟡
**Labels:** `help wanted`, `area: ci`, `performance`

`ci.yml` runs `npm run build` on all three runners, and `release.yml`'s preflight
builds a fourth time on Linux before the platform matrix builds again. The frontend
bundle is platform-independent — the same `dist/` would serve every leg.

Investigate building the frontend once and sharing it via `actions/upload-artifact`
/ `download-artifact`. Measure before and after: if artifact transfer costs more than
the rebuild saves, the correct outcome is a comment in the workflow saying so.

- **Touch:** `.github/workflows/ci.yml`
- **Done when:** either total CI wall-clock measurably drops, or the investigation is
  recorded as a workflow comment with the numbers. A negative result documented is a
  successful outcome here.

### B3 · Fail the coverage workflow on a coverage drop 🟡
**Labels:** `help wanted`, `area: ci`, `test`

`coverage.yml` generates LCOV for both languages, verifies the files were produced,
uploads them, and prints a summary. Nothing acts on the numbers — coverage can fall
indefinitely without CI noticing.

Add a floor. Keep it honest: a threshold that cannot be computed (missing report,
parse failure) must **fail loudly**, never pass by default. A check that could not
run must not report the same result as a check that ran and passed.

- **Touch:** `.github/workflows/coverage.yml`
- **Done when:** an artificially lowered threshold fails the job, a corrupt LCOV file
  fails the job, and the failure message names the actual and required percentages.

---

## C. Metric trackers

### C1 · Branch health scoring in the sidebar 🟡
**Labels:** `help wanted`, `enhancement`, `area: rust-core`

`cmd_branch_stats` returns ahead/behind counts and `cmd_branch_cleanup_plan` already
reasons about stale branches. There is no single at-a-glance signal for "this branch
needs attention".

Derive a health verdict per branch from data already fetched — staleness (days since
last commit), divergence (ahead/behind), and whether it is merged into the default
branch. Surface it as a small indicator in `BranchList.svelte`.

- **Touch:** `src-tauri/src/engine/git_reader.rs` or a new `analyzer` module,
  `src/lib/branches/`, `src/lib/components/BranchList.svelte`
- **Done when:** the scoring function is pure and unit-tested against fixture inputs
  (fresh, stale, merged, diverged, and a branch with no upstream), and the indicator
  has a tooltip explaining the verdict.
- **Design note:** agree the thresholds on the issue before implementing. An
  arbitrary "30 days = stale" baked into a PR is the part most likely to be rejected.

### C2 · Pull-request review velocity in the GitHub panel 🟡
**Labels:** `help wanted`, `enhancement`, `area: github`

`GitHubContext.pull_requests` carries `PullRequestInfo` with state and CI status but
no timing. Time-to-first-review and time-open are the numbers that tell a team where
its pipeline is stalling.

Extend the `gh pr list --json` field set with the timestamps, and render an age
column plus an aggregate in `GitHubPanel.svelte`.

- **Touch:** `src-tauri/src/github/mod.rs`, `src/lib/github/types.ts`,
  `src/lib/components/GitHubPanel.svelte`
- **Done when:** timings appear for open PRs, the Rust struct and TS interface agree
  field for field, and `npm run check:ipc` still reports zero drift.
- **⚠️ Read first:** `release_list_leading_args` in `github/mod.rs` documents a real
  trap — requesting a field `gh` does not support fails the *entire* listing, which
  once degraded a whole panel into an error. Verify every field name against
  `gh pr list --json` with no arguments (it prints the valid set) before wiring it in.

### C3 · Commit cadence sparkline 🟢
**Labels:** `good first issue`, `enhancement`, `area: frontend`

The graph store already holds every loaded commit with author timestamps. A small
commits-per-day sparkline in the status bar or sidebar makes repository rhythm
visible at no fetch cost.

`src/lib/language/barStats.ts` is a working model for a compact stats bar, and
`ChurnBar.svelte` for the rendering approach.

- **Touch:** new `src/lib/metrics/commitCadence.ts` + test, and a component
- **Done when:** the bucketing function is pure and unit-tested (empty history, a
  single commit, commits spanning a DST boundary, all commits on one day), and the
  visual degrades cleanly on a repository with fewer commits than buckets.

---

## D. Documentation & accessibility

### D1 · Fix the three outstanding a11y warnings 🟢
**Labels:** `good first issue`, `area: frontend`

`npm run check` reports three warnings, unchanged for some time:

- `CommitRow.svelte:265` — clickable `<div>` with no keyboard handler
- `files/CodeViewer.svelte:258` — non-interactive element with a non-negative
  `tabIndex`, and mouse/keyboard listeners on a non-interactive element

Each needs a real fix — a keyboard path, or the correct role and semantics — not a
suppression comment. Keyboard-only navigation must reach the same behaviour the mouse
does.

- **Done when:** `npm run check` reports `0 ERRORS 0 WARNINGS`, and each element is
  operable by keyboard alone.

### D2 · Document the keyboard shortcut surface 🟢
**Labels:** `good first issue`, `documentation`

`ShortcutsModal.svelte` and the global handler in `App.svelte` are the only record of
GitPulse's shortcuts. They are not in the README or `docs/FEATURES.md`, so they are
undiscoverable before installing.

- **Touch:** `docs/FEATURES.md`
- **Done when:** every shortcut in `ShortcutsModal.svelte` and `App.svelte`'s
  `handleGlobalKeydown` appears in the table, with macOS and Windows/Linux chords
  given separately.

---

## Proposing an entry

Open an issue describing the task in this format — problem, files, definition of
done, and how to verify. Entries that cannot state a verifiable "done when" usually
need design discussion first, which is what the 🔴 marker is for.
