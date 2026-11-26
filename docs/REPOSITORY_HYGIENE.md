# Repository hygiene and cache maintenance

## Which setting reaches which repository

Hygiene has three settings scopes. They are separate mechanisms, not three
views of one number, so each control names the scope it governs.

| Scope | Where you set it | What it reaches | Where it is stored |
| --- | --- | --- | --- |
| **Host-wide default** | Settings → Repo hygiene → Hygiene defaults | Every repository you preview cleanup in, unless that repository overrides it | `gitpulse:hygiene:defaults:v2` in app storage |
| **Repository override** | Insights → Storage → Repository hygiene | That one repository. Never the default, never the schedule | `gitpulse:hygiene:repo:v2:<repo>` in app storage |
| **Scheduled cleaner policy** | Fleet → Global build cleaner, or Settings → Repo hygiene | Every repository beneath its project roots, including ones not open in GitPulse | `hygiene/state.json` in GitPulse's config folder |

The scheduled cleaner keeps its own retention deliberately. Its sweep runs from
a headless worker with no window — on macOS from a launchd job while GitPulse is
closed — so it cannot read the app storage the other two layers live in. A
repository override changes previews in that repository and nothing else.

A retention control shows `Use the default · N days` until you choose otherwise;
choosing a value pins that repository and says so. Clearing it back to the
default restores inheritance, so moving the default later moves that repository
with it.

The **shared-cache review** is host-wide, not per repository. It measures caches
owned by the host — the Cargo registry, `GOCACHE`, the npm cache and their peers
— which `cmd_cache_inventory` reports without taking a repository argument.
One switch and one weekly stamp therefore serve the whole host: enabling it
anywhere enables it everywhere, and reviewing from one repository satisfies the
week for all of them. It is measurement only; nothing is removed without a
preview you accept.

The same list shows the **workbench profile backups** the profile store writes
beside the profile before a schema upgrade (`workbench.sqlite.pre-schema-<N>.bak`
and the other `workbench.sqlite.<stamp>.bak` copies), with size and age. They
are the only way back from an upgrade, so they are removed only through a
`backup:<file name>` preview you confirm: no retention age, scheduled hygiene run
or agent-driven cleanup selects them.

Settings written by an earlier build were a single flat record per repository.
Each one is adopted the next time its repository is opened: a retention that
differed from the old hardcoded 30 days becomes that repository's override, a
retention left at 30 becomes inheritance, and a weekly review that was on
promotes the host-wide switch, carrying the most recent measurement stamp so
adoption does not force an immediate rescan. The legacy record is removed as it
is adopted, so a stale record cannot switch the review back on after you turn
it off.

## Scheduled cleanup

Open **Fleet → Global build cleaner** or **Settings → Repo hygiene** to maintain
repositories beneath selected project roots, including repositories that are not
open in GitPulse. Save roots, exclusions, retention, byte/target limits and an
optional schedule. Defaults are off, 30-day retention, a weekly interval,
10 GiB of attempted output and at most 20 directories per run. Inspect saved
roots before an immediate run. An incomplete inventory does not block cleanup:
what it could not read is reported and left alone, and the directories it read
in full stay eligible.

**Insights → Storage → Repository hygiene** provides individual expiring
previews and tool-owned shared-cache maintenance. Its shared-cache review is
read-only and runs while the page is visible; it is separate from the native
global cleanup schedule.

## Global scheduling and ownership

The native scheduler runs every 30 seconds while GitPulse is running. It uses a
versioned, atomically written policy and a bounded history in GitPulse's config
folder under `hygiene/state.json`. UI timers only refresh status. A saved policy
revision prevents stale windows from restoring old settings; changes and
cancellation revoke the current run. Cross-process locks serialize claims and
mutations, including manual per-repository operations. Locks explicitly unlock
before closing to avoid transient forked descriptors retaining ownership.

