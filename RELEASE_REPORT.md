# GitPulse v1.5.0 — release preparation report

Prepared 2026-10-17 on `main`. **PUSH: no.** Nothing was pushed, no tag was created, and nothing was published.

**State:** `main` @ `cc33ddc8` is release-ready on this machine: 18 of 18 `ci:local` gates passed on that exact commit. It is 846 commits ahead of `origin/main`. The release workflow will refuse to build until `ci.yml` and `coverage.yml` are green **on GitHub** for the tagged commit, and origin CI has been red since 2026-10-02 (ft-f8a297ff). So the push sequence at the end of this report waits for that before tagging. This report is left uncommitted on purpose: committing it would move `main` off the commit that `ci:local` verified, and the tag must be that commit.

Labels: **verified** = checked here, with the evidence named. **inferred** = follows from what was read, but not run. **unverified** = not checked.

---

## 1. Orientation

- **gitpulse_insights:** 3 worktrees, 0 dirty, 0 contended files, one live agent session (this one). No other lane held a file this release touched. Verified.
- **Unpushed work:** `main` is 846 commits ahead of `origin/main`. When the work started it was 842; the four after that are this release's (section 2). Verified with `git rev-list --count origin/main..main`.
- **The last published release is v1.3.1** (`gh release list`). Local tags that never reached origin:
  - `v1.3.5` (`3d21d7f6`)
  - `v1.4.0` (`ed54ac18`)
  - `v1.2.1`
  - eleven `safety/v1.3.5-…` and `safety/v1.4.0-…` tags

  `v1.5.0` exists neither locally nor on origin. Verified with `git tag -l` and `git ls-remote --tags origin`.
- **Owner-decision card `ft-aa3086e6`** settles two things, and both were followed:
  - Push order is DevCouncil → Manvi → gusset → GitPulse.
  - Agents never touch tags. No tag was created, moved or deleted.

  Its other items remain open; the v1.5.0 ones were added to it (section 4c).

## 2. Commits made

| Commit | What | Task |
|---|---|---|
| `63c2c5f6` | fix(terminal): the bypass agreement takes clicks again under the WebGL renderer | — (regression in unreleased code) |
| `f7028c94` | docs(reports): the branch report cites no /tmp log as evidence | — (a `ci:local` failure) |
| `1603c69d` | ci(workflows): pin every action to a commit and drop checkout credentials | ft-ed5d43e4 |
| `d414ad3f` | fix(updates): a release probe that could not be answered is an error, not an unpublished release | ft-d37c7225 |
| `cc33ddc8` | docs(release): 1.5.0 notes cover everything since 1.3.1; document rolling a release back | — |

- **Pending work at the start:** there was none. The working tree was clean, so Phase 2 committed only `63c2c5f6`, the dialog fix this session made.
- **Impact checks:** run with DevMap `devmap_impact` and `devmap_affected_tests` on `http_head_ok`, `curl_head_args` and `release_is_published`. The direct tests are the `tool_install::release` and `updates` modules and `tests/updates_integration.rs`. The walk stopped at depth 3, so that list is a lower bound; the full `ci:local` covered the rest.
- **Tests that failed before each fix:**
  - `63c2c5f6`: harness phase `runBypass` failed with "the pointer lands on CANVAS.xterm-link-layer".
  - `1603c69d`: three checker tests failed with "is not a function".
  - `d414ad3f`: four tests failed to compile (`head_verdict` missing), and the args test refused `-fsI`.

## 3. ci:local

**Deviation from "ci:local after each fix commit":** each fix commit was followed by the gates that change touches, not the whole 18-gate run. One full run takes about 65 minutes. The full run is on `cc33ddc8`, the final HEAD, which contains every one of the fix commits.

- `f7028c94`: `documented-counts-contract` (it failed before; afterwards 20 passed, 1 skip).
- `1603c69d`:
  - `check:workflows` (actionlint plus the new rules);
  - `check-workflows`, `release-workflow` and `ci-local` tests (41 passed).
