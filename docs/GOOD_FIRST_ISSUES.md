# Curated Contributor Backlog

A staging ground for scoped, self-contained work. Each entry names the files to
touch, what "done" means, and how to verify it — enough that a contributor can start
without a design conversation first.

> **Status: every entry above is closed.** A1–A3, B1–B3, C1–C3, and D1–D2 are
> implemented (B2 investigated and declined), so this file currently reads as a record
> of completed work rather than an invitation. New entries follow the format in
> *Proposing an entry* below, and a maintainer files them as GitHub issues with the
> label set in [`.github/labels.yml`](../.github/labels.yml).

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

### C2 · Pull-request review velocity in the GitHub panel ✅ *(Completed)*
**Labels:** `enhancement`, `area: github`

*Implemented in `src-tauri/src/github/mod.rs`,
[`src/lib/github/prVelocity.ts`](../src/lib/github/prVelocity.ts), and
[`GitHubPanel.svelte`](../src/lib/components/GitHubPanel.svelte).*

On the entry's warning: every added field name — `createdAt`, `updatedAt`,
`reviewDecision`, `reviews` — was checked against `gh pr list --json` with no
arguments (gh 2.95.0) before being wired in, since an unknown field fails the whole
listing rather than one column.

`PullRequestInfo` gains `created_at`, `updated_at`, `review_decision`, and
`first_review_at`, the last derived from the earliest *submitted* review — PENDING
reviews are excluded, because counting one would report a pull request as reviewed
when nobody had looked at it. Empty stays distinguishable from "reviewed at time
zero" on both sides of the boundary, and `prVelocity.ts` returns `null` rather than
`0` for an unreviewed pull request throughout.

The aggregate uses the median, so one pull request left open for a year does not
become the headline for the queue, and excludes drafts, which are not waiting on
anyone. The longest-open one is still shown separately so the outlier stays visible.

`scripts/pr-timing-contract.test.ts` pins the Rust struct and the TypeScript
interface field-for-field — the interface is declared inline in the panel, so
`check:types` does not reach it — and pins that the requested gh fields stay in gh's
camelCase vocabulary.

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

### D1 · Fix the three outstanding a11y warnings ✅ *(Completed)*
**Labels:** `good first issue`, `area: frontend`

*Fixed in `CommitRow.svelte` and `files/CodeViewer.svelte`; `npm run check` reports
`0 ERRORS 0 WARNINGS`.*

The commit row's context menu now opens on the ContextMenu key and Shift+F10, anchored
to the row, and moves focus to its first item; the row advertises the relationship with
`aria-haspopup`/`aria-expanded`, and Escape closes the menu from inside it. CodeViewer's
`role="region"` — on an element that takes keystrokes and edits text — became
`role="textbox"` with `aria-multiline` and an `aria-readonly` that tracks edit mode. Both
suppression comments were removed rather than relocated.

A follow-up pass found the wider problem this entry only sampled: twenty bare
`svelte-ignore` comments across ten components meant `npm run check` reporting zero
warnings said "zero *unsuppressed* warnings". Seven modal dialogs were fixed properly and
the five genuinely-correct suppressions now state their reasoning, with
`scripts/a11y-suppression-contract.test.ts` failing on any bare one.

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
