# Repository hygiene and cache maintenance

Open **Fleet → Global build cleaner** or **Settings → Repo hygiene** to maintain
repositories beneath selected project roots, including repositories that are not
open in GitPulse. Save roots, exclusions, retention, byte/target limits and an
optional schedule. Defaults are off, 30-day retention, a weekly interval,
10 GiB of attempted output and at most 20 directories per run. Inspect saved
roots before an immediate run; incomplete inventories block cleanup.

**Insights → Storage → Repository hygiene** provides individual expiring
previews and tool-owned shared-cache maintenance. Its optional weekly review is
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
`~/Library/LaunchAgents/com.gitpulse.hygiene.plist`, with a 60-second interval and
explicit `--cleaner-due` arguments. The worker checks the same policy without
creating a window. Registration first commits disabled authority; only successful
setup enables it. A crash, unavailable launchd or failed recovery write cannot
leave the attempted schedule enabled. Foreign jobs and symlink paths are refused.
Disabling revokes authority before unloading the job. No shell or crontab entry
is used. See [Apple's launchd guidance](https://developer.apple.com/library/archive/documentation/MacOSX/Conceptual/BPSystemStartup/Chapters/ScheduledJobs.html).

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

A run requires a complete inventory, clean worktrees, readable task state with no
active leases, producer evidence, ignored and untracked output, retention and
fresh filesystem/activity checks. Git checks suppress configured filesystem-monitor
hooks. Every deletion rechecks the saved revision and exclusions after the host
policy gate. Attempts consume the byte budget even when execution fails.
The user can select 1–100 targets, up to 1 TiB and 7–3,650 retention days through
the native contract; UI presets are intentionally narrower. The worker stops
starting targets after five minutes; each operation keeps its own deadline.
The headless process has a 15-minute outer deadline and cancels before exiting.

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
   preparation. The default retention is 30 days; the UI offers 7/14/30/90 days.
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

Sizes are **logical directory bytes**, not guaranteed disk-space savings.
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
- `src/lib/components/HygienePanel.svelte`: review, execution, cancellation,
  recovery, shared-cache inventory and saved preferences within Storage.
- `src/lib/storage/hygiene/`: wire types and preference/ignore-rule helpers.
- `harness/hygiene.html`: rendered interaction regressions using controlled IPC
  fixtures; these are UI tests, not proof of real cache deletion.

Run `npm run test:browser -- --harness hygiene` (or add `--webkit` on macOS),
`npm test -- src/lib/storage`, and the native `storage::hygiene`, `storage_stress`
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

See [the implementation audit](REPOSITORY_HYGIENE_AUDIT.md) for reproduced
failures, fixes, test evidence and remaining platform/install gates. A passing
fixture or mocked IPC test is not proof of unattended operation in an installed
application. No existing user cache was removed or cleanup schedule enabled
while implementing this feature.
