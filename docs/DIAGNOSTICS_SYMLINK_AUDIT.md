# September 9 diagnostics audit

Scope: the supplied GitPulse 0.0.9 report, generated at 09:26:18 UTC, and the
Git entry, file-reading, language-measurement, and metric-scheduling paths it
implicates. Implementation baseline: `3d61e2e` in the isolated `e8f6` worktree.

## Evidence and diagnosis

**Verified:** the report contains one `repo:operation` error for a symlink named
`node_modules`, 14 UI timer warnings, and 200 backend log records. Of those
backend records, 55 are slow language-statistics calls, with maximum observed
work time 10,567 ms. Timer warnings measure scheduling delay; neither they nor
the command logs establish the cause of a particular rendered stall.

**Verified reproduction:** `GitReader::get_file_diff` rejected an untracked
directory symlink with the exact reported error. Its shared canonical-path
check followed the final symlink, although Git represents a symlink as a
mode-120000 blob containing the target pathname. The original test suites did
not cover that legitimate Git-entry operation.

**Inferred:** repeated scans and background work could contribute to the
reported delays. The scheduling defects below are independently reproduced;
their contribution to the recorded UI delays has not been measured in the
installed app.

## Audited defects and fixes

| Case | Before | After |
| --- | --- | --- |
| External, dangling, or cyclic symlink diff | Canonical-path error | Render the link's pathname and mode, without reading the target |
| Staged/history/blob query for a symlink | Rejected based on today's filesystem | Validate the Git object path lexically and read Git's object/index data |
| Historical directory replaced by an external symlink | History became inaccessible | Historical content remains readable; working-tree target access remains refused |
| Tabs/newlines/quotes/backslashes in a synthetic diff path | Invalid Git patch headers | C-quoted headers accepted by real `git apply --cached --check` |
| Invalid UTF-8 in synthetic text or symlink payload | Replacement characters offered as a complete patch | Explicit error instead of a corrupted patch |
| 10,001 eligible source files | 10,000 scanned, `truncated=false` | Exact candidate and scanned counts with `truncated=true` |
| Filename with literal leading whitespace | Opens a different normalized path | Preserve exact filenames for reads |
| Three conflict stages for one file | Repeated candidates and LOC | Deduplicate Git's listing before selection |
| Tracked regular file replaced by a FIFO | Scan waits for a writer past its budget | Refuse special files; report the unread candidate as partial |
| File growth between stat and read | Unbounded allocation before the size check | Limit reads to the byte budget plus one overflow byte |
| Last metric subscriber leaves during a scan | Automatic scans restart after teardown | Retain stale state with no automatic timer or scan until observed again |
| Repository changes during a scan | Result temporarily stamped fresh | Keep the result stale until the follow-up measurement |
| Provider throws before returning a promise | Completed promise remains in the running slot | Reserve the slot before calling the provider; reliably release it |
| 1,000 forced rescans while one scan runs | 1,001 simultaneous scan calls | One running scan and one coalesced fresh follow-up |
| Hidden/inactive repositories receive changes | Metrics continue automatic scans | Existing application background scope defers those scans without polling |

The allocation gap for file growth was established by source inspection, not a
controlled filesystem-race reproduction. The regular-file read helper's FIFO
refusal was reproduced against the original code before implementation.

## Preserved contracts

- The existing canonical containment check remains the owner for content reads
  and writes. The new entry resolver checks parents and preserves the final
  entry only for Git diff and `read_link`; it is not a target-read API.
- Git object queries do not read arbitrary working-tree targets. Literal
  pathspecs still prevent glob expansion. Absolute paths and parent traversal
  remain invalid.
- Direct reads retain the existing byte budgets. Unix opens additionally use
  `O_NOFOLLOW` and `O_NONBLOCK`, followed by a regular-file handle check.
- Partial measurements remain visibly partial, and a failed refresh retains
  its previous value with a stale marker. Explicit refreshes can run without
  subscribers and bypass automatic visibility policy.
- Rescan still obtains a measurement started after the request. The old test
  assumed immediate overlap; it now verifies serialized execution and rejection
  of the superseded result. No stale-result assertion was removed.
- The older history test assumed every path beneath an external symlink must
  be refused. It now checks that an untracked path has empty Git history,
  retains diff/blame refusal, and adds content-read refusal plus an assertion
  that the external file is unchanged. A separate fixture verifies actual
  historical content after a working-tree directory is replaced by a link.
- No dependencies, policy gates, permissions, or installed binaries changed.

## Verification boundaries

**Final native verification passed:** `cargo test --no-fail-fast` completed
with 2,065 tests passing and nine existing ignores, plus seven checks in the
custom native-menu harness. The final all-target Clippy run with warnings
denied, Rust formatting, and `git diff --check` also passed. The earlier broad
run exposed the obsolete history expectation and the encoding regression;
both were corrected before this complete rerun.

Baseline verification before production edits passed 22 native tests and 35
frontend tests. New failing cases were run before their corresponding fixes;
the later Git apply and invalid-encoding checks exposed additional defects
during hardening. Source and regression tests add 578 lines and remove 77.

Completed frontend validation: 393 files, 5,183 tests passed and one existing
test skipped. The coverage run also passed: statements 94.02%, branches 88.62%,
functions 95.62%, lines 96.02%. Seven additional soak seeds passed in a focused
run, bringing metric randomized coverage to 16,000 operations across eight
seeds, in addition to the 1,000-request force and visibility cases.

Svelte/TypeScript checks reported zero errors or warnings. All 187 IPC handlers
and 50 wire contracts passed their checks. The production frontend built with
the existing large-chunk advisory, and Chromium returned 24/24 regressions.
The WKWebView harness failed twice to finish within its 60-second deadline;
its last observed requests were dependency/module loads, not a test verdict.
Those attempts do not establish native renderer correctness.

Release-version, workflow, and vendored-schema contract checks passed. The
complete `ci:local` command and native coverage instrumentation were not run;
the native verification above uses the full test suite rather than claiming
a native coverage percentage. DevMap and GitNexus impact checks were used,
including the high-risk history/graph flows. Their incomplete graph walks and
adjacent-symbol diff attribution were checked against source, not treated as
exhaustive coverage or evidence of extra edits.

The regression suite uses real temporary Git repositories and actual Git patch
validation, alongside deterministic timer and lifecycle stress. These checks
do not prove native frame pacing or eliminate every possible repository bug.

The file-read deadline is cooperative between files; OS filesystem operations
can still block on slow or unavailable storage. Canonical validation and open
are separate operations, so this work does not claim to close every concurrent
parent-directory replacement race. Running IPC work is not cancellable from
the frontend, and independent clients or a disposed/recreated metric instance
can still issue their own scans. Those limits must not be described as universal
cancellation or a process-wide scan limit.

The supplied diagnostics predate these changes. No updated app has been
installed, and Windows/Linux runtime behavior remains unverified here.
