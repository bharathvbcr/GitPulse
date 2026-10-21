# GitPulse 1.5.0

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
- **Default Priority Ordering on Tasks Board.** The Tasks board orders by priority by default across columns and in the List layout (`interfaceStore.taskOrder`, default "priority"), preserved in saved views and the View menu. (`8032c383`)
- **Storage Panel Commit-Graph Write (OD 3).** User-clicked, gated commit-graph write (`git commit-graph write --reachable --changed-paths`) for repositories missing commit-graphs or Bloom filters, previewed, judged by the command gate and cancellable. (`d7aa79a2`)
- **Ledger Site Binding & Adoption (OD 1).** A ledger is bound to its anchor directory, lineage UUID, inode, and volume persistent UUID. A ledger moved across sibling clones is refused; **Adopt this ledger** (`cmd_adopt_ledger`) allows safe takeover with audit logging. (`2e904ff4`, `f053e297`)
- **Cross-Lane Merge Conflict Detection.** `gitpulse_merge_conflicts` checks whether pairs of checkout heads would merge cleanly or conflict over an isolated object quarantine before merging. Insights worktree rows display cached `head`, `ahead`, `behind`, and `commits_since_base`. (`d3d947f8`, `14aced7e`)
- **Commit Landing Verification (`gitpulse_landed`).** Proves whether a commit's work reached a base branch (`merge`, `ff`, `squash`, `rebase`, or `tree`) using quarantined merge trees with provable absence and lazy-fetch fences. (`a678ed7c`, `c22e9073`)
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
- **Reflog replay resumes from watermarks** per worktree and ref, covering the full worktree family and default branch in one transaction. (`79bccca6`)
- **Transcript cursors carry fingerprints** (head, tail, length, mtime), so rewritten transcripts re-read and moved files resume cleanly. (`a8990f56`)
- **File & folder creation respects process umask** (`0o666 & !umask` for files, `0o777 & !umask` for executables/folders) instead of forcing owner-only permissions. (`e6578626`)
- **In-flight edit collision hook asks the running app** for its cached collision scan, cutting spawned git processes during edit bursts. (`b1559022`)

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
- **The ledger schema bumps to writer generation 2 (OD 1).** Additive columns and tables for occurred time, reflog ordinals, and deduplication indexes. Older generation 1 binaries (`gitpulsed`, `gitpulse-mcp`) refuse to write a migrated ledger and must be rebuilt and reinstalled together. (`f053e297`)
- **Dead `cmd_stash_pop` IPC command removed.** `GitWriter::stash_pop` and `cmd_stash_pop` are removed in favor of `cmd_stash_action` with target OID. Registry stands at 284 handlers. (`6869235a`)
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

