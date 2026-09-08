# Devmap embedding contracts and independent audit

This change makes GitPulse consume DevCouncil's artifact provider and an
enforced read-only database handle. The same provider can produce a JSON
canvas payload or standalone HTML. Manvi's embedding interface remains the
process boundary for hosts that do not link Rust.

## Module boundaries

| Module | Owner | Integration / replacement |
| --- | --- | --- |
| Parsing, resolution, indexing and migrations | DevCouncil `devmap` | Configure the CLI executable; the writer owns database upgrades |
| Persisted queries | DevCouncil `devmap-query`, `devmap-store` | Link with `default-features = false`; use `Store::open_read_only` for advisory readers |
| Graph/map input, canvas payload and HTML | DevCouncil `devmap_query::host` | Supply an `ArtifactProvider`; configure `FilesystemArtifactProvider` for local files |
| Artifact locations | DevCouncil `devmap_query::paths` | One resolver for `DEVMAP_HOME`, standalone, legacy and fresh repositories |
| Host operations | Manvi `serve` | Configure `Options.Modules`; register or explicitly replace operations at construction |
| Devmap through a sidecar | Manvi `serve.DevmapModule` | Replace its `DevmapClient` implementation; negotiate `hello` and advertised operations |
| Vendored Rust dependencies | GitPulse vendor script | Refresh the complete snapshot, or explicitly select one crate with `--crate` |

For a Rust host, the linked implementation is selected with one dependency
declaration; updating it requires rebuilding the host:

```toml
devmap-query = { path = "../DevCouncil/rust-port/crates/devmap-query", default-features = false }
```

The filesystem provider uses the host's repository root. Its path and byte-limit
configuration belongs to the host, while the shared validation and projection
belong to DevCouncil:

```rust
use devmap_query::host::{ArtifactProvider, FilesystemArtifactProvider};
use devmap_query::viz::VizOptions;

let provider = FilesystemArtifactProvider::for_repo(repo_root);
let payload = provider.code_graph_payload(&VizOptions::default())?;
let html = provider.code_graph_html(&VizOptions::default())?;
```

A custom provider supplies `load_artifact`. The checked default methods validate
its returned value before projection. Hosts must preserve the checked contract
when implementing their own overrides. These are trusted application modules,
not a sandbox for executing untrusted plug-in code. Sidecar replacements must
preserve protocol negotiation, cancellation, readiness, completeness counts and
error semantics; changing an executable path alone does not establish compatibility.

## Reproduced defects and fixes

| Finding | Evidence before the fix | Canonical fix |
| --- | --- | --- |
| Readers disagree with the writer's layout | Fresh and mixed-directory integration tests resolve `.devcouncil` instead of `.devmap` | All three GitPulse readers delegate to `devmap_query::paths` |
| Invalid input looks like an empty successful graph | `malformed graph accepted: null` | Shared artifact provider rejects malformed, incompatible and incomplete input |
| Advisory reads migrate writer state | Status changes `user_version` from 18 to 19 | `Store::open_read_only` refuses incompatible schemas without migration |
| A read-only handle can acquire a writer lease | `lock_writer` creates a `.writer.lock` file on an embedding reader | Refuse writer-lease acquisition before sidecar creation |
| A failed refresh can partially replace dependencies | A late source-manifest failure changes an earlier vendored crate | Prepare and validate selected crates before replacing the current directory |
| Deleted upstream files and inherited manifest changes are missed | Upstream drift checks return clean after both changes | Compare complete normalized source snapshots against the recorded snapshot |
| Linked Codex worktrees cannot locate sibling repositories | Ancestor-only search resolves nonexistent paths | Consult the Git common directory before walking for siblings |
| HTML fit magnifies a singleton graph to fill the screen | Filtering two nodes to one produces a roughly 586-pixel circle; the map preview reports 6735% zoom | Bound degenerate auto-fit at the existing renderer owner |
| Preview badges present capped samples as totals | A producer's 400 shown / 900 total becomes a 256-item list with no completeness fields | Carry shown, available, total and truncation through projection; label legacy totals unknown |
| Malformed entries survive an array-only check | Invalid node/subsystem objects can produce successful empty output | Validate required object fields and modern map completeness at the artifact boundary |
| Custom Manvi modules panic during construction | Typed-nil modules and a panicking `Configure` crash the server | Refuse invalid modules and return a configuration error before reading protocol input |
| Custom adapters return contradictory or unidentified results | Empty database paths, non-positive generations and impossible result counts are accepted | Share versioned readiness, snapshot-identity and completeness validation across stock and custom adapters |

Regression coverage includes standalone/legacy/mixed and missing selected
layouts, invalid JSON shapes, incompatible schemas, incomplete exports,
oversized artifacts, source symlinks, failed and concurrent vendor updates,
read-only stores, missing/corrupt databases and concurrent reader attempts.
The upstream provider tests also exercise substitution by an in-memory provider.

Security impact: advisory database access is narrowed to SQLite read-only
connections. Artifact readers and vendor inputs refuse non-regular files and
symlinks. Application authorization and network access are unchanged. Portable
filesystem preflight checks are not a proof against adversarial replacement of
path components during an open; the provider requires host-controlled storage.
Cargo workspace and crate manifests are limited to 1 MiB and read through a
checked descriptor. Refresh and comparison refuse FIFOs, symlinks, oversized
manifests and observed size changes while preserving the previous vendor tree.

## Update and validation workflow

1. Validate the canonical upstream source and its feature-disabled embedding
   configuration. Keep an explicit source snapshot when another task is editing
   that checkout.
