# GitPulse 0.2.0 Git-client release audit

Date: 2026-09-10. Status: verified local candidate; no publication.

## Scope and provenance

The audit starts at `afe6a14159502a1c420f386fcf8d41bbb617eb1b` plus the
94 pre-existing dirty/untracked paths captured in `baseline.json` and
`baseline.patch`. They include ongoing DevMap, diagnostics, task and status UI
work. Those changes were copied into an isolated worktree; they are inherited
work, not improvements authored by this audit.

Candidate: `/Users/bharath/.codex/worktrees/git-client-release/GitPulse`, branch
`codex/git-client-major-release`. Evidence logs and the original snapshot are
in the adjacent `../evidence` directory. The original checkout is retained.

**Verified scope:** repository ownership/index inspection; critical Git mutation
and recovery paths; selected-file commit semantics; cloning; stash IPC and UI;
policy argv fidelity; async repository/model switching; declared local checks;
and dependency advisory scans. Existing integration/stress suites exercise the
wider graph, diff/conflict, watcher, process, terminal, storage, coverage,
worktree, IPC, sidecar and desktop surfaces.

This is a repository-wide verification campaign with focused source audits,
not proof that every line, filesystem, Git extension, external client, provider,
or operating system is defect-free. Test coverage measures the instrumented
code and fixtures, not universal behavior.

## Reproduced defects and implemented improvements

| Priority | Before | Candidate behavior | Regression evidence |
| --- | --- | --- | --- |
| P1 | A missing branch could fall back to checking out a same-named file, discarding its edits. | Checkout fallback explicitly selects revision mode. | `release_missing_branch_never_discards_a_same_named_file`; pre-fix core log. |
| P1 | Selected-file commits included unrelated staged work and glob-shaped names could widen selection. | NUL-delimited literal pathspecs and `commit --only` preserve unrelated staged/working content. | Selected-commit and literal-filename tests in `git_ops.rs`; pre-fix core log. |
| P1 | Concurrent clones shared a destination; one failure could remove another attempt's `.git`. Eight simultaneous attempts all failed in the reproduction. | Private staging per attempt and atomic publication without replacement; cleanup only owns its staging directory. | Eight contenders, three rounds, 256 source files; `pre-fix-clone-race.log`. |
| P2 | Unstaging before the first commit failed because staged restore required HEAD. | Path-limited index reset works on unborn and existing branches. | `release_unstage_before_first_commit_preserves_working_file`. |
| P1 | Partially staged files disappeared from unstaged lists and stage-all. | Shared side classification, side-specific churn, both diff/sidebar rows, explorer actions and native menu enablement preserve remaining edits. | Three failing frontend tests plus a real Git mixed-status regression. |
| P2 | Unmerged index entries were labelled staged and could appear ready to commit. | Conflict status is kept out of the staged set until resolution. | Real divergent-branch merge regression failed before the flag correction. |
| P2 | Unstaging a rename left its original path staged as a deletion. | Individual and bulk UI selections include both rename paths. | Two failing store regressions and `pre-fix-rename-real-git.json`. |
| P2 | The empty-message validation rejected amend without changing its existing message. | Explicit empty amend reaches Git's existing no-edit path; blank normal commits remain rejected. | Empty-amend regression in `git_ops.rs`. |
| P2 | Bulk actions made a separate IPC call and lock acquisition per file. | One request, one shared mutation lock, bounded chunks, all policy plans judged before writes. Partial failures report the completed prefix. | 1,025 paths / nine Git chunks; policy failure, invalid path, deduplication and index-lock tests. |
| P2 | A failed bulk operation stopped showing activity before recovery refresh completed. | Activity stays visible until success or failure recovery finishes. | `pre-fix-batch-recovery-activity.log` and retained store regression. |
| P2 | Stash list and preview caps disappeared at the IPC boundary. | Typed listing/preview envelopes retain truncation; the UI displays it and treats incomplete stash state conservatively. | 501-entry stack and oversized preview regressions; wire-contract tests. |
| P2 | Repeated stash OIDs collided as UI keys, and restoring an entry lacked an inline preview. | Position plus OID identity; guarded, read-only preview in the existing repository panel. | Chrome/WebKit duplicate, preview, limit and late-reply assertions. |
| P2 | Changing the shared model could leave an open task editor stuck with the old configuration error. | Model selection reloads configuration; stale replies are discarded and a pending change triggers a follow-up load. | Task browser regression failed before the implementation fix. |
| P1 | The macOS bundle built successfully but lacked a sealed resource signature. | Tauri now signs the entire bundle with an ad-hoc identity by default; an explicit release identity overrides it. | `bundle-verification.json` captured the pre-fix codesign failure; final bundle verification records the sealed result. |
| Feature | Stash save lacked inline message/options. | Named stashes with include-untracked and keep-index controls, sharing one argv definition with policy evaluation. | Real Git option semantics, Tauri IPC and browser form tests. |

## Contracts and security impact