- `d414ad3f`:
  - `tool_install::release::` and `updates::` (40 passed, 1 ignored);
  - `updates_integration` (15 passed);
  - `cargo fmt --check`;
  - `cargo clippy --all-targets -D warnings`.
- `cc33ddc8`:
  - `release:notes --tag v1.5.0`;
  - `check:release --tag v1.5.0`;
  - the doc and release contract tests (67 passed, 1 skip).

Two changes in `1603c69d` only take effect on GitHub's runners, so their inputs were checked at the pinned commits:
- `setup-bun@0c5077e5` reads `packageManager` (`split("bun@")`) from `bun-version-file: package.json`. Bun 1.4.2 is a published release, and it matches the local `bun --version`.
- `install-action@8cca0918`'s `manifests/cargo-cyclonedx.json` lists 0.5.9.

### Final run: `cc33ddc8` — 18 passed · 0 failed · 0 skipped, exit 0

| Gate | Result |
|---|---|
| Vendored crates unedited · vendored schema vs installed devmap · actionlint (+ the new pin and credential rules) · no fixture-authored commit · IPC contract · coverage type contract · release version gate | PASS |
| Frontend Svelte and TypeScript check | PASS |
| Frontend unit tests with coverage | **10,608 passed, 1 skipped**, 761 files |
| Browser regressions, headless Chrome | **2,839 checks over 34 pages**, all passed |
| Native renderer regressions, WKWebView | **2,839 checks over 34 pages**, all passed |
| Frontend Vite build · Rust format · clippy `-D warnings` | PASS |
| Rust tests with coverage | **6,100 passed, 0 failed, 45 ignored**, 121 test binaries |
| Terminal suites against the terminal host | **92 passed, 0 failed, 1 ignored**, 7 binaries |
| Gusset engine check | PASS (parity, match-any, panic firewall) |
| Coverage floor | frontend lines 96.74%, branches 90.28%; Rust lines 90.00% (floors 90 / 85 / 80) |

- **Every test gate executed tests.** The counts above are read from the run's log.
- **Clippy finished in 0.8 s.** That is a warm cache: no `.rs` file changed after an explicit clean `cargo clippy --all-targets -- -D warnings` on `d414ad3f`.
- **The `env.rs` panics in the log are deliberate.** They come from `a_failing_test_cannot_leave_its_env_override_installed`, which runs a failing test on purpose and reports `ok`.

### First run: `63c2c5f6` — red

1. **Real:** `documented-counts-contract` → "cites no /tmp log as evidence", because `BRANCH_REPORT.md` cited a worktree's tmp log. Fixed in `f7028c94`.
2. **Host fault:** two `vite-watch` tests (`control reload never observed; log: []`) and ten Rust `watcher::` tests failed.
   - **Cause:** `fseventsd` was stuck at 100% CPU and 13.8 GB resident, after 5 days of uptime.
   - **Probe:** a bare `fs.watch` probe saw 3 of 5 single-file writes never reported within 20 s.
   - **After the restart:**
     - the same probe reports every write in about 10 ms;
     - `vite-watch` passes 3 of 3 runs;
     - the watcher tests pass 86 of 86 in the lib (2 `#[ignore]` by design), plus the adversarial storm test;
     - the final full run passes.
   - **Nothing was marked passed or changed to get there.**

## 4. Release-pipeline audit

Four read-only Sonnet auditors covered:
- version and changelog;
- workflow security;
- the update-check contract and rollback;
- reproducibility and bundles.

Opus checked their claims against the files and live commands. The advisor decided fix-now versus file, by one rule: fix now only what a local check can prove before the release. The auditors had no shell, so every claim below that needed one was re-run here.

### 4a. Fixed