2. Run `npm run vendor` with explicit `GITPULSE_DEVCOUNCIL_ROOT`,
   `GITPULSE_MANVI_ROOT` and `GITPULSE_MARKDEV_ROOT` when selecting worktrees.
3. Run `npm run vendor:check` against the same roots. A comparison against a
   different worktree is a different provenance check. Missing upstreams are
   reported as unavailable; local edits and upstream drift are separate results.
4. Run `npm run ci:local`, the upstream module tests, and real CLI/sidecar
   integration checks. Preserve failed, skipped and platform-specific results.
5. Build/install or publish separately when requested. A source test pass does
   not replace an installed application, restart a running MCP server, or prove
   Windows/Linux/native-webview behavior.

The recorded source commit in `VENDOR.json` identifies the upstream base;
per-file hashes identify the exact imported bytes, including uncommitted fixes.
An isolated upstream module must be integrated into its owner before treating
that base commit alone as reproducible provenance for the update.

## Verified integration evidence (2026-09-08)

All `ci:local` stages passed on the final implementation:

| Check | Final result |
| --- | --- |
| Frontend and script tests | 374 files; 4,851 passed, one skipped |
| Rust tests | 55 binaries; 1,929 passed, eight ignored |
| Frontend coverage | 95.84% lines; 88.46% branches |
| Rust coverage | 83.86% lines |
| Required coverage floors | 90% frontend lines; 85% frontend branches; 80% Rust lines |
| Static and build gates | IPC, shared types, release versions, workflows, Svelte, TypeScript, production build, Rust format and strict Clippy passed |
| Focused integration | Six GitPulse embedding regressions; nine executable HTML behavior tests; all selected upstream vendor bytes matched |

An earlier complete `npm run ci:local` invocation passed before the final
review fixes. The final invocation found a missing CONTRIBUTING entry for the
new HTML contract test, then a trailing blank line in the upstream Rust
template. Both were corrected; the already-passing frontend/build stages and
the subsequent formatting/Clippy/Rust/coverage stages together verify the
final implementation. No check or assertion was weakened.

The ignored Rust checks were the existing network update check, an explicit
deep graph-layout fuzz run and six real-repository graph/pulse smoke tests
requiring configured repository paths. The frontend skip was the existing
optional `docs/PROMO.md` count check because that local draft is absent. These
are not counted as passes. Svelte retained one existing canvas
accessibility warning; the build retained its existing chunk-size advisory.

A separate real integration fixture contained 66 Python files. The current
`devmap` CLI built its standalone schema-19 store, then the newly built
`gitpulse-mcp` served status, search and impact over stdio using negotiated
protocol metadata. Status reported generation 1; search returned two matching
symbols; impact returned all 65 edges. The database SHA-256 was unchanged by
those advisory requests.

Browser checks exercised standalone graph rendering, filtering, label toggling,
repository-map language filtering and detail selection. The hostile title
rendered literally, with no injected image, inline event attribute, JavaScript
dialog or console error. After the renderer correction, the filtered singleton
remained a normal visible node; the map's singleton auto-fit stopped at 400%.
The capped fixture visibly displayed `unwired 256 of 900`, while its legacy
unreachable list displayed `0 shown · total unknown`.

Manvi's isolated implementation passed the complete Go suite both serialized
and with default parallel scheduling, strict `go vet`, focused race tests,
50 repetitions of module/adapter tests and 20 repetitions of advanced-query
tests. A live sidecar served `hello`, status and all four advanced query kinds
(`explore`, `impact`, `trace`, `affected`) against a schema-19 store. Invalid
depth returned `E_BAD_REQUEST`; a missing devmap executable returned
`E_DEPENDENCY`, after which the same session still answered `hello`.

An earlier loaded parallel Go run failed two existing probe tests: one reported
`expected exactly one execution within the cooldown, got 0`; the other hit its
200 ms probe timeout. Those tests use a shorter subprocess deadline than the
production 10 seconds. The focused, serialized and later parallel runs passed;
the observed load sensitivity remains recorded rather than being erased by
the reruns. No probe timeout or test assertion was weakened.

The canonical GitPulse and Manvi checkouts were being edited by another task.
This task's GitPulse changes remain in its supplied worktree; DevCouncil and
Manvi changes were developed in isolated worktrees to preserve that concurrent
work. Integration into the shared branches, installed application replacement,
running-sidecar restart and release publication are separate, unverified steps.
Windows, Linux and the native macOS webview were not exercised. These results
establish the tested contracts, not proof that every application or possible
failure mode has been covered.

## Review scope

The GitPulse implementation, tests, contributing guide and vendored snapshot
add 2,596 lines and remove 265, excluding this audit report. The isolated
DevCouncil change adds 1,608 and removes 22. The incremental Manvi hardening
patch adds 227 and removes 33 relative to the captured integration snapshot.
These figures overlap because GitPulse contains vendored DevCouncil source.

GitNexus's final comparison against `main` reported 17 tracked files, 63
symbols and two affected execution flows at medium aggregate risk. The preview
builder's individual upstream impact was high (nine direct callers). New
untracked modules/tests were also inspected directly; the graph result is not
complete coverage of those files.

Portable review artifacts were saved as `/tmp/devcouncil-devmap-modules.patch`
and `/tmp/manvi-devmap-audit.patch`. The former reverses cleanly against the
isolated DevCouncil worktree; the latter dry-runs cleanly against its exact
captured pre-existing Manvi integration snapshot. Applying either to a newer
shared checkout requires comparing that checkout's intervening changes.
