# Reusable modules and coordinated updates

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
| Task/lease reads and credential redaction | Manvi `dc-store`, `dc-verify`, `dc-glob` | Vendored Rust libraries; task store remains read-only in GitPulse |
| Policy, local-model discovery and chat preparation | Manvi `serve` | Protocol v1 in `src-tauri/src/harness/`; advertised `hello.ops` capabilities |
| Code intelligence through a process boundary | Manvi `serve.DevmapModule` | Available to other hosts through `devmap.status` and `devmap.query`; GitPulse retains its in-process readers |

The module owner implements behavior once. Host adapters translate their UI or
transport types; they do not copy policy decisions, schema migrations, parsers
or HTML assets into independent implementations.

## Integrate or replace a module

For a Rust host, a dependency declaration selects the existing query library:

```toml
devmap-query = { path = "../DevCouncil/rust-port/crates/devmap-query", default-features = false }
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
tool and a malfunctioning configured tool are different states.

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