Optional **Also run when GitPulse is closed** is available in an installed macOS
application bundle. It registers the current user's
`~/Library/LaunchAgents/dev.vbcr.gitpulse.hygiene.plist` (builds before the VBCR Dev rename used `com.gitpulse.hygiene`; the first launch of a newer build replaces that agent with this one), with a 60-second interval and
explicit `--cleaner-due` arguments. The worker checks the same policy without
creating a window. Registration first commits disabled authority; only successful
setup enables it. A crash, unavailable launchd or failed recovery write cannot
leave the attempted schedule enabled. Foreign jobs and symlink paths are refused.
Disabling revokes authority before unloading the job. No shell or crontab entry
is used. See [Apple's launchd guidance](https://developer.apple.com/library/archive/documentation/MacOSX/Conceptual/BPSystemStartup/Chapters/ScheduledJobs.html).

Replacing an ad-hoc signed app while this agent is loaded makes its next run
fail. macOS pins an agent loaded at login to the code hash of the build it
starts, which is all an ad-hoc signature offers, so the first run after the
replacement is killed with "Launch Constraint Violation" before `--cleaner-due`
executes. A build signed with a team identity is pinned by team and signing
identifier instead and keeps running across rebuilds; local builds use one when
the keychain has it ([Installation](INSTALLATION.md#signing-local-builds)).
No cleanup starts, the policy is untouched, and launchd re-pins the agent to the
new build for the following run. GitPulse's startup check compares only the
agent's executable path, so it does not detect a build replaced at the same path.
[Installation](INSTALLATION.md#replacing-an-installed-build) shows how to replace
the app without the refused run.

Intervals are elapsed hours, not a calendar or timezone promise. A due run claims
and advances the next time before mutation. Restart/wake can attempt one missed
run; missed intervals are not replayed. A manual run also advances an enabled
schedule. There is no boot-time system service; closed-app mode requires the
user's macOS login session. Linux supports the app-running path; Windows exposes
inventory/guidance and refuses unsupported cleanup.

Discovery is bounded to 16 roots, 128 exclusions, 40,000 inspected entries,
24 levels, 128 repositories and 30 seconds. Overlapping roots are deduplicated.
Hidden, dependency and generated-output directories are not recursively searched
for repositories unless explicitly selected as roots. Symlink repositories are
not followed. Repository inventory stops starting scans after 120 seconds or
512 candidates; the current bounded storage scan may finish. Storage's 64-item
artifact cap and permission failures propagate as partial results. Returned
counts describe inspected results, not an exhaustive machine inventory.

Incompleteness is refused at the narrowest scope that the evidence supports, in
three tiers. A **directory** whose own enumeration did not reach the end is never
a removal candidate: its size is a floor and what the rest of it holds is
unknown. A **repository** whose inventory could not be enumerated at all — a scan
error, an untrusted repository, an ignore or tracked-file check that did not run,
or artifacts cut off by the 512-candidate cap — has every one of its candidates
dropped. The **run** refuses only when nothing is eligible and something could
not be read; a complete scan that simply found nothing to remove is an ordinary
successful run. A partial inventory is otherwise a statement about coverage and
not a veto, so a permission failure in one directory, a repository past the
linked-worktree sizing cap or a 65th artifact directory no longer stops the
trees that were read in full from being cleaned. Every run that could not read
everything reports `completed_with_skips` and names what it skipped, so a
reduced-coverage run is never presented as complete.

A run requires clean worktrees, readable task state with no active leases,
producer evidence, ignored and untracked output, retention and fresh
filesystem/activity checks. Git checks suppress configured filesystem-monitor
hooks. Every deletion rechecks the saved revision and exclusions after the host
policy gate. Attempts consume the byte budget even when execution fails.
The user can select 1–100 targets, up to 1 TiB and 7–3,650 retention days through
the native contract; UI presets are intentionally narrower. The worker stops
starting targets after five minutes; each operation keeps its own deadline.
The headless process has a 15-minute outer deadline and cancels before exiting.

The target limit counts removals attempted, not rows: a candidate refused for
its repository's state, its age or the byte budget does not use it up, and a
repository refused for its own state is asked once per run. A run lists at most
100 rows, keeping room for every removal it may still attempt; refused rows
past that are counted as "not listed", never dropped silently.

A check the host-load governor sheds under memory pressure is not a refusal.
The run asks again after 5, 15 and 40 seconds while 90 seconds of waiting
remain, then reports the candidate as **could not examine** (`unexamined`)
rather than skipped. A run that ends with any such row brings an enabled
schedule forward by 15 minutes, doubling for each such run in a row, at most
four times, after which the saved interval applies; a run that examined
everything resets this. The check keeps its own admission class: it is never
promoted to a user action to get past the governor.

### Reclaim scopes (off by default)

Three settings widen what the scheduled cleaner deletes. Each is off by
default, and no run — scheduled, in-app or manual — uses any of them until a
person has previewed the current set with **Inspect saved roots**. The preview
is recorded in `hygiene/state.json` only by that command, never from the saved
policy, and any change to the set clears it, so turning a scope off and on
again needs a fresh preview. Changing the roots or the exclusions clears it
too, for every scope. A run with scopes enabled but not previewed uses none of
them and says so.

A run does not delete "exactly the previewed set". It applies the rules you
previewed to what it finds when it runs: a folder that newly matches since the
preview is included, one that stopped matching is not, and every candidate is
judged again from scratch at delete time. For the agent scratch scope the
approval also lapses 30 days after the last preview (`previewed_at`, written
only while that scope is on); a run after that skips the scope and says so in
its issues, and the Settings page stops showing the set as previewed.

| Setting | What it adds | Vetoes kept |
| --- | --- | --- |
| **Reclaim build caches in idle linked worktrees** (`scopes.worktree_caches`) | Linked worktrees under the roots (including `.claude/worktrees/*` and `.gitpulse/worktrees/*`, which discovery never enters) are inventoried, and the caches worktree creation clones into them (`target`, `.turbo`) become candidates | No process working anywhere in the worktree; its tree clean; no task lease in the main checkout's store naming its branch (or naming none), and none at all in the worktree's own store; producer evidence, ignored and untracked; nothing open; nothing modified for the idle days. Budgeted by clone-private bytes, not logical size |
| **Judge uncommitted work per project** (`scopes.per_tree_vetoes`) | Uncommitted work refuses a repository's output only when it is in the project directory that produced it | Task leases stay repository-wide; every other check unchanged |
| **Remove stale linked worktrees** (`scopes.prune_worktrees`) | Linked worktrees the stale-worktree report calls stale are removed with `git worktree remove`, never forced; the branch is kept | No process inside, clean, unlocked, no task lease in either store, idle at least a day, and integrated into the main worktree's branch or idle past the idle days; a detached HEAD must be integrated |
| **Remove agent scratch builds** (`scopes.agent_scratch_builds`) | Cargo build folders agents leave outside any repository scan: in `<temp>/claude-<uid>/<project>/<session>/scratchpad` of a session that is finished, in `.gitpulse/scratch` or `tmp` of a checkout (the repository and each of its linked worktrees under the roots) when git ignores them, and Cargo targets (`target` or `*-target`) directly under a checkout's `.claude/`, including the copy a new session worktree is seeded with (only the target folder, never config or skills; a never-built-in copy is called out as such) | producer evidence (`.rustc_info.json`, a file Cargo writes, beside a `CACHEDIR.TAG` carrying the cache-directory signature `8a477f597d28d172789f06886806bc55`; that signature is the generic one every tool following the cache-directory convention writes, not Cargo's own, so it is evidence of a cache directory and `.rustc_info.json` and the layout check carry the rest; a directory merely named `target` is kept); the folder's top level holds only what Cargo, rustdoc and the coverage tools write (`CACHEDIR.TAG`, `.rustc_info.json`, `.future-incompat-report.json`, `.rustdoc_fingerprint.json`, `*.profraw`, `*.profdata`, `.DS_Store`, profile directories recognised by `.cargo-lock`, `.fingerprint` or `deps`, a target-triple directory holding one, `doc`, `package`, `tmp`, and a nested marked target such as `llvm-cov-target`), so a folder anyone keeps their own files in, or a symlink at its top level, is kept and named; in a checkout's `.gitpulse/scratch` and `tmp` only folders named `target`, `<name>-target` or `target-<name>` are offered; in a session's scratchpad a plain folder with such a name that wraps real targets is looked into one level, each child judged as a target of its own and the wrapper never removed; no symlink on the way and the path inside the scope root; DevCouncil's `protected_artifact` on the path relative to the scope root (`<checkout>/tmp`, `<checkout>/.gitpulse/scratch`, the scratchpad, `.claude`: the root's own name is not consulted, every name between it and the target is) and `protected_entry` on every entry; no entry on a different device than the target (a mount point keeps it); a strict walk of at most 600,000 entries or 45 seconds, past which it is kept as "too large to verify"; no process uses it (working directory, executable, argument or environment) and none of your processes was unreadable; for a scratchpad, the folder name is a session id the Claude registry does not list as running, the Claude detector could look (a detector that could not look is **could not be checked**, not "kept"), and no running Claude session lacks an id (a session the registry names no id for is matched through the `--session-id` on its command line; the bundled ripgrep and the browser bridge are not sessions; one with neither keeps every session folder, is stated once as its own sentence naming how many, and the folders are offered again when that count is zero) (Codex, Antigravity, DevCouncil and Manvi sessions never keep a scratchpad: they cannot hold one); git's answers to "is it ignored" and "is anything tracked" are three-valued, and one git could not give (shed under load, untrusted repository, timeout) keeps the folder as **could not be checked**, counted as unexamined so the next run comes sooner, never as "not ignored"; nothing modified within `scopes.agent_scratch_days` (default 7, minimum 7). The scratchpad and every other file in it stay. A session folder must also be quiet: the newest file in the folder and in the top levels of its `tasks/` and `scratchpad/` is at least `scopes.agent_scratch_days` old (a running session writes there continually, so this holds for a Claude process the detector does not recognise or a session whose id changed after launch; a modification time in the future keeps it; a listing that could not be read, or one of more than 5,000 entries, is **could not be checked**). What could not be looked at follows one rule. A lasting property of one folder is an answer about it: permission denied on a folder or marker, or a listing over its entry bound (5,000 for a session folder's listings, 5,000 for a target's top level) is kept with a reason that says so ("GitPulse may not read it", "more than N entries"), listed, never offered, and the scan stays complete, so the other scopes stay approvable. A could-not-look that may clear on its own (the host shed the check, the process table or a process could not be read, a time budget ran out, a cancelled walk, any other I/O error) is **could not be checked**, never "kept" and never "fine", and marks the scan incomplete. Those folders are counted apart from repositories shed under load, and the "inspect again when the host is less busy" advice appears only when load was the reason. The worktree-cache scope's lease and main-checkout checks keep their plain "skipped" refusal. The delete step judges each candidate again from scratch, reads the process table once more immediately before the first deletion, and stops at the run's byte cap, which is charged the bytes actually removed: a removal that stopped part-way (a build that started in the folder, an entry that changed) is reported as **partly removed** with the bytes it did take, and those bytes count against the cap. Bytes are counted per file, not per name: Cargo hard-links heavily (an uplifted binary beside its `deps` copy), so a file counts once, and only when every one of its names is inside the folder; a file that keeps a name outside it is not freed and is not counted. Removing one name of a hard-linked file changes the change time of the others, which the removal expects for a file it has already unlinked a name of (size and modification time are still checked); a file none of whose names it has touched is checked in full, as before. This engine also removes local repository build folders, so the repository scope had the same defect and has the same fix. The recency guard excuses only the modification time of the directory whose entry the run itself removed (the scratchpad or the wrapper folder), only from the instant before its first removal there, and reads what is inside instead: a file a live session writes afterwards, in `scratchpad/`, in a wrapper or in `tasks/`, still keeps the folder. A partly removed target leaves itself with a new time, so its siblings are kept for that retention period and say so. The guard sees only the top level of the session folder, `tasks/` and `scratchpad/`, and Finder writing `.DS_Store` there keeps a folder "recent"; both are limits. For a parent directory the run itself removed from (recorded per directory, only once a removal there unlinked something), a session that only deletes files there, or moves in a file with an old date, leaves no trace for the guard; the process check is what covers a live session. The window that remains is stated plainly: a build that starts after that last reading and before the removal reaches its folder is not prevented. Removal stops at the first entry that changed since the review, so such a build can lose what the removal had already deleted from its folder before it noticed, and the run says so |

The idle threshold (`scopes.worktree_idle_days`) defaults to 7 days and cannot
go below DevCouncil's scheduled-retention minimum of 7. `node_modules` is never
offered: DevCouncil's `protected_artifact` preserves it.

With the scratch scope on, the inventory also lists every folder it left alone
with the reason, and temp directories in the user temp area that nobody can
account for. Those are report-only: an origin would be provable only from a
creator marker naming a process that is gone, nothing writes one, and there is
no deletion path for them. The SessionStart brief tells agents to put
throwaway builds in `<their checkout>/.gitpulse/scratch/target-<label>`, as an
absolute `--target-dir` or `CARGO_TARGET_DIR`, and never to put build output
directly inside a checkout's `.claude/` folder, because new session worktrees
are seeded with a copy of it. That path is per repository, on its volume,
ignored by git here, and not on the preserved-name list (`tmp`, `.cache` and
`.claude` are).

The session scope is not limited by the configured roots: scratchpads live in
the system temp directory. Exclusions still apply to every agent candidate, in
the preview as in the run.

### Long-lived data (report only)

When the disk is full and the cleaner finds little, the space is usually in
data GitPulse will not clean. The inventory therefore carries a **Not cleaned by
GitPulse** list (`long_lived`): a fixed set of generic locations (model caches,
agent configuration and transcript folders, toolchains) that exist on this
machine, each with a size, plus up to 16 absolute directories the person adds in
the cleaner settings (`long_lived_paths`). Paths the cleaner already manages (the
shared caches) are not on it. It is only a list: it has no deletion path, no
approval and no effect on any scope's preview or run, and it is measured only
on the preview that measures (the inventory), not on a scheduled run.

What a user-added path can make GitPulse read: the sizes (metadata only, never
file contents) of one existing real directory. It is refused when it is not
absolute, holds `.` or `..`, is a symlink or is reached through one, is not a
directory, is `/`, the home directory or an ancestor of it, or contains a
reclaim root; it is validated when saved and again when reported. Its walk
never follows a symlink, never crosses onto another filesystem, counts a hard
link once, and has its own budget (300,000 entries and 8 seconds per path, 1,000,000
entries and 30 seconds for the whole report, and cancellation), separate from
the agent scan's. A walk cut short, or a part it could not read, shows the size
as "at least"; a path that could not be read at all shows an error, not zero.

### The in-use check and disk relief

**What the in-use check cannot see.** It reads working directory, executable,
arguments and environment of the processes of the user running GitPulse.
Relative `--target-dir rel`, `--target-dir=rel` and `CARGO_TARGET_DIR=rel` are
resolved against the process's own working directory. Still invisible: the
environment of Apple platform programs, which macOS withholds and which reads
as empty (it cannot be told apart from a genuinely empty environment, so it is
not marked unproven: doing so would keep every folder while any `/bin/sh` runs);
a `target-dir` set only in a `.cargo/config` file; and other users' processes,
which are counted and reported but not inspected. Worktree teardown uses the
same check, and a process of yours it could not read refuses, after the unreadable
ones are read again up to three times, 20 ms apart, so one caught mid-exit is not
mistaken for one that cannot be read. A process whose
owner the OS will not name is treated as yours and unreadable. A working
directory the OS gives no path for, for any reason, is unproven, not "inside
nothing". (macOS keeps answering with the old path for a directory removed
under a process, measured, so such a process still counts as a user of that
path.)

When the disk remedy is due (`sysload::disk_remedy_due`: the disk is the only
reason the host is shedding work, which is `shed_is_disk_only`, or free disk
reads below the Severe floor on a fresh sensor whatever else is pressed,
because a nearly full disk brings memory pressure with it; low disk at
Elevated, or the fill projection alone, with something else pressed does not
qualify, and it answers false on any doubt), an enabled in-app schedule is
brought forward to now, at most once every 3 hours (it does not bump the policy revision, so a Settings page
open at the time can still save). Under a persistently low disk this is a run
every 3 hours, only for a schedule that is already enabled and only in the app:
the headless launchd path never brings itself forward. The run that follows uses
the saved, previewed scopes and every check it always makes, and its bounded
read-only git checks (`check-ignore`, `ls-files`, `worktree list`, at preview
and again at delete time) are admitted while that holds (`with_disk_relief` in
the spawn gate, granted around each one spawn, never around a scan); under
memory, swap, CPU or thermal pressure while the remedy is not due they are shed
like any other spawn.
`git worktree list` is not free of writes: `listed_worktrees` reaches
`scan_listing`, which calls `remember_verified_member_paths`
(`engine/worktree.rs`), and that stores the worktree family through
`worktree_family_disk::store`, a small JSON file under the app's data
directory. It does so under relief too. The spawn class (background or reactive) and the
governor's answer (shed or not, and whether the disk remedy is due) are
read once, when a spawn asks the gate (`acquire_judged`); a spawn that then
waits in the gate for a slot or a rate token is not asked again, so pressure
that changes during that wait does not change its admission. The reverse
case is a due scheduled run under pressure the run does not relieve (memory,
swap, CPU, thermal, or low disk only at Elevated, or a fill projection alone,
beside other pressure): the in-app scheduler holds it back (logged once) and
starts it when the host stops shedding or the disk remedy becomes due. While it
waits, the cleaner's status carries the governor's reason (in memory only,
never saved, dropped the moment the run starts, is disabled or is no longer
due) and the panel says: "The scheduled clean is due and waiting: <reason>. It
runs when that clears, or run it now." A person's own preview or run is never
held back. One fact limits it: the headless run (`run_due_headless`) has no
governor in its process, so the relief is moot there: its checks are never shed
for load. With the disk input set to shed rather than refuse, the Severe branch
of `disk_remedy_due` never fires and only the disk-only branch applies.

### Stale worktrees

**Insights → Storage → Stale worktrees** lists the linked worktrees nobody is
using: no process works in them (the process table, which covers agent
sessions, shells and builds alike), unlocked, clean, holding no task lease
(the task store is per checkout, so both the main checkout's and the worktree's
own are read), and integrated or idle past the threshold (a detached HEAD must
be integrated), with what removing each frees in clone-private bytes. A check
that could not run makes the worktree not stale and names the check. Nothing is
removed except by a click, which goes through the guarded worktree removal.

### Build-cache clones

A new worktree gets an APFS clone of the main checkout's Cargo `target`.
**Settings → Repo hygiene → Build-cache clones** limits how many linked
worktrees of one repository may hold one (`clone_budget.max_target_clones`, 0
for no limit); at the limit the new worktree is made without one and its
creation result says so. The default is no limit, from a measurement on
2026-11-14: a cloned `target` held 1.8 MB of private bytes against 66 GB
logical, while a worktree that built cold held 26.7 GB. Refusing the clone
makes the next build cold, so a count limit costs disk rather than saving it. The limit is read from `hygiene/state.json` by
the app, the MCP server and the `WorktreeCreate` hook alike. A total of the
linked worktrees' private bytes (`clone_budget.max_private_bytes`, default
50 GiB, about two cold builds of this size, 0 for no limit) is advisory: the Storage panel warns when the sized
worktrees exceed it. It is never enforced at creation, where measuring it would
walk every worktree.

New policy fields are written to `state.json` only once they differ from their
defaults, so a file this build writes with defaults stays readable by an older
build. Once a scope or the clone budget is changed, an older build refuses the
file as invalid and stops cleaning until this build is installed again. That
includes turning on the agent scratch scope (its fields are then written), the
preview timestamp that scope keeps, and a bring-forward still within its
3-hour gap (the stamp is dropped afterwards): enabling the new scope makes the
state unreadable to builds from before it existed.

The newest 20 runs retain exact target paths, before/after logical bytes, skips,
errors and partial outcomes. An item is journaled before mutation; failed journal
writes stop further work. Interrupted operations are reported, never replayed.
Source, environments, downloads and Git history are outside the scheduled scope.
Shared package caches keep their native retention and explicit manual actions.

## Reusable DevCouncil policy

The canonical Rust owner is `devmap-query::hygiene` in DevCouncil. GitPulse's
provider module is a thin re-export from the existing vendored crate. The shared
module provides producer classification, path/preservation rules, retention
validation, cache advice and agent guidance without a new dependency or daemon.
DevCouncil's existing Rust guide writer includes the rules when generating managed
agent guides; custom/mixed guides keep their existing ownership protection.
Hosts own consent, live activity checks, execution, history and OS scheduling.
Updating a DevCouncil binary alone does not update GitPulse's compiled-in policy:
re-vendor and rebuild the consumer. The rules forbid agents from enabling or
widening schedules on their own.

## Efficient defaults

| Ecosystem | Recommended strategy | GitPulse implementation |
| --- | --- | --- |
| Rust / Cargo | Keep automatic global garbage collection enabled; separately retire stale project output. | Measures registry/Git cache payload, explains native GC, discovers `target` and `target-*`, and previews ignored output with Cargo markers. |
| Go | Keep the shared build cache and its automatic expiry; avoid habitual module-cache purges. | Discovers `GOCACHE` and `GOMODCACHE` through Go. Offers `go clean -cache`, pins `GOCACHE` to the reviewed directory, preserves module downloads/fuzzing inputs, and recognizes repository `.gocache` with Go's README marker. |
| npm | Verify and garbage-collect unneeded cache data. | `npm --cache <reviewed path> cache verify`; leaves `node_modules` alone. |
| pnpm | Occasionally prune unreferenced packages. | `pnpm --store-dir <reviewed path> store prune`; leaves project dependencies alone. |
| Python / uv | Let uv manage its own locks and pruning. | `uv --cache-dir <reviewed path> cache prune`, with a bounded lock wait. Python bytecode and tagged pytest/mypy/ruff caches have local output adapters. |
| JavaScript / TypeScript | Retire ignored framework-generated output after work stops. | Recognizes `.next`, `.nuxt`, `.output`, `.svelte-kit`, `.parcel-cache`, `.turbo` and `.vite` beside `package.json`. |
| Java / Kotlin | Retain Gradle's automatic cache policy and downloaded modules. | Previews ignored `build` beside a Gradle manifest or `target` beside `pom.xml`; preserves `.gradle`, wrappers and dependency stores. |
| C / C++ | Retire generated CMake trees, not source directories with generic names. | Recognizes `cmake-build-debug`/`cmake-build-release` with `CMakeCache.txt` beside `CMakeLists.txt`. |
| .NET | Retire intermediates separately from released executables. | Recognizes ignored `obj` beside a C#, F# or VB project file. |
| Swift / Xcode | Use native build cleanup after stopping builds. | Guidance only: `.build` can contain dependency checkouts. GitPulse does not sweep it or DerivedData automatically. |

Cargo 1.88+ already tracks global-cache usage. Its default cleanup frequency is
one day, with one-month retention for regenerable data and three months for
network downloads. Offline and frozen commands disable that automatic cleanup.
It does not cover project build output. See the [Cargo configuration reference](https://doc.rust-lang.org/cargo/reference/config.html#cache).

A home-wide `cargo sweep -r` cron job obscures the scope and can contend with
active work. A single `CARGO_TARGET_DIR` also mixes final binaries from different
projects and invalidation contexts. Prefer per-project targets. Teams needing
cross-project compiler reuse can evaluate a compiler cache separately; GitPulse
does not install one or rewrite shell/Cargo configuration. Cargo's newer
`build.build-dir` can separate intermediates from final artifacts, but any
shared-directory policy must use distinct workspace paths and explicit retention.
See [Cargo build cache](https://doc.rust-lang.org/cargo/reference/build-cache.html).

Go's build cache also expires old entries automatically. Resetting test results
with `go clean -testcache` is useful for verification, not a meaningful space
recovery action. Module-cache and fuzz-cache clearing carry different costs and
are intentionally absent from the automatic recommendations. See the
[Go command documentation](https://pkg.go.dev/cmd/go#hdr-Build_and_test_caching).

Other provider references: [npm cache verify](https://docs.npmjs.com/cli/v11/commands/npm-cache/),
[pnpm store prune](https://pnpm.io/cli/store), [uv cache safety and pruning](https://docs.astral.sh/uv/concepts/cache/),
[Gradle cache retention](https://docs.gradle.org/current/userguide/directory_layout.html).
uv pruning can remove centralized project environments in versions that support
them; uv recreates them as needed. Local virtual environments are not swept.
Pruning packages can require downloads when switching branches or reinstalling.

## Review and execution contract

1. Repository storage remains a bounded measurement. Its artifact list is a
   candidate list, not permission to delete. Agent state, virtual environments,
   dependency stores, Terraform state, logs and scratch directories are reported
   as needing review even when Git does not track them.
2. A preview validates a literal relative path, the producer, ignore rules, the
   Git index, nested repository boundaries, protected names, symlinks, recent
   modifications, active build processes and open files. A failed check refuses
   preparation. Retention comes from the host-wide default, which ships at
   30 days, unless the repository carries an override; the UI offers
   7/14/30/90 days for either.
3. A complete snapshot records file identities, lengths, modification/change
   times, directories and logical sizes. Preview storage is bounded to four
   plans, each valid for five minutes and bound to one canonical repository.
4. The frontend sends only a plan ID to execute. The backend consumes it once,
   serializes maintenance, revalidates the directory/cache configuration and
   snapshot, calls the existing policy gate and rechecks after the gate returns.
   Shared actions gate both the exact tool argv and the affected file boundary.
5. Local generated files are removed relative to open directory descriptors,
   refusing symlink traversal and changed identities. Only reviewed entries are
   removed, followed by empty directories. New files are not swept. This avoids
   running project-defined cleanup scripts or letting a configured build tool
   widen the operation to an external build directory.
6. Shared caches are maintained by their owning tools, from a neutral home
   working directory with the reviewed cache path explicitly pinned. Cargo
   downloads and Go module downloads are measured but have no purge action.
   Custom cache locations outside recognized home-cache roots require manual
   maintenance; GitPulse never guesses a directory to remove.
7. Cancellation stops remaining local removals or terminates the tool process
   group through GitPulse's existing process runner. It does not roll back
   completed deletions. Failures and incomplete post-operation measurements are
   visible. Native tool output is not dumped into the UI, avoiding accidental
   credential/configuration disclosure.

Each snapshot has a 15-second, 250,000-entry, 48-level budget. Local deletion has
a 60-second deadline; native maintenance has 120 seconds and a 64 KiB output
budget. Discovery commands have five seconds; process/open-file checks have
five/ten seconds. Only one inventory, one preview preparation and one execution
can run at a time. Shared-cache inventory stops starting providers after 45 seconds;
the current bounded provider is allowed to finish. A cap or unavailable tool is
reported explicitly, never presented as a complete scan or zero-byte cache.

The open-file/build checks are conservative observations, not a transaction
with external build systems. Stop builds before cleanup; another process can
start after the final check. Descriptor-relative removal protects the filesystem
boundary and notices replacements, but cannot roll back partial deletion or
freeze concurrent writers. Per-repository cleanup requires a preview; unattended
cleanup requires an explicitly saved global schedule and still applies fresh checks.

Sizes are **logical directory bytes** (each file counted once, only files wholly inside the folder), not guaranteed disk-space savings.
Hardlinks, APFS clones, sparse files and concurrent activity can change physical
space reclaimed. Shared-tool previews show current cache size, not how much a
prune will remove. Missing post-operation measurements remain unavailable.

Local descriptor-based removal is implemented for Unix. On unsupported systems,
activity verification refuses cleanup explicitly; Windows removal has not been
implemented or verified. Native installed-app scheduling, Linux execution and real shared npm/uv/pnpm
cache mutations remain separate validation gates.

## Repo hygiene beyond disk usage

Unignored artifacts offer an anchored, escaped ignore rule to copy and review.
Tracked files stay tracked; the app does not run `git clean -X`, edit the index,
or rewrite `.gitignore` silently. Existing Storage findings still show merged
branches, gone upstreams, Git objects and linked-worktree usage, with the existing
MANVI workflow for branch cleanup. Git history/reflog pruning is not automated.

## Code and validation

- `src-tauri/src/storage/hygiene/`: provider eligibility, bounded inspection,
  prepared operations, activity checks, process execution and exact-entry removal.
- `src-tauri/src/storage/hygiene/global.rs` and `background.rs`: native policy,
  discovery orchestration, scheduling, locks, journal and macOS job integration.
- `src/lib/components/GlobalCleaner.svelte`: shared Fleet/Settings controls,
  saved policy, inventory, cancellation and run history.
- `src/lib/components/HygieneDefaultsPanel.svelte`: the host-wide default and
  shared-cache review, and the scope table naming what reaches which repository.
- `src/lib/components/HygienePanel.svelte`: review, execution, cancellation,
  recovery, shared-cache inventory and this repository's override within Storage.
- `src/lib/storage/hygiene/`: wire types, the default/override resolution and
  legacy adoption, and ignore-rule helpers.
- `harness/hygiene.html`: rendered interaction regressions using controlled IPC
  fixtures; these are UI tests, not proof of real cache deletion.

Run `bun run test:browser -- --harness hygiene` (or add `--webkit` on macOS),
`bun run test -- src/lib/storage`, and the native `storage::hygiene`, `storage_stress`
and `hygiene_regression` tests using `src-tauri/Cargo.toml`. Run the repository's
IPC/type checks and Clippy after changing the command boundary. The regression
for untracked environments/agent state was observed failing before the fix.

The optional native Go check builds a small program with a disposable `GOCACHE`,
then runs the same prepared-operation executor and verifies that cache usage
decreases while source and the executable remain. It never discovers the user's
shared cache. Run it explicitly when Go is installed:

```sh
cargo test --manifest-path src-tauri/Cargo.toml --lib \
  storage::hygiene::tests::installed_go_cleans_only_the_reviewed_temporary_cache \
  -- --ignored --test-threads=1
```

### Audit and verification

See [the archived implementation audit](https://github.com/bharathvbcr/GitPulse/blob/16483b7de2b5c74003c7e7af046f7142ece7699b/docs/archive/REPOSITORY_HYGIENE_AUDIT.md) for reproduced
failures, fixes, test evidence and remaining platform/install gates. A passing
fixture or mocked IPC test is not proof of unattended operation in an installed
application. No existing user cache was removed or cleanup schedule enabled
while implementing this feature.