| Finding | Evidence | Fix |
|---|---|---|
| Six workflows used tag/branch-pinned actions. `release.yml` ran `dtolnay/rust-toolchain@stable` 3×, once in the `contents: write` attest job. | Verified with `rg` over `.github/workflows`. Every SHA now in place was checked against `git ls-remote` for its tag on 2026-10-17. | `1603c69d`, with `check:workflows` enforcing it |
| 11 checkouts kept the job token in `.git/config`. | Verified per file. No step in those workflows pushes, fetches or calls `gh`. | `1603c69d` |
| Bun version floated in every workflow. | `setup-bun` had no version input. Its action.yml (at the pinned SHA) has `bun-version-file`. | `1603c69d`, `bun-version-file: package.json` (bun@1.4.2) |
| `cargo-cyclonedx` (release SBOM) unpinned. | `release.yml` attest job. | `1603c69d`, pinned to 0.5.9 (crates.io max stable) |
| The update check read any failed HEAD (DNS, timeout, 403, 5xx) as "not published", then said "up to date". | `http_head_ok` returned `Ok(run.success)`. Measured live: with `-f` a GitHub 404 exits 56, not 22, so exit codes cannot classify. | `d414ad3f`: the status comes from `-w`. 2xx means published, 404/410 means absent, anything else is an error. Checked live: existing asset 200, missing 404, bad host 000. |
| The release body (`[1.5.0]` only) told a 1.3.1 user nothing of the never-published 1.4.0 work or its upgrade steps. | `release-state.mjs` `extractNotes(CHANGELOG.md, tag)` | `cc33ddc8`: the section opens with that pointer |
| `[1.5.0]` compare link started at `v1.4.0`, which is not on origin. | `git ls-remote` | `cc33ddc8`: now `v1.3.1...v1.5.0` |
| No rollback procedure. Demoting a release to pre-release silently does nothing. | The check only HEADs the `SHA256SUMS` asset and never reads the prerelease flag (`updates/mod.rs`). | `cc33ddc8`: CONTRIBUTING → "Releasing and rolling back" |
| The `4a8b4eb0` release-job hardening was missing from the changelog. | — | `cc33ddc8`: `[1.5.0]` Security |

### 4b. Filed — added to `ft-ed5d43e4` (revision 7)

Criteria were added for each, with evidence:
- **No bundle-content assertion.** `verify` checks installer names and checksums only. A bundle missing `gitpulse-hook`, `gitpulse-mcp`, `gitpulse-ptyd` or `gitpulsed` would ship as a green draft.
- **The shipping build is not `--locked`.** Only preflight's `cargo test` and a post-hoc `Cargo.lock` diff hold the lock.
- **No build provenance attestation.**
- **Still open from the card's own criteria:** the tauri-action build runs with `GITHUB_TOKEN` in its env; there is no `rust-toolchain.toml`; the actionlint install has no checksum.

Criteria 7 and 8 (pin checker, credential checker) are met by `1603c69d`.

### 4c. Needs your decision — added to `ft-aa3086e6` (revision 2)

- **Push order** DevCouncil → Manvi → gusset → GitPulse still stands. GitPulse CI's Go job pins DevCouncil `3b58dd7c` and gusset `92c20560`, and both exist on their remotes (verified via the GitHub API). So GitPulse CI does not wait on those pushes for its checkouts.
- **Local-only tags:** `v1.3.5`, `v1.4.0`, `v1.2.1` and 11 `safety/*` tags. Keep, delete or push? Pushing `v1.4.0` or `v1.3.5` would fire `release.yml`.
  - `[1.4.0]`'s compare link 404s while `v1.4.0` is not on origin.
  - Historic mismatches: `v0.1.0` is on origin with no release, and `[0.0.7]` has a section but no tag.
- **Signing:** macOS is ad-hoc (`signingIdentity: "-"`) and not notarized, and Windows is unsigned. That needs Apple Developer ID and notarization secrets plus Windows signing, or a decision to ship unsigned.
- **Build/upload split,** so the build never holds a write token. This drops tauri-action's `releaseId` upload.
- **Rust compiler pin policy** (`rust-toolchain.toml`).
- **Tag protection / rulesets / an immutable or gated publish:** repository settings.
- **Old clients see the tag early.** Installed builds older than `cf0fe90f`, including the published 1.3.1, announce the newest tag from `ls-remote` without checking for a release. Opted-in 1.3.1 users will see v1.5.0 the moment its tag is pushed, before the draft is published. This can't be fixed retroactively.
- **Static plugin PATH (ft-c3b428e6):** the bundled `plugin/` manifests still use bare binary names.

