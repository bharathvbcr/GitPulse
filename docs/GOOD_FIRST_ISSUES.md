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

### A1 · Add `--json` output to the contract checkers ✅ *(Completed)*
**Labels:** `documentation`, `area: ci`

*Implemented across `scripts/check-ipc-contract.mjs`, `check-coverage-types.mjs`, and
`check-release-version.mjs`.*

`--json` emits the result object each checker already builds and suppresses the text
report; exit codes are identical in both modes (`0` holds, `1` violated, `2` the check
could not run). `check-coverage-types` checks several contracts in one run, so it
emits a single array rather than a stream of objects a consumer would have to
reassemble.

`scripts/cli-json-contract.test.ts` pins the parity in both directions — including
that the human report never leaks into the machine-readable stream — and covers the
violated and internal-error codes, not just the passing one.

### A2 · Add `--help` to every script entry point ✅ *(Completed)*
**Labels:** `good first issue`, `area: ci`

*Implemented in [`scripts/usage.mjs`](../scripts/usage.mjs).*

`formatUsage()` renders a summary, the flag list with descriptions aligned into one
column, and the script's exit-code contract; `wantsHelp()` recognises `--help` and
`-h` anywhere in argv. Every entry point answers with usage and exits `0` — asking
for help is not an error — while an unknown flag still exits `2`.

`scripts/cli-help-contract.test.ts` asserts both halves of that distinction for each
entry point, so a new script cannot quietly ship without help.

`dev-port.mjs` needed no usage of its own: it is a library, and its `--help` handling
correctly defers to the tool being wrapped. `vite-dev.mjs` now forwards `--help`
straight to vite rather than resolving a dev port first, which printed a port notice
and could reclaim a held port as a side effect of asking a question.

### A3 · Aligned column output for the contract report ✅ *(Completed)*
**Labels:** `good first issue`, `area: ci`

*Implemented in [`scripts/columns.mjs`](../scripts/columns.mjs).*

`alignRows()` computes the label column from the labels themselves, so adding a
metric needs no space re-counting and a long label cannot ragged-edge the report.
`alignFlags()` does the same for the usage printer, so the two share one aligner
rather than each padding by hand.

`scripts/columns.test.ts` pins the invariant directly — every value lands in one
column, a longer label re-aligns the whole block, and an oversized value or note
does not disturb it — and `check-ipc-contract.test.ts` pins it through the real
report.

---

## B. CI/CD automation

### B1 · Run the contract checkers in CI ✅ *(Completed)*
**Labels:** `area: ci`

*Implemented in `.github/workflows/ci.yml` (commits `445d096`, `4a5a7ac`).*

`npm run check:ipc`, `npm run check:types`, and `npm run check:release` now run on every push and pull request across all matrix runners (Ubuntu, macOS, Windows) to prevent cross-language drift.

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

### B3 · Fail the coverage workflow on a coverage drop ✅ *(Completed)*
**Labels:** `area: ci`, `test`

*Implemented in [`scripts/check-coverage-floor.mjs`](../scripts/check-coverage-floor.mjs),
wired as `npm run check:coverage`.*

The checker parses both LCOV reports, validates them structurally before trusting any
number (duplicate `SF` records, `LH` above `LF`, `BRH` disagreeing with the `BRDA`
rows, and malformed fields all abort), then applies the floors in
`DEFAULT_THRESHOLDS`: frontend 90% lines / 85% branches, Rust 80% lines. A report that
cannot be parsed exits `2` — distinct from `1` for a missed floor and `0` for a pass —
so a check that could not run never reports the same result as one that ran and passed.

It runs in two places: the *Enforce Coverage Floors and Validate LCOV* step of
[`coverage.yml`](../.github/workflows/coverage.yml), and the tail of `npm run ci:local`,
which regenerates both reports (`npm run coverage` and `cargo llvm-cov`) before checking
them so a stale `lcov.info` cannot be mistaken for a passing run.

Thresholds are overridable per invocation via `--frontend-lines`, `--frontend-branches`,
and `--rust-lines`; `--help` prints the full flag list.

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

### D2 · Document the keyboard shortcut surface ✅ *(Completed)*
**Labels:** `documentation`

*Completed in [`docs/FEATURES.md`](FEATURES.md#4-keyboard-shortcuts-reference).*

All keyboard shortcuts from `ShortcutsModal.svelte`, `App.svelte`, and native OS menus are documented with macOS and Windows/Linux chords.

---

## Proposing an entry

Open an issue describing the task in this format — problem, files, definition of
done, and how to verify. Entries that cannot state a verifiable "done when" usually
need design discussion first, which is what the 🔴 marker is for.
