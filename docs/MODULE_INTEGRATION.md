# Reusable modules and coordinated updates

**DevCouncil** is **components and modules**. **Manvi** wraps those
components into a harness. **GitPulse** uses Manvi for policy, workbench, and
agent hosting, and DevCouncil components for code intelligence and related
analysis. The stack is modular: update one module at a time, or take only the
subset an app needs.

DevCouncil supplies reusable Rust libraries and a process API. Manvi supplies
a Go embedding API and a language-independent NDJSON sidecar. They are not
dynamic shared-library plugins: changing a linked Rust or Go implementation
requires rebuilding its host. A compatible sidecar can be replaced by changing
the configured executable and restarting it.

## Choose the integration boundary

| Need | Canonical owner | Integration in GitPulse |
| --- | --- | --- |
| Parse, resolve and build a code index | DevCouncil `devmap` CLI | Bounded CLI calls in `src-tauri/src/devmap/cli.rs` |
| Read and query a persisted index | DevCouncil `devmap-store`, `devmap-query`, `devmap-resolve` | Vendored Rust path dependencies with `default-features = false` |
| HTML map and graph projection | DevCouncil `devmap-query` | Same upstream projection; HTML assets live inside the crate |
| Repository execution task/lease reads and credential redaction | DevCouncil `dc-store`, `dc-verify`, `dc-glob` | Vendored Rust libraries (Manvi wraps the same modules in the harness); execution tasks and leases remain read-only in GitPulse |
| Profile workspaces, tasks, briefs and run records | Manvi wrap of DevCouncil `dc-store` workbench API | `cmd_workbench_request` performs typed CRUD against a separate profile database; Manvi owns the wrap's schemas, revisions and receipts |
| Task suggestions and managed agent hosting | Profile Manvi host | GitPulse presents proposals and run controls; Manvi owns provider execution and request/decision delivery |
| Policy, local-model discovery and chat preparation | Manvi `serve` | Protocol v1 in `src-tauri/src/harness/`; advertised `hello.ops` capabilities |
| Code intelligence through a process boundary | Manvi `serve.DevmapModule` | Available to other hosts through `devmap.status` and `devmap.query`; GitPulse retains its in-process readers |

The module owner implements behavior once. Host adapters translate their UI or
transport types; they do not copy policy decisions, schema migrations, parsers
or HTML assets into independent implementations.

## Integrate or replace a module

For a Rust host, a dependency declaration selects the existing query library:

```toml
devmap-query = { path = "../DevCouncil/rust/devmap-query", default-features = false }
```

That line is suitable for a local multi-repository workspace. A distributed
host must package the dependency closure or pin its Git source. GitPulse's
vendor command packages that closure and resolves workspace inheritance, so a
standalone clone builds without sibling checkouts. Updating a linked crate
still requires rebuilding GitPulse; an ABI-incompatible library cannot be
hot-swapped by changing a path.

For a Go host, Manvi's `serve.Options.Modules` accepts modules implementing
`Configure(*serve.Router) error`. `Router.Register` adds a named operation;
`Router.Replace` explicitly replaces an existing one. The router is scoped to
the server instance. `serve.DevmapModule.Client` is an interface, allowing a
host to provide the stock devmap process client or an adapter implementing
the same status and advanced-query contract. See Manvi's
`manvi/serve/module.go`, `manvi/serve/devmap.go` and embedding documentation
for the compiled examples and configuration guarantees.

For a non-Go host, start `manvi serve --posture host`, send one JSON request
per line, and negotiate `hello` before using advertised operations:

```json
{"id":"hello-1","op":"hello","params":{"protocol":1,"host":"my-app"}}
```

GitPulse already supports executable overrides:

```sh
GITPULSE_DEVMAP_BIN=/absolute/path/devmap GITPULSE_MANVI_BIN=/absolute/path/manvi npm run tauri dev
```

Its saved tool configuration serves GUI launches that do not inherit the
shell's environment. Explicit broken overrides are errors; a missing optional
tool and a malfunctioning configured tool are different states. Setup can
install DevMap alone, the analysis suite, or the full DevCouncil host from the
documented scripts (copy, or Run in Terminal). Disable hides a tool without
deleting it; Uninstall removes only a binary GitPulse placed in its app bin
directory. There is no uv / Python install path.

