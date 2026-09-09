# Dependency health — 2026-09-08

The four reported dependency findings are resolved in this worktree. The Rust
migration uses GTK3 0.19.0, GLib 0.22.9 and Soup 0.9.0, with local consumer
patches until Tauri and its platform crates adopt those bindings upstream.
Neither advisory is ignored or filtered out.

## npm toolchain

- `@lucide/svelte` is locked at **1.43.0**.
- The `@typescript/native-preview` nightly is replaced by stable **TypeScript
  7.0.2**, installed under the `@typescript/native` alias.
- `typescript` resolves to Microsoft's `@typescript/typescript6` **6.0.2**
  compatibility package, which delegates to classic TypeScript **6.0.3**.
  `svelte-check` 4.7.6 and the enum contract tests need that JavaScript API.
- `npm run typecheck` invokes the native package's CLI explicitly. With npm
  11.17.0, the compatibility package's hoisted `@typescript/old` can own
  `node_modules/.bin/tsc`; bare `tsc` selected 6.0.3 during testing.

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

`src-tauri/framework/` contains twelve local consumer snapshots, with upstream
revisions/archive checksums, changed-file provenance, licenses and full file
hashes in `PATCHES.json`. `changes.patch` presents the reviewable port after
Tauri's workspace inheritance is resolved: **190 lines added / 67 removed**
across the port, within **574** retained framework files. The nine registry archives were
verified against the original lockfile checksums and copied source; the three
Tauri snapshots were checked against their pinned Git objects.

Five unchanged Tauri family crates retain revision
`406feea75283545496ef7398c5e2f0fb9b306b64`. Registry plugins and patched crates
share one Tauri runtime. The existing `urlpattern 0.6.0` migration is retained;
unmaintained `unic-*` packages have not returned. Crate versions are not spoofed.
See `src-tauri/framework/README.md` for maintenance and removal criteria.

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
artifact-discovery cap, platform filtering or advisory exclusions:

- `npm audit --json`: **0 vulnerabilities**, 174 dependencies in its metadata.
- `npm outdated --json`: **{}**, exit **0**, for direct dependencies.
- `cargo audit --file src-tauri/Cargo.lock --deny warnings --json`:
  **544 dependencies**, **0 vulnerabilities**, **0 warnings**, exit **0**.
- RustSec database: `bf25f6575a93a35f30796c65c0ed91bee7fa19fd`, 1,242 advisories,
  updated `2026-09-08T11:58:15+02:00`.

This covers the resolved application dependencies in these lockfiles. It does
not claim coverage of every generated artifact or defects absent from the
advisory databases.

## Verification

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