- Index requests: at most 20,000 input paths, 4 MiB total path bytes and 4,096
  bytes per path; at most 128 paths and a 12 KiB argument estimate per chunk.
  Validate all paths and judge every exact argv before executing any chunk.
  A later Git failure can leave a completed prefix: the UI refreshes and reports
  that fact. This is not a multi-command filesystem transaction.
- Unmerged conflict entries are never marked staged. Partial staging retains one
  repository-wide row and exposes independent index/working sides to views.
- Path arguments are literal. Ordinary unstaging and selected commits preserve
  unrelated staged content; rename selections include both names.
- Clone publication never replaces an existing entry. macOS/Linux use atomic
  exclusive rename; Windows uses no-replace `MoveFileExW`. Unsupported platforms
  or filesystems fail closed. Unix staging starts with mode 0700. Failed cleanup
  identifies the exact retained private directory for manual recovery.
- Conflict recovery reuses the same publication owner while preserving its
  pinned parent/ancestor protections and Windows verbatim path conversion.
- Stash lists expose up to 500 entries and a 2 MiB read cap; previews have an
  8 MiB cap. Missing/malformed listing data is a failed read, not an empty stack.
  Destructive stash actions retain the existing expected-OID guard.
- macOS packaging defaults to an ad-hoc bundle signature, sealing embedded binaries
  and resources. It grants no new trust or entitlements. `APPLE_SIGNING_IDENTITY`
  can select a configured release identity; this run uses no certificate or keychain secret.
- **Security impact:** no new dependencies, privileges, capability grants,
  authentication paths, network destinations or credential access. The existing
  policy gate and its explicit unavailable/not-installed semantics remain in
  force. Batch policy checks judge the same command plans the writer executes.
  Filesystem publication narrows overwrite risk; it does not expand access.
- Repository mutation locks coordinate GitPulse writers in this process and
  linked worktrees. External Git clients remain concurrent; Git's own locking
  and explicit failures still matter. Hooks and external helpers are not made
  transactional by this release.

