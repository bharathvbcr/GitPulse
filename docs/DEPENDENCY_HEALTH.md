# Dependency health — 2026-09-10

Current follow-up: [2026-09-13 remediation and all 57 candidate references](DEPENDENCY_HEALTH_2026-09-13.md).
CI and release hardening, and the latest framework port re-check:
[2026-10-08](#ci-and-release-hardening--2026-10-08).
Lucide is now 1.45.0; the dated results below describe the earlier migration.

The Health panel report of 2026-09-10 (44 CodeQL alerts, three outdated npm
packages, cargo discovery capped at 24 of 754) is resolved in this worktree
as follows. The Rust GTK migration from 2026-09-08 remains in place. Neither
RustSec advisory is ignored or filtered out.

## Code scanning

`.github/codeql/codeql-config.yml` excludes `src-tauri/framework/**`. Attach it
to CodeQL default setup with the `github-codeql-config-file` repository
property (or the CodeQL configuration UI). The custom-properties API 404s on
this personal repository, so the file is in-tree and the attachment is a GitHub
Settings step; until it is attached, the framework alerts are dismissed as
false positives / used-in-tests so they do not return on an unchanged tree.

| Alert | Resolution |
| --- | --- |
| [js/incomplete-sanitization](https://github.com/bharathvbcr/GitPulse/security/code-scanning/26) in `ignoreRule` | The gitignore sanitizer now escapes `\` as well as `!*?[]# ` (CodeQL: "This does not escape backslash characters in the input."). Copy-ignore still refuses paths that contain `\` as an ambiguous shape. |
| 40× [rust/access-invalid-pointer](https://github.com/bharathvbcr/GitPulse/security/code-scanning/27) in `libappindicator-sys` | Bindgen `#[test]` offsetof probes (`&(*null).field`). Excluded with the local framework ports. |
| 3× [rust/insecure-cookie](https://github.com/bharathvbcr/GitPulse/security/code-scanning/67) in WRY | Cookie builders copy `is_secure()` / `IsSecure` / `isSecure()` from the webview. Forcing `Secure=true` would break HTTP localhost cookies. Excluded with the local framework ports. |

`src-tauri/src` and `src-tauri/vendored` stay in the scan.

## npm toolchain

- `@lucide/svelte` is locked at **1.44.0**.
- `vite` is locked at **8.3.0**.
- `@types/node` is locked at **26.5.1** (not the Node 22 `latest` dist-tag).
- The `@typescript/native-preview` nightly is replaced by stable **TypeScript
  7.0.2**, installed under the `@typescript/native` alias.
- `typescript` resolves to Microsoft's `@typescript/typescript6` **6.0.2**
  compatibility package, which delegates to classic TypeScript **6.0.3**.
  `svelte-check` 4.7.6 and the enum contract tests need that JavaScript API.
- `bun run typecheck` invokes the native package's CLI explicitly
  (`node node_modules/@typescript/native/bin/tsc`) rather than bare `tsc`. When
  tested with npm 11.17.0, the compatibility package's hoisted `@typescript/old`
  could own `node_modules/.bin/tsc`, and bare `tsc` selected 6.0.3; the explicit
  path avoids depending on which package manager laid out `.bin`.

This follows Microsoft's [TypeScript 6/7 compatibility setup](https://devblogs.microsoft.com/typescript/announcing-typescript-7-0/#running-side-by-side-with-typescript-6-0)
and Svelte's [native compiler alias support](https://github.com/sveltejs/language-tools/pull/3073).
An `npm outdated --all` inventory can still list the intentional transitive
TypeScript 6 API. The native CLI and classic compiler API serve different tools.

## Rust migration

| Reported issue | Resolution |
| --- | --- |
| [RUSTSEC-2024-0370](https://rustsec.org/advisories/RUSTSEC-2024-0370.html), `proc-macro-error 1.0.4` | GTK3 macros 0.19 and GLib macros 0.22 remove the dependency. Neither `proc-macro-error` nor its also-unmaintained `proc-macro-error2` replacement is in the lockfile. |
| [RUSTSEC-2024-0429](https://rustsec.org/advisories/RUSTSEC-2024-0429.html), `glib 0.18.5` | The graph contains a single GLib instance, published 0.22.9, which includes the upstream string-iterator soundness fix. |

[GTK3 0.19](https://github.com/gtk-rs/gtk3-rs/blob/0.19.0/CHANGELOG.md)
uses GLib 0.22. A leaf-only update failed because the existing Tauri, WRY, TAO,
Muda and WebKitGTK manifests required GTK/GLib 0.18. The authorized migration
ports those consumers together, including JavaScriptCore and the optional
AppIndicator chain, so all their native `links` dependencies and Rust types agree.
The application continues using GTK3 and WebKitGTK 4.1.

`src-tauri/framework/` contains eleven local consumer snapshots (thirteen
until 2026-10-08, when javascriptcore-rs 2.0.0 let its two ports retire), with
upstream revisions/archive checksums, changed-file provenance, licenses and
full file hashes in `PATCHES.json`. `changes.patch` presents the reviewable port
after Tauri's workspace inheritance is resolved: **587 lines added / 113
removed** across the port, within **556** retained framework files. The eight
registry archives were verified against their crates.io checksums and copied
source; the three Tauri snapshots were checked against their pinned Git objects.

The ported Tauri crates are at the `tauri-v2.12.1` tag,
`30da1fd6e17de6107ecc850c95dfb16b5729f2dd`, on WRY 0.57.0 and Muda 0.20.0, and
the five unchanged Tauri family crates use the same revision. Registry plugins
and patched crates share one Tauri runtime, and the npm `@tauri-apps/*` packages
stay on the same major/minor as their crates, which `tauri build` requires. The
existing `urlpattern 0.6.0` migration is retained; unmaintained `unic-*`
packages have not returned. Crate versions are not spoofed. See
`src-tauri/framework/README.md` for maintenance and removal criteria.

GLib removed its channel API. TAO's replacement uses `async-channel` 2.5,
already in the dependency graph, with receivers on the owning main context.
It preserves ordered, nonblocking sends, yields after 64 messages, and aborts
receivers when the event loop drops. Tests cover order, thread affinity,
closed-channel errors, pending-message cancellation and fairness under load.

Security impact: the migration removes the reported unsound implementation and
abandoned macro dependency. It preserves application authorization, credentials,
network policy, macOS private-API settings and WebKit sandbox configuration.
The local protocol handler belongs only to the native integration test.

## Uncapped audit scope — verified

The application lockfiles were audited directly, without the Health UI's
artifact-discovery cap, platform filtering or advisory exclusions.

2026-09-08:

- `npm audit --json`: **0 vulnerabilities**, 174 dependencies in its metadata.
- `npm outdated --json`: **{}**, exit **0**, for direct dependencies.
- `cargo audit --file src-tauri/Cargo.lock --deny warnings --json`:
  **544 dependencies**, **0 vulnerabilities**, **0 warnings**, exit **0**.
- RustSec database: `bf25f6575a93a35f30796c65c0ed91bee7fa19fd`, 1,242 advisories,
  updated `2026-09-08T11:58:15+02:00`.

2026-09-10 follow-up (this change): Dependabot **0** open alerts. `cargo audit
--file src-tauri/Cargo.lock --deny warnings` exit **0**. `npm audit` **0**
vulnerabilities. CodeQL alerts **27–66** dismissed as used-in-tests, **67–69**
as false positives. Alert **26** stays open until this branch is scanned.
Direct npm refreshes: `@lucide/svelte` 1.44.0, `vite` 8.3.0, `@types/node`
26.5.1. `npm outdated` still lists `@types/node` only because `latest` is the
Node 22 dist-tag (22.20.2); wanted and current are 26.5.1.

This covers the resolved application dependencies in these lockfiles. It does
not claim coverage of every generated artifact or defects absent from the
advisory databases.

## Verification

2026-09-10 (this change, macOS): `escapeGitignoreLiteral` / `ignoreRule` tests
and the advisory lockfile contract passed (14 tests). Vite **8.3.0** production
build succeeded; entry chunk **683.64 kB** under the 780 kB budget. `npm run
typecheck` and `npm run check:release` passed. `npm run check` (svelte-check)
was not used as evidence: the worktree also has unrelated TaskBoard edits that
fail svelte-check independently of this change. Browser hygiene harness and
`npm run ci:local` were not re-run. GitNexus was stale versus HEAD; DevMap
answered `ignoreRule` with two deterministic callers (`copyIgnore`, the unit
tests) and `walk_incomplete` (index-wide unresolved sites). Source grep agreed.

The GTK migration verification below is unchanged from 2026-09-08.

The new dependency contracts failed against the old versions. The native CLI
selection assertion also failed against bare `tsc`. The framework integrity
regressions failed before the checker was extended, for edited, missing and
unrecorded files, and for an entirely missing framework tree referenced by the
application manifest. All now pass. A clean `npm ci` validated the compiler aliases.

**macOS:** `npm run ci:local` completed with exit **0** after the Rust migration.
Svelte reported zero errors and warnings; TypeScript 7 checking, IPC/types,
release/workflow, vendor integrity/schema checks, formatting and Clippy passed.
All **24 browser regressions**, **393 frontend files / 5,184 tests**, and the
production Vite build passed. One frontend test remains skipped. Rust summaries
reported **2,059 passed / 0 failed / 9 ignored**; coverage was **84.72%**
(52,591/62,078 lines). Frontend coverage was **95.99% lines / 88.59% branches**.
Ignored Rust cases require manual performance, deep fuzzing, network or explicit
real-repository environments.

After the final missing-tree regression and checker fix, the two dependency
contract files passed all **19 tests**, TypeScript checking passed, and the
integrity check verified all **21** sibling/framework crate snapshots.

**Linux:** an ARM64 Ubuntu **22.04** container used Rust **1.98.0**, system GLib
**2.72.4**, GTK **3.24.33**, and WebKitGTK **2.50.4**. `cargo check --locked
--all-targets` passed, as did compilation with `--features tauri/tray-icon`.
Linux `cargo clippy --locked --all-targets -- -D warnings` also passed.
The real native tests ran as an unprivileged user under Xvfb and a session bus:

- Both `gtk_dispatch` regressions passed.
- `gtk_runtime` loaded local HTML through Tauri's custom protocol, executed
  JavaScript in WebKit, received the expected Unicode title, rebuilt menus,
  created a transient child, and observed window destruction and clean exit.
  GLib iterator cases cover empty arrays, UTF-8, both directions, exhaustion
  and oversized skips.
- `native_menu_main_thread` passed all **7** checks.

The same iterator assertion block was also compiled into an isolated probe
against published GLib **0.22.9** and passed with `cargo run --release` on Linux.

The Linux test compile initially exceeded the VM's 2 GB memory limit and was
rerun successfully with an 8 GB VM and a 6 GB container limit. The first WebKit
fixture waited on `about:blank`, which Tauri intentionally does not load; the
corrected fixture serves actual HTML. Neither failure was recorded as a pass.
The temporary container was removed and the VM restored to its original
stopped state and 2 GB memory allocation.

The local runtime emits GTK accelerator warnings during menu rebuilding and
container portal-service diagnostics. Its construction and lifecycle assertions
pass. Physical shortcut activation, a Wayland session, Windows native execution,
Linux x86_64 execution, and distribution packaging/signing were not tested here.
CI supplies Xvfb/session buses for Linux native tests in CI, coverage and release
validation. The complete Rust suite was run on macOS; Linux execution focused on
the migrated native stack, with all targets compiled.

Navigation limitation: this worktree has no queryable DevMap generation; a
build attempt exited 1 without diagnostics. The sibling GitNexus index was used
with source comparison for existing scripts (vendor checker impact LOW). Newly
vendored framework symbols were absent from that index; source callers and
Cargo/compiler checks provided the migration's impact evidence.

## CI and release hardening — 2026-10-08

### Rust advisories and licences in CI

`cargo deny check` runs on `src-tauri` in `.github/workflows/bun-audit.yml`
(job `cargo-deny`) on push and pull request when a Cargo manifest, the lockfile
or `src-tauri/deny.toml` changes, and on the Monday 06:00 UTC cron. The policy
denies vulnerabilities, unsound, unmaintained and yanked crates (the strict
`cargo audit --deny warnings` equivalent) with nothing ignored, allows only the
licences the graph actually uses (MPL-2.0 is the one file-level copyleft, for
cssparser, selectors, dtoa-short and option-ext), and restricts sources to
crates.io plus the pinned Tauri Git revision. Duplicate versions are reported
as warnings (29 today), not gated. Path dependencies — every
`src-tauri/framework` port — are never matched against RustSec; that is a
cargo-deny/cargo-audit property, so a port refresh still needs the manual
advisory check in `src-tauri/framework/README.md`.

Locally, cargo-deny 0.20.2: `cargo deny --manifest-path src-tauri/Cargo.toml
check` → `advisories ok, bans ok, licenses ok, sources ok`, exit 0. Removing
MPL-2.0 from the allow-list made `check licenses` fail with exit 4 and 10
rejections, so the licence gate is live.

### Release SBOMs and checksums

A new `attest` job in `release.yml` runs after every platform has uploaded and
before `verify`. It adds three assets, which the exact-asset manifest
(`scripts/check-release-assets.mjs`) now requires:

- `GitPulse_<v>_sbom.spdx.json`: Syft (`anchore/sbom-action`) over the checked-out
  tree with installed `node_modules`. Syft's `bun.lock` reader alone produced
  28 npm packages out of 179 locked entries, so the installed-package cataloger
  is enabled. Locally (Syft 1.54.1) that gave 523 Cargo, about 110–120 npm, 41
  GitHub Actions and 11 Go entries.
- `GitPulse_<v>_sbom.cargo.cdx.json`: `cargo cyclonedx --target all`
  (CycloneDX 1.5; 550 components locally). The job fails if cargo rewrites
  `Cargo.lock`.
- `GitPulse_<v>_SHA256SUMS.txt`: `sha256sum -c` format, one line per installer
  and SBOM. Installer lines are GitHub's own `digest` for the stored bytes; SBOM
  lines are hashed locally and must equal GitHub's digest after upload, or the
  stage fails. A rerun replaces only these three files.

Check a download with `sha256sum -c --ignore-missing GitPulse_<v>_SHA256SUMS.txt`.

### ARM release targets — decided yes

Linux aarch64 (`ubuntu-22.04-arm`, keeping the 22.04 glibc floor) and Windows
arm64 (`windows-11-arm`, native because `libsqlite3-sys` does not cross-build
to MSVC) are in the release matrix. That brings the release to twelve installers: the
ARM legs add `GitPulse-<v>-1.aarch64.rpm`, `GitPulse_<v>_aarch64.AppImage`,
`GitPulse_<v>_arm64.deb`, `GitPulse_<v>_arm64-setup.exe` and
`GitPulse_<v>_arm64_en-US.msi` (the Tauri 2.12.1 bundler's own arch names). A
release would fail on an ARM leg only after every other platform had
uploaded, so `ci.yml` has a dispatch-only `arm-bundle` job that bundles both
legs on their real runners through the same tauri-action and checks the
installer names against the manifest.

### Property tests

`proptest` 1.11 (dev-only, default features off) drives
`engine::git_reader::tests::parse_status_records_prop`: round trip over every
porcelain v1 `XY` code including worktree-side rename/copy pairs and arbitrary
NUL-free UTF-8 paths, the prefix property for a stream cut at any byte, and
panic-freedom with well-formed records on arbitrary bytes. Run
`cargo test --manifest-path src-tauri/Cargo.toml parse_status_records_prop`;
`PROPTEST_CASES=100000` widens the search. With the worktree-side copy field
deliberately left unread, two properties failed and shrank to
`" C" 0 -> ¡`-style records, so the properties have teeth. They run in CI as
ordinary unit tests.

### Framework port removal criteria

Re-checked against crates.io on 2026-10-08. A port can retire once its
latest upstream release accepts GTK 0.19 / GLib 0.22, or for notify a release
carrying the FSEvents fixes.

| Port | Latest upstream | Upstream requires | Result |
| --- | --- | --- | --- |
| tauri, tauri-runtime, tauri-runtime-wry | 2.12.1 | `gtk ^0.18` | keep |
| wry | 0.57.0 | `gtk ^0.18`, `soup3 ^0.5` | keep |
| tao | 0.37.1 | `gtk ^0.18` | keep |
| muda | 0.21.1 | `gtk ^0.18` | keep |
| webkit2gtk, webkit2gtk-sys | 2.0.2 | `gtk ^0.18`, `glib ^0.18` | keep |
| libappindicator, libappindicator-sys | 0.9.0 (2023) | `gtk ^0.18` | keep |
| notify | 8.2.0, our patched version | — | keep: no release carries the fixes |
| javascriptcore-rs, javascriptcore-rs-sys | 2.0.0 (2026-09-22) | `glib ^0.22` | **retired** |

javascriptcore-rs 2.0.0 matches the retired port except for regenerated
bindings: its `ContextExt` is no longer sealed, and it uses system-deps 9,
which is already in the graph. The port's other two edits (a public
`builders` module and `TypedArrayData<'_>`) were lint fixes needed only
because path dependencies are not capped by `--cap-lints`; a registry crate
is. WebKitGTK, WebKitGTK-sys and WRY now require `2` instead of `=1.1`/`=1.1.2`,
which is recorded in `PATCHES.json` and `changes.patch`. The graph still holds
one GLib (0.22.9) and one GTK (0.19.0), and neither `proc-macro-error` crate.

### CodeQL configuration

Not attachable from here: the custom-property endpoint still returns 404 for
this personal repository. Default setup is `configured` (weekly, last updated
2026-09-22) and had **0** open alerts on 2026-10-08. A person must attach
`.github/codeql/codeql-config.yml` in Settings → Code security, or record that
they decline to.