## Per-repository initialization

Opening a repository is the whole setup. When a repository is trusted and
becomes an open tab, GitPulse does three things without asking, because none of
them can appear in that repository's `git status`, its diff, or a commit:

1. **Ignore hygiene.** `devmap build` leaves its state directory untracked and
   nothing in the toolchain ignores it, so an automatic index would put a
   permanent `?? .devmap/` in the view this application exists to render. The
   resolved state directory — whichever `devmap_extract::paths` selects, so a
   legacy `.devcouncil` tree is handled where the CLI actually writes — is
   added to `$GIT_COMMON_DIR/info/exclude`, which is per-clone, never
   committed, and reaches linked worktrees through the common directory.
   `.gitignore` is deliberately not touched: it is tracked content in a
   repository the application does not own. An existing project rule that
   already covers the directory is honoured rather than duplicated, the pattern
   is anchored to the repository root so a nested directory of the same name
   stays visible, and git is asked again after the write — a pattern a later
   rule re-includes is reported as a refusal, never as a success.
2. **The index itself.** The live gate escalates to `devmap build --manifest`
   whenever a consumer artifact is absent or the CLI sets `rebuild_required`.
   A plain `devmap build` writes the database only, so the incremental path
   could previously run to completion and leave Code → Map with no document —
   including on the first build of every newly opened repository. Store
   freshness is not evidence about the artifacts beside it: a repository whose
   `repo_map.json` is deleted still reports `is_fresh: true`.
3. **The workspace registry.** The open-tab set is written into the active
   repository's registry, which is what cross-repository symbol search and
   import-link candidates read.

Initialization writes no state for a tool that is not installed, and writes
none at all when the state directory could not be hidden — creating exactly the
untracked directory the first step failed to prevent would be worse than
declining.

That refusal is remembered, but only as far as it stays true. A run that
stopped short because `devmap` was absent still *succeeds*, so treating it like
a finished one made installing devmap a no-op for every repository already
open: cross-repository search kept answering "nothing" for the rest of the
session. The tool probe — the one call every install path and every tool-aware
surface makes — drops those results as soon as it sees devmap present, which
covers an install made in this app and one made in a terminal alike. Shortfalls
an install cannot fix, such as a refused exclude, are deliberately left alone
by that signal and retried when the open-tab set next changes; clearing them on
every probe would re-initialize an unfixable repository each time a panel
opened.

## Indexing the whole fleet, once

The automatic path indexes what you are looking at. That is the right default —
`devmap build` saturates its cores, and nobody wants eleven graphs rebuilt
because eleven tabs are open — but it serves the first run badly: installing
devmap against a workspace that is already full of tabs leaves every one of
them un-indexed until it is visited. **Fleet → Index all** does them in one
pass, one at a time, abortable between repositories.

Sequential is not timidity. A build takes the store's writer lock, which
`devmap serve` also takes, so overlapping builds queue on the lock instead of
finishing sooner. Repositories that are already current are reported as such
rather than rebuilt, a build that fails carries the kernel's own reason, and a
sweep that could not run at all — no devmap — says that for every repository
instead of reporting zero indexed. The sweep never claims the whole fleet when
it did part of it: the summary carries both numbers whenever they differ.

## Where a `devmap serve` daemon is running, the gate stands down

`devmap serve` watches the tree itself, enqueues what changed, and rebuilds
inside its own process. Where one is serving, GitPulse's poll-and-build loop is
not merely redundant — it is a *second writer* for the same store lock. The
live gate therefore probes the daemon over its IPC endpoint (one JSON line in,
one out; the endpoint path comes from `serve --print-socket-path`, never
re-derived here, because two implementations of that hash which disagree each
start a daemon against the same store and neither side can see it) and answers
`skip_daemon` when it finds work already queued.

Three deliberate limits, each measured against a running daemon rather than
assumed:

* The stand-down is gated on **queued work**, not on the daemon being alive. If
  the CLI says the store is stale and the daemon reports nothing pending, the
  two disagree — the daemon's watcher did not see the change — and standing
  down would leave the index stale with nobody rebuilding it.
