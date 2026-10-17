# GitPulse 1.4.0 — release notes

**Requested label was v1.3.5, merging 1.3.4 into 1.3.5. There is no 1.3.4 section anywhere in this repository, and no `[1.3.5]` changelog heading. The tree is 1.4.0 (`package.json`, `src-tauri/Cargo.toml`, `src-tauri/tauri.conf.json`, and `plugins/gitpulse/plugin.json`). Tags were not moved.**

These notes are everything since the published release **v1.3.1** (annotated tag `v1.3.1` → `0e020ab1`, changelog `## [1.3.1] - 2026-09-22`). v1.4.0 was never published. The local annotated tag `v1.4.0` still points at `ed54ac18`, which is 528 commits behind HEAD `0f862e80b94db4c6ebfc7447d806c3d91533811a`. Local annotated tag `v1.3.5` still points at `3d21d7f6`. Safety tags were left in place.

`CHANGELOG.md` now has an empty `## [Unreleased]` and `## [1.4.0] - 2026-10-17`. That section is the previous `[1.4.0] - 2026-10-11` body with the former Unreleased body moved above it. Wording of the entries was not rewritten. The compare links were already in Keep a Changelog form and were left as they are:

- `[Unreleased]: …/compare/v1.4.0...HEAD`
- `[1.4.0]: …/compare/v1.3.1...v1.4.0`

The Documentation subsection of the previous 1.4.0 body still says the feature reference covers "the 1.3.5 behaviour". That sentence was not edited.

The 528 commits after the local `v1.4.0` tag are mostly fixes and tests (157 `fix`, 138 `test`, 33 `perf`, 26 `docs`, 20 `feat`). The busiest scopes are terminal, ledger, mcp, repostore, pulse, ingest, files, and git. No commit subject used a breaking-change marker.

The GitHub release body is still produced by `scripts/release-notes.mjs` from the changelog section. This file is the grouped reading of that section.

## Highlights

Each item is the changelog's own claim. "Former Unreleased" means the block that was under `## [Unreleased]` and now sits at the top of `## [1.4.0]`. "Previous 1.4.0" means the body dated 2026-10-11.