Git behavior was checked against the installed Git help and official
[reset](https://git-scm.com/docs/git-reset),
[checkout](https://git-scm.com/docs/git-checkout),
[commit](https://git-scm.com/docs/git-commit), and
[stash](https://git-scm.com/docs/git-stash) documentation. Publication uses the
[Windows MoveFileExW contract](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-movefileexw)
and [Linux rename contract](https://man7.org/linux/man-pages/man2/rename.2.html).
Signing configuration follows [Tauri macOS signing](https://tauri.app/distribute/sign/macos/)
and its [environment override contract](https://v2.tauri.app/reference/environment-variables/).

## Verification record

Baseline: 5,925 frontend tests passed with one skipped. The declared CI gate
failed in the task browser harness. Its obsolete UI selectors were corrected,
then the actual model-change recovery failure was reproduced and fixed. Five
core Git regressions and two stash-envelope regressions failed before their
implementation fixes. The initial selected-commit test used an invalid `HEAD`
argument in an OID-only reader; it was corrected and rerun against unchanged
production code before attributing that failure to the product.

**Verified full gate:** `npm run ci:local` passed. The complete result is in
`../evidence/release-ci-complete.log`; the final repetition after the packaging
configuration change also passed in `release-ci-exact-final.log`.

| Check | Verified result | Evidence in `../evidence` |
| --- | --- | --- |
| Frontend suite | 449 test files, 5,947 passed, one explicitly skipped | Full CI log |
| Native suite | 65 targets, 2,292 reported passes, zero failures, 14 explicitly ignored | `native-ci-summary.json`; opt-in caveat below |
| Coverage | Frontend lines 96.15% (11,231/11,681), branches 89.07% (10,905/12,243); Rust lines 84.48% (60,842/72,019) | Final full CI log and generated LCOV reports |
| Contracts and build | IPC, wire types, release versions, workflow checks, Svelte/TypeScript, production frontend build, Rust formatting and Clippy passed | Full CI log |
| Rendered browser matrix | Ten harnesses in each of Chromium and WebKit; all 20 runs and 1,088 assertions passed | `final-browser-matrix.json` and its individual logs |
| Native stress repetitions | Seven suites, 122 tests each at one, four and eight test threads: 366 successful executions | `stress-rounds.json`, `stress-round-*.log` |
| Deep graph fuzz | 6,800 generated histories, including 800 histories of 600 nodes and 2,000 with ghost parents | `deep-graph-fuzz.log`; `lane_solver_fuzz.rs` seed loops |
| Real repository qualification | Five graph/layout checks and one Pulse report check passed | `real-repository-qualification.log` |
| Required Manvi integration | Five tests passed with missing-sidecar skipping disabled | `manvi-required-integration.log` |
| Actual DevMap store | Two checks passed with the isolated repository explicitly selected | `devmap-real-store-integration.log` |
| DevMap CLI isolation | The installed schema-20 CLI produced two isolated maps readable by the native embedding | `optional-devmap-cli.log` |
| Additional local integrations | Disposable Go cache cleanup, installed agent CLI help, document refresh and read-only upstream update check passed | `optional-local-native.log` |
| Native task profile | Passed with the candidate's compiled `dcstore`; installed `dcstore` failed protocol compatibility | `candidate-store-native-profile.log`; mismatch details below |
| Windows/Linux compilation | Shared publication and conflict-filesystem modules compile for x86_64 and ARM64 on both platforms | `filesystem-crosscheck/results.json`; this is not a whole-app cross-build or runtime test |
| Dependency advisories | Zero known npm and Cargo advisories at scan time | `npm-audit.json`, `cargo-audit.json` |
| macOS artifact | Version 0.2.0, ARM64 app and three embedded tools; strict recursive codesign verification passed | `bundle-verification-final.json`, `release-bundle-sealed.log` |

Native totals are the final result for each test target, without double-counting
child-process test summaries. Some opt-in tests return early when their required
environment is absent, so reported passes alone do not establish live provider
coverage. Manvi and the actual DevMap store were separately exercised with
explicit required inputs. Deep fuzz and real repository checks were explicitly
run despite being ignored by the ordinary suite. Thirteen of the 14 ordinary
ignored tests were exercised separately; the remaining managed-agent test sends
a live model turn and was not run. Live local-AI completion tests were also not
enabled. These are open provider gates, not passes.

The real repository Pulse run processed 411 commits. Its knowledge scan covered
128 of 1,970 files and explicitly reported truncation; it is a bounded sample.
The batching test verifies 1,025 paths use nine Git invocations within one IPC
request. That is a verified reduction in calls and lock acquisitions, not a
measured claim about interactive latency on every repository. Documentation
refresh measured a 16.9 ms median and 33.4 ms maximum across 15 unchanged
refreshes of 100 disposable notes; these timings describe this machine only.

The first full post-edit native run exposed a legacy `FileStatus` deserialization
failure introduced by required side-count fields. The final fields are optional:
current native reads emit measured counts, and older payloads retain absence
instead of fabricated zeroes. The retained compatibility test and complete
rerun passed. Earlier failed logs are preserved as evidence, not overwritten.

The diagnostics browser fixture deliberately injects errors to test their
classification. Those expected log lines are not crashes of the final app.
Existing vendored framework warnings remain visible; the project's declared
checks were not weakened to suppress them.

**Verified machine compatibility gap:** the installed
`/Users/bharath/.local/bin/dcstore` rejects Manvi's `work --method` protocol with
`unknown flag --method`. The opt-in worker test paused with that diagnostic,
rather than executing a stale task. Building the existing vendored package with
`cargo build --manifest-path src-tauri/Cargo.toml --locked -p dc-store --bin dcstore`
and rerunning with its output passed the shared-profile/revision test. The
compatible local binary is `src-tauri/target/debug/dcstore`; no dependency was
added and the installed tools were not replaced. Qualifying or updating the
installed companion is required before relying on task workers on this machine.

## Local artifact and review handoff

The built application is `src-tauri/target/release/bundle/macos/GitPulse.app`.
Its manifest records the version, architecture and SHA-256 of `gitpulse`,
`gitpulsed`, `gitpulse-mcp` and `gitpulse-hook`. `codesign --verify --deep --strict`
passes and reports sealed resources. `spctl --assess --type execute` rejects
this ad-hoc artifact: it has no trusted distribution identity or notarization.
The local ZIP is `../artifacts/GitPulse-v0.2.0-macos-arm64-local.zip`.
`local-artifact-manifest.json` records its hash and verifies that archive
extraction preserves all four binary hashes and the valid resource signature.

`../evidence/release-only.patch` contains this audit's changes relative to the
captured dirty baseline. `release-only-stat.txt` separates that work from the
inherited changes. The candidate is intentionally uncommitted. The original
checkout's HEAD, branch and 94 initial file hashes are checked again in
`original-preservation-check.json`; review artifacts use temporary index files
and do not stage the developer's work.

## Evidence limits and release gates

- DevMap was built in this worktree and queried for ownership, impact and
  affected tests. Bounded walks and dynamic IPC leave incomplete graph evidence;
  source inspection and full suites supplement it. A capped walk is not a
  complete caller inventory. Seven SQL files lack import extraction.
- macOS is the local runtime platform. Windows/Linux runtime qualification,
  external filesystems, hardware failures and hostile power-loss scenarios
  require their respective environments. Cross-platform CI configuration is
  not evidence that those jobs ran.
- Browser harnesses exercise rendered Svelte/DOM behavior with controlled IPC;
  real Git tests and Tauri mock-runtime bridge tests independently cover their
  respective layers. They do not establish an installed application's complete
  physical UI behavior or live hosted-provider behavior.
- Authenticated remotes, live LLM providers, trusted signing, notarization,
  update distribution and production release publication remain external gates.
  Dependency scans report known advisories at scan time, not absence of all
  vulnerabilities.
- No push, tag, merge, production deployment or installed-app replacement is
  part of this local candidate. Future release execution must rerun the declared
  checks on the final merged tree and verify exact artifacts before publication.