* **Schema decisions stay with the CLI.** A daemon's `status` reply carries
  `is_fresh`, `degraded_reason` and the generation counts, but not
  `schema_outdated`, `rebuild_required` or `schema_relation`. Trusting it for
  everything would blind the gate to exactly the state it exists to see.
* **Consumer artifacts stay with us.** The daemon persists generations to the
  store; it never writes `repo_map.json`. A missing artifact still takes
  `build --manifest`, whatever the daemon is doing.

On Windows the daemon speaks a named pipe, and GitPulse has no client for it
yet; the probe says so rather than reporting "no daemon", because those are
different claims.

A schema-behind store is migrated by a full rebuild when, and only when, the
CLI's `rebuild_required` says a rebuild fixes it. `newer`, `foreign` and
`unsupported` stores are refused with the CLI's own remedy, because rebuilding
a newer store would downgrade a database a newer reader owns. The degraded-text
fallbacks that let an older binary report an obsolete payload never authorize a
rebuild of an *outdated schema*: that field is the only thing that carries
migratability.

## Agent-host integration is separate, and previewed

`devmap integrate <host>` writes `AGENTS.md`, `CLAUDE.md`,
`.cursor/rules/devmap.mdc`, project MCP entries, skills and hook configuration
— tracked content — plus a machine-wide MCP registration in the user's home
directory. None of that happens because a tab was opened. **Settings → Agents**
runs `--dry-run` and renders the resulting change list; applying is a separate
click. The two counts stay separate, because the command has no flag to write
the project assets without the global registration and agreeing to add guides
to a project is not agreeing to edit `~/.claude.json`. A guide the kernel
reports as `not_ours` is a file the user wrote; it is surfaced as protected and
left alone.

## What is installed, and whether it is what answers

`devmap` and `manvi` are the two binaries GitPulse installs and manages. The
setup wizard also offers `dcstore`, `dcverify`, `dcgrep` and the Go host, and
those are probed too — by running them, not by stat-ing a path. `dcstore` is
not cosmetic: `manvi serve --workbench-db` resolves it from `PATH`, so the
profile workbench and managed runs fail without it.

Three of those components reject `--version`, so presence and version are
reported as separate facts and a component that ran but cannot name itself is
not shown the same as one that is absent. `devmap doctor`'s installation
warnings are surfaced beside the inventory: every `*_warning` field the payload
carries, ordered by consequence — the ones that change *which binary answers*
first — with any field GitPulse does not recognise kept at the end rather than
dropped. That sweep is the contract, not a list of field names: a hand-written
list is how `stray_state_warning` fired on a user's machine into a panel that
said `devmap doctor` found nothing to report. Each warning is bounded for
display and says so with both numbers when it is shortened, because
`stale_server_warning` enumerates one process id per running `devmap mcp` and
is unbounded at the source. Doctor needs a trusted repository to run in;
without one the panel says health was not checked rather than showing an empty
list.

Doctor is spawned with the child `PATH` the rest of the app uses — the
inherited value plus the GUI-launch fallback dirs. It has to be: doctor
resolves the bare `devmap` command that host MCP configs name against *its own*
`PATH`, so a Dock-launched GitPulse handing it launchd's minimal
`/usr/bin:/bin:/usr/sbin:/sbin` made it report the binary GitPulse had just
resolved as missing, under a heading that reads as the user's broken install.

## Compatibility and evidence

Devmap's JSON status includes `host_contract_version`, `binary_version`,
`expected_schema_version`, `schema_relation`, `reader_ready`, `query_ready`,
and `capabilities`. Require a compatible store and query readiness before
interpreting query results. A missing, stale, foreign or newer database is not
an empty successful query. Preserve response counts and truncation/incomplete
walk fields when presenting results. A newer database requires a matching or
newer reader; do not downgrade the database to fit an older binary.

`query_ready` establishes schema compatibility and a committed generation;
freshness is separate. Check `is_fresh` and show `degraded_reason` even when a
query can run. Consumers requiring current results must refuse a stale index.