- **Background agents, stacked worktree tabs, and a model per agent.** Agents start in the background with a pane of everyone at work; every checkout of one repository stacks as tabs; Settings → Agents → Agent models chooses the model. One agent guidance is included in every handoff. Previous 1.4.0 intro and Added ("Stacked worktree tabs", "Choose each agent's model").
- **The task board can delete, merge, group, undo, and run in waves.** Agents can delete and merge tasks and are asked to group findings onto one card. People get Undo, bulk changes, swimlanes, saved views, work-in-progress limits, checklists, and links. "Run in waves" launches the top N suggested tasks in their own worktrees and merges only a `passed` verdict, and nothing is pushed. Previous 1.4.0 intro and Added ("Run the board in waves", "Undo on the task board", "Agents can delete a board task", "Agents can merge duplicate board tasks", "Agents group work instead of filing a card per finding", "Board views").
- **Pull requests, file search, and file rename/move/delete.** Code → Search searches file contents. The file tree renames, moves, and deletes tracked files. A pull request expands in place and can be reviewed and merged. Previous 1.4.0 Added ("Search file contents", "Rename, move and delete from the file tree", "Open, review and merge pull requests").
- **Git work is admitted by class.** A refresh storm can no longer starve an action you asked for. All GitPulse processes share one spawn budget (`spawn-budget.v2` in the former Unreleased Changed entry "The spawn budget shared between GitPulse processes is versioned"). Previous 1.4.0 Fixed ("A refresh storm can no longer refuse your own actions", "All GitPulse processes share one spawn budget").
- **Two agents in one worktree show up in collision risk.** Previous 1.4.0 Added ("Two agents in one worktree are no longer invisible").
- **Dead-branch cleanup keeps backups, and a partial secret scan is not "clean".** Previous 1.4.0 Added ("Dead-branch cleanup", "Secrets panel") and Fixed ("Secrets no longer reports a partial scan as clean").
- **A terminal agent ends only on its own exit, Close, or Quit.** A stalled or hidden view does not kill it. On macOS, Cmd-Q, the Dock, and a Quit Apple event reach the quit confirmation, and a second launch hands its request to the running GitPulse. Previous 1.4.0 Fixed ("A terminal agent is no longer ended…", "Cmd-Q, Dock → Quit…", "One GitPulse per profile on macOS").
- **The code graph opens the maps a current devmap writes, and schema parity is checked.** `bun run check:schema-parity` compares DevCouncil's checkout with every installed reader. Previous 1.4.0 Added (`check:schema-parity`) and Fixed ("The code graph opens maps the current devmap CLI writes").
- **GPU terminal rendering, still gated.** Visible terminals can draw through `@xterm/addon-webgl` 0.19.0, at most 8 contexts, with an immediate DOM fallback after a lost context. Settings → Appearance → GPU terminal rendering turns it off. The deciding bench has not run. Former Unreleased Added ("GPU terminal rendering"). Commit `feat(terminal): WebGL renderer on attended sessions, DOM after a loss; A/B not yet run`.
- **A host-load governor refuses agent launches under memory pressure.** One verdict (`sysload::governor`) refuses launches at severe pressure and sheds background work through the existing spawn gate. The user's shell, and stop, kill, cancel, save, commit, release, and reconcile, are not gated. **Admit anyway** admits for two minutes. Former Unreleased Added ("Host load governor"). Commit `feat(sysload): one load-governor verdict refuses agent launches and sheds the spawn gate under memory pressure`.
- **A repository is read one snapshot at a time.** A watcher, poll, activation, or refresh that arrives during a snapshot shares one trailing read. Switching back to a tab whose snapshot landed in the last 5 seconds issues no snapshot read. Former Unreleased Changed ("A repository is read one snapshot at a time"). Commit `perf(hydrate): one snapshot in flight per tab…`.
- **Open tabs keep less history.** The graph keeps rows for the 6 most recently used repositories. Terminal scrollback is a setting (5,000 rows for shells, 1,000 for agent CLIs and task runs). On macOS, Option is Meta in the terminal unless turned off. Former Unreleased Changed ("Open tabs no longer hold their commit history, and terminal history is a setting").
- **Idle GitPulse wakes on events.** Watched repositories, the single-instance mailbox, the task-board check, and idle PTY readers no longer poll on short timers. Former Unreleased Changed ("An idle GitPulse wakes far less often", "An idle terminal no longer wakes its reader ten times a second").
- **Ledger rows record when the event happened.** `detail_json.occurred_at` and the task bound at that time replace a stamp from the catch-up pass. The event contract and schema are unchanged. Former Unreleased Fixed ("Ledger rows say when the thing happened"). Commits `feat(ledger): event time in detail_json…` and `feat(ledger): the journal shows event time…`.
- **Manvi proposals and a verification verdict on GitHub.** A Proposals panel reviews Manvi's proposals. A task's verification verdict can be posted as the `devcouncil/verify` commit status. An agent's `gitpulse_complete_task(done)` lands in `done` only with a settled `passed` verdict for the current head; otherwise it stays in `review`. Previous 1.4.0 Added ("Review Manvi's proposals", "Post a task's verification verdict") and Changed ("An agent's `gitpulse_complete_task(done)` no longer takes its own word").

## Fixes

- A slow host no longer kills terminal input. Input waits for the real answer, queues keystrokes, and stops only when the session is gone or nothing answers for 60 seconds. Former Unreleased Fixed ("A slow host no longer kills a terminal's input").
- Terminal output travels on a per-session channel of raw bytes, up to 64 KiB a message, instead of a global event. Former Unreleased Fixed ("Terminal output travels on a channel of its own"). Commit `perf(terminal): a session's output goes down its own Channel of raw bytes, in 64 KiB messages`.
- Typing and resizing no longer wait behind git. They run on a thread per session. Former Unreleased Fixed ("Typing and resizing a terminal no longer wait behind git").
- Branch names git allows (`fix/a&b`, `feat(ui)=x`, `a]b`) work. A name still may not start with `-` or contain C1 controls. Former Unreleased Fixed ("Branch names that git allows now work").
- A commit that finishes after a repository-tab switch no longer clears the other repository's message. Former Unreleased Fixed ("A commit that finished after you switched repository tabs").
- One failing GitHub alert tier is named as missing; Health shows "Incomplete" instead of hiding the other tiers. Former Unreleased Fixed ("One failing GitHub alert tier no longer hides the others").
- A file-system event with a non-UTF-8 path, an unknown flag bit, or a panic in the watch handler no longer aborts GitPulse on macOS. Former Unreleased Fixed ("A file-system event can no longer abort GitPulse on macOS").
- Send to agent from a board card no longer crashes. Branch-menu Push, Pull, and Checkout run again. Previous 1.4.0 Fixed ("Send to agent from a board card crashed on every launch", "A branch's menu Push, Pull and Checkout did nothing and threw").
- Open repository tabs and their groups survive quitting. Repository trust survives reboots and toolchain updates. Previous 1.4.0 Fixed ("Open repository tabs and their groups survive quitting the app", "Repository trust survives reboots and toolchain updates").
- A partial dependency or coverage scan is not reported as measured. One `AUDIT_TIMEOUT` (90 s) covers the dependency scan; a cut coverage report is `truncated` and is not cached. Former Unreleased Fixed ("A dependency or coverage scan returns by one deadline").
- Closing a tab group releases its tabs the way closing a single tab does. Former Unreleased Fixed ("Closing a tab group now releases its tabs").