### 4d. Audited and sound (no change)

- **Versions:** all 8 version sources read 1.5.0: `package.json`, `Cargo.toml`, the one `gitpulse` `Cargo.lock` entry shared by the 5 `[[bin]]`s, `tauri.conf.json`, and 4 plugin manifests. `bun run check:release -- --tag v1.5.0` prints "all version sources agree on 1.5.0 and match tag v1.5.0". Verified.
- **Triggers:** `release.yml` fires on `tags: ['v*']`, so a `safety/…` tag does not fire it (inferred from GitHub's filter semantics). `check-release-version.mjs` rejects any tag but `vX.Y.Z`. Verified.
- **Permissions:** top level is `contents: read`. Write is granted only to `prepare`, `release`, `attest` and `verify`. Every job has a timeout, and concurrency is keyed on the tag. Verified.
- **Draft-first:** the release is created as a draft and never published by the workflow. `verify` fails the run unless the exact asset set is present (12 installers, `GitPulse_1.5.0_SHA256SUMS.txt`, and SPDX + CycloneDX SBOMs). Verified by reading. A real tag has never exercised the SBOM/checksum path (unverified until the tag build).
- **Update-check contract:** a version is announced only once its `SHA256SUMS` asset answers an anonymous HEAD. A draft's assets do not, so clients cannot see 1.5.0 before it is published and its checksums exist. Verified by reading `updates/mod.rs` and its tests, and live: a HEAD on a real v1.3.1 asset follows the 302 and returns 200.
- **Reproducibility:**
  - `bun.lock` and `Cargo.lock` are committed, and preflight uses `--frozen-lockfile` and `--locked`.
  - Every Cargo `path =` dependency is inside the repo; `framework/` and `vendored/` are tracked (815 files). Out-of-repo paths exist only in `go/go.mod`, outside the app build.
  - `documented-counts-contract` skips when the gitignored `PROMO.md` is absent.

  A clean-clone build was not run (unverified).
- **Platform matrix:** five runners (macOS universal, Ubuntu 22.04 x64/arm, Windows x64/arm) produce 12 installers. A local `bun run tauri build --bundles app` of `63c2c5f6` produced all five binaries in `Contents/MacOS`. Windows and Linux bundle contents are unverified.

## 5. Release notes (v1.5.0)

The grouped notes below are `docs/RELEASE_NOTES_v1.5.0.md`. Every bullet names the commit that implements it: 95 distinct hashes, all in `v1.3.1..HEAD`, and a ten-hash sample checked against `git show`. The signing known issue cites the config line instead.

The GitHub release body is produced by the workflow from `CHANGELOG.md`'s `[1.5.0]` section (`bun run release:notes -- --tag v1.5.0` prints it). That was updated in `cc33ddc8` to match.

Claims dropped because no implementing commit states them:
- measured figures that appear only in the changelog;
- "Option-click selects inside mouse-reporting programs";
- per-CLI model details from an untested wip snapshot;
- features inside the broad squash commit `9706f20d`.

The `63c2c5f6` dialog fix is left out because it fixed code that never shipped.

GitPulse 1.5.0 is a large release: agents that run in the background with a pane of everyone at work, a much richer task board, pull-request review, repository-wide search, and a lot of work on terminal reliability and idle cost. It covers everything since v1.3.1, the last published release, because v1.3.5 and v1.4.0 were never published and their changes are included here. To upgrade, install the 1.5.0 build, then read Breaking changes below: the task profile is migrated one way, and `gitpulse-mcp` and `gitpulse-hook` need reinstalling.

## Highlights

- **Agents start in the background.** Launching an agent from a task no longer closes the task sheet. The task's Agents pane lists every attempt working on it, and board cards show how many agents work on them and how many need you. (`576e005c`, `389a3498`)
- **Several agents at once, each in its own worktree.** You choose how many agents run at once (Settings, Agents, default 8) and how many terminal sessions may be open (default 32, up to 128). An agent can mark its own task done when the work is finished. (`a16e9171`, `5e547be5`, `421b4404`, `acd6116c`)
- **Stacked worktree tabs.** Every checkout of one repository shares one tab with a switcher, so agent worktrees stop pushing other repositories off the tab strip. (`88ae7ae9`, `d127042c`)
- **A model for each agent.** Settings, Agents, Agent models sets the default model per agent CLI, and a task handoff can pick a model, effort and advisor for a single launch. Every handoff now opens with the same agent guidance. (`16c79e71`, `35ea0410`, `c68e9b3a`)
- **A fuller task board.** Undo, swimlanes, saved views and work-in-progress limits, checklists and task links, restore from a Deleted view, a query language for search, and Suggest next. "Run in waves" launches the top suggested tasks in their own worktrees and merges only work with a passed verification, never pushing. (`389a3498`, `cb4ab882`, `3a7de123`, `f0c78a46`, `69d91131`, `d7545e04`)
- **Agents can delete and merge board tasks, and must group findings.** `gitpulse_delete_task` and `gitpulse_merge_tasks` exist, and `gitpulse_add_task` refuses a new card that looks like open work until the agent has read the related cards. (`f9d4fc9d`, `0f0ffbda`, `607ad2da`)
- **Pull requests, search and file operations.** A pull request can be viewed, reviewed, merged and opened from GitPulse through `gh`. Code, Search searches file contents. The file tree renames, moves and deletes tracked files. (`ff0e7c40`, `bcf18867`, `d5f159df`)
- **More history tools.** From a reflog entry you can create a branch or reset the current branch. Blame can show a file at a revision and step to a line's parent commit. (`3c27d41c`, `75e393ad`)
- **Two agents in one worktree are visible.** Collision risk now flags dirty files in any worktree two live sessions share. Codex keeps no session registry, so it is reported as unknown. (`2c3d6707`)
- **Dead-branch cleanup and a Secrets scan.** Stale branches are scanned and cleaned with a restorable backup per repository. An optional secrets scan locates each finding and never calls a partial scan clean. (`9706f20d`, `a20f8d58`, `86fdf28c`)
- **Agents keep running when their view goes away.** A stalled, hidden or reloaded view no longer kills an agent. On macOS, Cmd-Q, Dock Quit and the Quit Apple event reach the quit confirmation, and a second launch hands over to the running GitPulse. (`fe6236ef`, `8c9feada`, `998c43a2`, `b862aa28`)
- **Optional terminal host.** Settings, Agents, "Keep terminals running when GitPulse quits" (off by default) holds terminals in a separate `gitpulse-ptyd` process so they survive a quit or crash and are adopted on the next launch. (`9d693711`, `6ca14b3a`)
- **Git work is admitted by class.** Interactive, reactive and background git work are admitted separately, and all GitPulse processes share one per-user spawn budget, so a refresh storm can no longer starve an action you asked for. (`e9a64486`)
- **A host-load governor.** Under severe memory pressure GitPulse refuses agent launches with a clear reason and sheds its own background work. Your shell and stop, kill, cancel, save and commit are never gated. Health shows the state and offers **Admit anyway** (two minutes, after a native confirmation). (`73163a50`, `e96541bb`)
- **Lighter at idle and with many tabs.** A repository is read one snapshot at a time, switching back to a freshly read tab costs no read, open tabs keep history for only the 6 most recent repositories, terminal scrollback is a setting, and idle watchers, the single-instance mailbox and idle terminals wait for events instead of polling. Switching tabs no longer rebuilds the sidebar or status bar. (`296eb20b`, `5b419492`, `7dc5c132`, `3d1c9bae`, `05e734a7`)
- **GPU terminal rendering, on by default.** Visible terminals draw through WebGL (at most 8 at once) and fall back to the DOM renderer after a lost context. Settings, Appearance, GPU terminal rendering turns it off. See Known issues. (`75bee0f9`)
- **Ledger rows record when things happened,** not when a catch-up pass saw them, and the journal shows event time. (`8221b353`, `c71e8daf`)
- **Manvi proposals and verification verdicts.** A Proposals panel reviews Manvi's scan and refine proposals. A task's verdict can be posted to GitHub as a `devcouncil/verify` commit status, per repository and opt-in. (`cf07ebc2`, `03ca87ad`)

## Fixes

- **A slow host no longer kills terminal input.** Input waits for the real answer, shows one "responding slowly" notice, and stops only when the session is gone or nothing answers for 60 seconds. (`2e313355`)
- **Typing and resizing no longer queue behind git.** Terminal commands run on their own bounded executor instead of the pool git shares. (`e2c11933`)
- **Terminal output travels on a per-session channel** of raw bytes, up to 64 KiB a message, instead of a global event sent to every webview. (`3b1cf774`)
- **A Claude tab ended by a signal offers Resume** for the same conversation, directory and mode. (`6fc50cb2`)
- **Branch names git allows now work,** such as `fix/a&b`, `feat(ui)=x` and `a]b`. A leading `-` and control characters are still refused. (`de7b53f2`, `2dba1c9e`)
- **A commit that finishes after you switch repository tabs** no longer clears the other repository's message or amend flag. (`05e734a7`)
- **Send to agent from a board card no longer crashes,** and the branch menu's Push, Pull and Checkout work again. (`dc045c1f`)
- **Remote branch rows check out as a local branch** and no longer detach HEAD. (`d7ca958e`, `ff3c9628`)
- **Open tabs and groups survive quitting, and repository trust survives reboots** and toolchain updates. (`c6214518`, `a3323787`)
- **More than 24 repositories can be open,** and agents in worktrees no longer need a repository tab of their own. (`fe4393d3`, `edb364c1`)
- **Closing a working agent asks first,** and names its task. (`e27e6daf`)
- **Removing or merging a worktree refuses while any process is working in it.** (`438a21e2`)
- **The file editor no longer rewrites non-UTF-8 or mixed line-ending files.** Saves are compare-and-save with a conflict banner, and New File and New Folder create only. (`a4149eb1`, `30e30fdb`, `4c34c5c4`)
- **A partial scan is not reported as complete.** Dependency and coverage scans return by one 90-second deadline and a cut coverage report is marked truncated. A secrets scan that could not read a file is not "clean". (`1b91f9a6`, `86fdf28c`)
- **One failing GitHub alert tier no longer hides the others.** The failing tier is named and Health shows the result as incomplete. (`3cafbd11`)
- **A file-system event with an odd path or a panic in the handler no longer aborts GitPulse on macOS.** (`4d4c04d2`)
- **The update check announces published releases, not tags,** and a check that cannot be made fails rather than guessing. (`cf0fe90f`)
- **An update check that could not reach GitHub no longer reports "up to date".** A failed lookup, timeout, refusal or server error used to read as "not published"; now only a "not found" does, and anything else fails the check with its reason. (`d414ad3f`)
- **Closing a tab group releases its tabs** the way closing a single tab does. (`8b47801d`)
- **A moved or renamed repository takes its ledger over** instead of refusing to record. (`fbe3e99b`)

## Hardening

- **The app enforces an ACL on its own commands.** The main window gets only the commands the frontend calls, the status popover only its three, and webviews cannot navigate off the app origin. (`08607bfd`)
- **Trust and tool changes ask in a native dialog the page cannot answer.** Trusting a repository, and repointing devmap or manvi at a new binary or source root, need a native confirmation. Open-in-default-app requires a trusted repository and confirms executables. (`87d03fc8`)
- **Remote images in repository Markdown stay withheld until you click them,** and load over https only. (`08607bfd`, `a047354d`)
- **Remote tool installs are pinned.** They build one exact commit from a fixed directory, curl is confined to https, and clone refuses Git older than 2.45.1 (submodule recursion older than 2.50.1). (`3f09e029`)
- **Every signal GitPulse sends goes through one guarded primitive** that refuses pid 0, 1 and negative ids, its own process, and a pid whose start time moved. (`0709eded`)
- **Agents' kill commands are checked.** The command gate refuses a `pkill`, `killall` or similar that would reach agent sessions, GitPulse or more than 25 processes. (`8c03e89e`)
- **Previews and saves refuse oversized content at the command boundary,** before any copy is made. (`2105086c`)
- **Copying build caches into a worktree is all or nothing,** and only into a worktree of the same repository. (`80bd78c4`)
- **A peer that drips bytes cannot hold the alert bridge or the code-index probe open.** One wall-clock deadline covers both. (`c4155156`)
- **The shared spawn budget no longer resets to a full burst when its record is unreadable.** (`6ab65c15`)
- **Merging an agent attempt nobody reviewed is refused,** behind a per-repository opt-in. (`03b6b700`)
- **The file sandbox refuses reads and writes of `.git` metadata** from the webview. (`30e30fdb`)
- **A build that would ship source maps fails,** and the release job no longer runs install scripts with the GitHub token or persists checkout credentials. (`b9b9e116`, `4a8b4eb0`)
- **Every workflow pins its actions to commits.** No checkout keeps its token, Bun's version comes from `package.json`, and `check:workflows` refuses an unpinned action or a credential-keeping checkout. (`1603c69d`)

## Breaking changes

- **The task profile is migrated one way, to dc-store workbench schema 13.** A host built against an older schema refuses a migrated profile. Rebuild GitPulse, Manvi and DevCouncil from the same dc-store before any of them opens the profile. (`3a7de123`, `c6b92930`, `6c0e1faa`)
- **Reinstall `gitpulse-mcp` and `gitpulse-hook`** (`bun run mcp:install`). An older binary carries an older `devmap-store` and cannot read a store a current `devmap` has built. `bun run check:schema-parity` asks every installed reader which schemas it reads. (`710aa9e0`, `650a587a`)
- **Child processes drop relative `PATH` entries** (`.`, `bin`, `./node_modules/.bin`). A setup that relied on one must use an absolute path. The Manvi sidecar starts from an allowlisted environment, so provider keys and tokens set in the launching shell no longer reach it. (`87d03fc8`)
- **An agent can no longer mark a task done on its own word.** `gitpulse_complete_task(done)` leaves the task in review unless Manvi recorded a passed verification for the worktree's current head. A person moving a card is unaffected. (`50eedd43`)
- **`gitpulse_add_task` refuses a new task that looks like open work.** An agent must fold into a listed card, or call again naming every listed `item_id` in `reviewed_related`. (`607ad2da`)
- **Repository hooks (`.gitpulse/hooks.json` and `hooks.toml`) no longer run until you approve the file.** Any edit asks again. (`782cd07d`)
- **Clone refuses Git older than 2.45.1,** and submodule recursion refuses Git older than 2.50.1. (`3f09e029`)
- **A released app whose `gitpulse-hook` cannot be found now blocks agent calls** instead of letting them through. Make sure the hook resolves beside the app. (`32fe572c`)
- **On macOS, Option is Meta in the terminal by default.** A layout that types a character with Option (such as `@` on a German keyboard) must turn off Settings, Appearance, Option as Meta in terminals. (`d6c5a40d`)

## Known issues

- **The GPU terminal renderer shipped before its deciding benchmark ran.** It is on by default. The TUI-redraw A/B in `docs/PERFORMANCE.md` has not been run. A lost context falls back to the DOM renderer, and the setting turns it off. (`75bee0f9`)
- **Prebuilt remote installs are unavailable.** None are pinned yet, so that install option reports itself unavailable and installs build from source. (`3f09e029`)
- **Collision risk cannot see running Codex sessions.** Codex keeps no session registry, so Codex is reported as unknown rather than zero. (`2c3d6707`)
- **The macOS build is ad-hoc signed and not notarized, and Windows builds are unsigned.** Gatekeeper and SmartScreen warn on first launch. (`src-tauri/tauri.conf.json` `signingIdentity: "-"`; no signing secrets in `release.yml`.)
- **An installed 1.3.1 may announce 1.5.0 before its installers exist.** Builds older than the published-release check announce the newest tag as soon as it is pushed, so for the minutes before the draft is published that link leads nowhere. 1.5.0 itself waits for the published checksum file. (`cf0fe90f`)
- **v1.4.0 and v1.3.5 were never published.** Their changes are included in this release. v1.3.5 was withdrawn, and 1.4.0 remains the previous changelog section. People upgrading from v1.3.1 receive everything since v1.3.1. (`00ec2567`)


---

## 6. Push and tag (PUSH was "no", so none of this has run)

Decide the `ft-aa3086e6` items first, at least the push order, signing and the local tags. The order below is the one the gates enforce: `release.yml` requires a green push run of `ci.yml` **and** `coverage.yml` on the tagged commit, and `.githooks/pre-push` refuses a tag whose commit has not passed CI.

**0.** If you keep the card's order, push DevCouncil, then Manvi, then gusset first, each from its own checkout.

**1.** Push `main`. Its tip is `cc33ddc8`, the commit `ci:local` verified. Commit or move `RELEASE_REPORT.md` afterwards, not before: a new tip would need its own green CI before tagging.

```bash
git push origin main
```

**2.** Wait for both workflows to succeed on that commit:

```bash
gh run list --workflow ci.yml --commit cc33ddc8db2c89d4ee07724319d369b5ce150555
```

```bash
gh run list --workflow coverage.yml --commit cc33ddc8db2c89d4ee07724319d369b5ce150555
```

```bash
gh run watch <run-id>
```

**3.** Ask the release gate the same question without creating anything:

```bash
RELEASE_TAG=v1.5.0 RELEASE_COMMIT=cc33ddc8db2c89d4ee07724319d369b5ce150555 GH_REPO=bharathvbcr/GitPulse bun run release:ready
```

**4.** Create the annotated tag on that commit and push only that tag. Never use `--tags`, never force.

```bash
git tag -a v1.5.0 -m "GitPulse v1.5.0" cc33ddc8db2c89d4ee07724319d369b5ce150555
```

```bash
git push origin v1.5.0
```

**5.** Watch the release run. On failure, read only the failed steps, fix forward in a new commit, and do not re-tag.

```bash
gh run list --workflow release.yml --limit 1
```

```bash
gh run watch <run-id>
```

```bash
gh run view <run-id> --log-failed
```

**6.** Check the draft before publishing:
- 12 installers;
- `GitPulse_1.5.0_SHA256SUMS.txt`;
- the SPDX and CycloneDX SBOMs;
- `isDraft: true`, `isPrerelease: false`.

```bash
gh release view v1.5.0 --json isDraft,isPrerelease,assets --jq '{isDraft, isPrerelease, assets: [.assets[].name]}'
```

**7.** The workflow sets the body from `CHANGELOG.md` `[1.5.0]`, and its finalize step checks that it matches. Keep that body. To use the grouped notes instead, run this before publishing:

```bash
gh release edit v1.5.0 --notes-file docs/RELEASE_NOTES_v1.5.0.md
```

**8.** Publish promptly. Opted-in 1.3.1 users already see the tag:

```bash
gh release edit v1.5.0 --draft=false
```

**9.** Confirm the update check's signal. The checksum asset answers only once the release is published, so this should print `200`:

```bash
curl -sI -L -o /dev/null -w '%{http_code}\n' https://github.com/bharathvbcr/GitPulse/releases/download/v1.5.0/GitPulse_1.5.0_SHA256SUMS.txt
```

**Rollback:** see CONTRIBUTING → "Releasing and rolling back". Delete the release or revert it to a draft (pre-release does not retract it), then fix forward with v1.5.1.

```bash
gh release edit v1.5.0 --draft=true
```

## 7. Not part of the release, but done

- **Installed app.** `/Applications/GitPulse.app` had been built at 11:02, before the `63c2c5f6` dialog fix at 11:44. A bundle of `63c2c5f6` was built at 13:08. Its frontend contains the fix (`isolate` on the terminal host in `TerminalPanel-*.js`, and `.isolate{isolation:isolate}` in the stylesheet), and you installed it. The four commits after `63c2c5f6` change the workflows, update-check networking and docs, not the terminal.
- **Your `fseventsd` stall** (section 3) was the only cause of the first run's watcher and `vite-watch` failures. Restarting the Mac cleared it.