Status also carries nullable `source_freshness` and `analyzer_freshness`.
`true` means that check passed, `false` means a mismatch was observed, and
`null` means it was not verified. A parser-free MCP reader checks source bytes
without certifying a grammar identity it does not contain. Overall `is_fresh`
requires both checks to pass, no pending edits, and no store degradation.
Retain `freshness_reason` in GitPulse responses (the CLI calls it
`degraded_reason`); analyzer uncertainty must not hide a source mismatch.

Repository-map marker metadata includes `inventory_complete`,
`inventory_source`, `inventory_entries_examined`, `inventory_files_total`, and
`inventory_unreadable_count`. A computed inventory can still be incomplete.
Git discovery is bounded to 50,000 eligible paths and has no depth ceiling;
non-Git discovery retains depth, directory, frontier, entry and cooperative
time limits. The unreadable-path list is capped at 64 while its count reports
all observed failures. Completeness applies to the declared marker policy,
not to parser, call-graph, or dynamic-language coverage.


The host contract is a separate compatibility axis from the database schema,
the code-graph JSON schema and the application's release version. Manvi's
NDJSON protocol is another independent axis. Additive result fields can be
ignored; unsupported operations and incompatible protocol majors must be
reported. An installed binary reporting the right release version alone does
not prove it was built from the current worktree.

Artifact location is also part of integration. GitPulse delegates database,
repository-map and graph paths to `devmap_extract::paths`, the CLI's canonical
owner. Its precedence is `DEVMAP_HOME`, an existing `.devmap`, an existing
`.devcouncil`, then the default `.devmap`. A legacy-only repository continues
to work. If both directories exist, the standalone index wins consistently
across the CLI, desktop and MCP readers. Configure an override for the intended
repository; it selects the artifact location explicitly.

Security impact: the new update path refuses symlinked/non-regular module
sources, preventing unintended content outside a crate from being packaged.
No application permission or default policy is widened. A trusted embedding
application that deliberately replaces a Manvi policy handler owns that policy
decision; remote NDJSON requests do not configure module registration.

GitPulse also bounds sidecar output while no request is running. Its canonical
stdout pump retains at most eight frames (32 MiB at the per-frame ceiling),
plus the frame currently being read. Queue overflow kills and reaps the child
instead of leaving an unbounded buffer behind an apparently idle connection.

## Update the toolchain

1. Validate and commit the canonical DevCouncil and Manvi sources first. Their
   own workspaces run their library tests; GitPulse's vendored copies omit
   upstream test targets and development dependencies explicitly.
2. From GitPulse, synchronize all crates with `npm run vendor`. Roots are
   discovered from sibling checkouts, or selected with
   `GITPULSE_DEVCOUNCIL_ROOT`, `GITPULSE_MANVI_ROOT` and
   `GITPULSE_MARKDEV_ROOT`. Use
   `npm run vendor -- --crate=devmap-query` for one deliberate scoped update.
3. Run `npm run vendor:check` without allowing drift. This checks local hashes,
   the full upstream file set (including deleted files), and the resolved Cargo
   manifests. `comparable: false` means an upstream comparison could not run.
4. Refresh the installed devmap/Manvi binaries from those validated sources.
   Run `npm run check:vendor-schema`, then `npm run ci:local` against the final
   GitPulse snapshot.
5. Rebuild the native app and refresh MCP with `npm run mcp:install`. Verify
   `npm run mcp:doctor`, installed file hashes and a real code-intelligence
   query. Restart long-lived host processes to load the replacement binaries.

The vendor command prepares all selected crates before touching the current
tree. A preparation failure preserves it byte-for-byte. A directory lock
rejects concurrent updates; the previous tree is restored if final installation
fails. A process killed during the two directory renames can leave recovery
state in `src-tauri/.vendor-lock/previous`; inspect and restore that tree before
removing the lock. This is recoverable replacement, not a claim of power-loss
atomicity or a transaction shared with Cargo readers.

The canonical full gate intentionally permits upstream drift while checking
local copies, because unrelated sibling edits may be in progress. That is why
the explicit strict vendor check above remains a required coordinated-update
step.
