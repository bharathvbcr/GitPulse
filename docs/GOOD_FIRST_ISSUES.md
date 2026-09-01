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

### B2 · Cache the Vite build across CI jobs ✅ *(Investigated — declined)*
**Labels:** `area: ci`, `performance`

*Result recorded as a comment above the build step in
[`ci.yml`](../.github/workflows/ci.yml).*

The entry called a documented negative result a successful outcome, and that is
what this is. A cold `npm run build` measures ~5.0s and produces 1.2 MB across 12
files; a warm build is no faster. That is the whole prize per leg.

An artifact round trip is an upload step plus a download step plus both actions'
own setup, which does not plausibly beat a 5s rebuild. The structural cost decides
it regardless: the matrix legs depend on nothing today, and sharing `dist/` would
make each wait on a build job, converting parallel work into a serialized edge.
Every leg building from its own checkout also removes a way for a stale artifact
to be tested.

Measured on a developer machine rather than on a runner, so the absolute numbers
are optimistic; the conclusion rests on the ordering and on the serialization
argument, neither of which turns on runner speed. Worth revisiting if the bundle
grows enough that the build stops being cheap.

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

### C1 · Branch health scoring in the sidebar ✅ *(Completed)*
**Labels:** `enhancement`, `area: frontend`

*Implemented in [`src/lib/branches/health.ts`](../src/lib/branches/health.ts) and
[`BranchHealthDot.svelte`](../src/lib/components/BranchHealthDot.svelte).*

No backend change was needed: `BranchInfo` already carries `last_commit_timestamp`,
`commits_ahead_of_base`/`commits_behind_base`, `upstream`, `is_gone`, and
`is_default`, so the verdict is derived from data the sidebar has already fetched.

On the entry's design note — thresholds are a `BranchHealthThresholds` parameter with
documented defaults rather than a constant buried in a comparison, so changing
`staleDays` is a call-site decision and the tests pin behaviour either side of the
boundary. 30 days is a stated default, not a claim about what is right for a team.

Exactly one verdict is returned, so the priority order is the design: upstream gone,
then merged, then diverged, then stale, then behind, then unpublished. It runs
most-actionable first — a merged branch is told it can be deleted rather than that it
is old. Tested for fresh, stale, merged, diverged, behind, unpublished, default,
upstream-gone, threshold boundaries, custom thresholds, and a tip dated in the future.

Only branches needing attention draw an indicator: a dot on every row would make the
healthy majority noisy and the exceptions invisible.

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

### C3 · Commit cadence sparkline ✅ *(Completed)*
**Labels:** `enhancement`, `area: frontend`

*Implemented in [`src/lib/metrics/commitCadence.ts`](../src/lib/metrics/commitCadence.ts)
and [`CommitCadence.svelte`](../src/lib/components/CommitCadence.svelte), mounted in
the status bar's centre segment.*

Buckets are **local calendar days**, not fixed 86 400-second windows: a calendar day
is 23 or 25 hours across a DST transition, and dividing epoch seconds would shift
every later boundary. The axis is built by stepping calendar days, and a test pins
the March 2026 US transition.

Commits outside the window are excluded rather than clamped into an edge bucket,
which would invent activity that never happened, and a history shorter than the
window is reported as `partial` so the view can say the span is the whole loaded
history rather than implying a quiet stretch.

Tested for empty history, a single commit, all commits on one day, the DST
boundary, unusable timestamps, and window clamping. It reads the commits the graph
already loaded, so it costs no additional fetch.

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

## Declined proposals

Kept so they are not re-proposed as though they were never considered.

### Dependabot auto-merge for patch updates ❌ *(Declined)*

Auto-merging patch bumps once CI is green would remove the friction of approving
every `1.2.3 → 1.2.4`. It would also mean dependency code reaches `main`, and from
there a signed desktop binary, without a human having read the diff. For an app
distributed as an installer, that widens supply-chain exposure more than it saves
review time, and CI passing is not evidence that a dependency did not change what
it does. Dependabot still opens the PRs; a person still merges them.

## Proposing an entry

Open an issue describing the task in this format — problem, files, definition of
done, and how to verify. Entries that cannot state a verifiable "done when" usually
need design discussion first, which is what the 🔴 marker is for.