## Hardening

- The app enforces an ACL on its own commands. The main window is granted only the commands the frontend calls; the status popover only its three; six commands nothing calls are denied. Webviews cannot navigate off the app origin. Previous 1.4.0 Security.
- Trusting a repository, or pointing devmap or manvi at a new binary or source root, asks in a native dialog the page cannot answer. Open-in-default-app confirms executables and related launch files. Previous 1.4.0 Security.
- Remote images in repository Markdown do not load until clicked. Previous 1.4.0 Security.
- Remote tool installs build one pinned commit. curl is confined to https. Clone refuses Git older than 2.45.1; submodule recursion refuses Git older than 2.50.1. Previous 1.4.0 Security.
- A released app's hook that cannot launch refuses the call (exit status 2) instead of failing open. Previous 1.4.0 Changed ("A released app's safety hooks and MCP server now resolve").
- Repository hooks (`.gitpulse/hooks.json` and `hooks.toml`) run only after the exact file text is approved, and "Run repository hooks" is off by default. Previous 1.4.0 Changed ("Repository hooks need your approval").
- Every signal GitPulse sends goes through `procguard::signal`, which refuses pid 0, 1, and negative ids, the app's own process, and a pid whose start time moved. Former Unreleased Fixed ("Every signal GitPulse sends now goes through one guarded primitive").
- Code-intelligence previews and file saves refuse oversized content before work starts (2 MiB per previewed file, 8 MiB and 256 files per preview batch, 8 MiB per save). Former Unreleased Fixed.
- Copying build caches into a new worktree is all or nothing, and the target must be one of that repository's worktrees. Former Unreleased Fixed.
- A peer that sends a byte at a time cannot hold the alert bridge or the code-index daemon probe open past one wall-clock deadline. Former Unreleased Fixed.
- A build that would ship source maps fails. Former Unreleased Fixed ("A build that would ship source maps now fails").

## Breaking changes

- **dc-store schema 11 is a one-way migration.** A host built against schema 10 refuses a migrated profile. Rebuild GitPulse, Manvi, and DevCouncil from a schema 11 dc-store before any of them opens the profile. Previous 1.4.0 Upgrade. The same Changed body later records schema 12: a profile Manvi's dc-store has taken to 12 is refused by a GitPulse built at 11, and the installer requires a `dcstore` that reports schema 12.
- **Reinstall `gitpulse-mcp` and `gitpulse-hook`** (`bun run mcp:install`). A 1.4.0 binary built before 2026-10-08 carries the schema 25 `devmap-store` and cannot read a store a current `devmap` has built. Previous 1.4.0 Upgrade.
- **Child processes drop relative `PATH` entries** (`.`, `bin`, `./node_modules/.bin`). A setup that relied on one must use an absolute path. The Manvi sidecar starts from an allowlisted environment, so provider API keys and tokens set in the launching shell no longer reach it. Previous 1.4.0 Security.
- **An agent can no longer mark a task done on its own word.** Without a settled `passed` `work.verify.task` verdict for the worktree's current head, `gitpulse_complete_task(done)` leaves the task in `review`. A person moving a card is unaffected. Previous 1.4.0 Changed. The changelog's security note: this narrows what an agent can do; the check fails closed.
- **The update check announces published releases, not tags.** A tag newer than the running build is announced only after its release is published, confirmed by one bounded `HEAD` of the release's checksum asset. A check that cannot be made fails rather than guessing. Previous 1.4.0 Changed ("The update check announces releases, not tags").

## Known issues

- **The GPU terminal renderer is not cleared to ship on the bench that decides it.** `docs/PERFORMANCE.md`'s TUI-redraw A/B has not run. The setting exists and a lost context falls back to the DOM renderer. Former Unreleased Added ("GPU terminal rendering").
- **Prebuilt remote installs are unavailable.** None are pinned yet, so that rung reports itself unavailable. Previous 1.4.0 Security ("Remote tool installs").
- **The local `v1.4.0` tag is not this commit.** It still names `ed54ac18`. Publishing 1.4.0 at HEAD means moving that existing local tag. That move was not done here.
