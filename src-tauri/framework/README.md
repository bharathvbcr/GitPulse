# GTK3 0.19 / GLib 0.22 consumer port

These twelve crates carry the GitPulse port of Tauri's Linux GTK consumers.
The application continues to use GTK3 and WebKitGTK 4.1. GTK, GLib, GDK and
Soup come from published maintained releases; this directory contains their
consumers whose upstream manifests still require GTK 0.18.

`PATCHES.json` records each upstream revision or crate archive SHA-256, the
standalone manifest rewrites, the original and patched hashes of changed files,
and the full snapshot's file hashes. `changes.patch` provides the reviewable
delta after Tauri's workspace inheritance is resolved. The files here retain
their upstream package versions and licenses. They are local source patches,
not published upstream releases.

## Changes

- Tauri, its runtime crates, TAO, Muda, WRY, WebKitGTK, JavaScriptCore, and
  AppIndicator agree on GTK 0.19 / GLib 0.22. WebKitGTK and WRY use Soup 0.9.
- Removed GLib trait re-exports are imported from the prelude. WRY uses the
  current `glib::clone!` syntax while retaining weak-reference semantics.
- TAO's removed GLib channels use `async-channel` 2.5, already present in the
  application graph. GTK callbacks remain on their owning main context.
  Dispatch yields every 64 messages; dropping the event loop aborts its
  receiver tasks and closes their channels. The previous unbounded,
  nonblocking, ordered send contract is retained.
- The five Tauri crates that need no GTK changes remain at the original
  `406feea75283545496ef7398c5e2f0fb9b306b64` revision. Cargo patches both the
  registry and Git source so registry plugins share the same runtime types.

The GTK bindings require Rust 1.92 or later. No new native system library is
required by this migration. Ubuntu 22.04 is the native build baseline under
test; release verification is recorded in `docs/DEPENDENCY_HEALTH.md`.

## Maintenance

Unlike `../vendored`, these are intentionally maintained local ports. Do not
refresh them with the sibling repository vendor command. For an update:

1. Obtain the exact upstream Git revision or registry archive in `PATCHES.json`;
   verify its revision or SHA-256 before comparing source.
2. For Tauri, resolve workspace inheritance using `resolveManifest` exported
   by `scripts/vendor-crates.mjs`. Keep the recorded dependency source rewrites,
   omit external examples and upstream dev-dependencies, and retain `src/test`
   because the application uses Tauri's `test` feature.
3. Review and reapply the consumer changes in `changes.patch`. Record changed
   file hashes and the full file inventory in `PATCHES.json`. Regenerate the
   review patch against the corresponding standalone upstream snapshots.
4. Run `node scripts/vendor-crates.mjs --check --allow-drift`, the strict Cargo
   audit, and the platform checks in `docs/DEPENDENCY_HEALTH.md`. The vendor
   check verifies local file integrity. It reports live upstream comparison
   as unavailable for these local ports, rather than claiming they match.

Remove each local patch only when the published upstream consumer accepts the
maintained GTK line and the complete Cargo graph resolves without old GLib or
either abandoned `proc-macro-error` implementation. Preserve the Tauri
`urlpattern` 0.6 migration when changing revisions.

The Linux tests `gtk_dispatch` and `gtk_runtime` exercise the actual channel
adapter and native WebKit/menu/window lifecycle. The runtime test requires a
display and a session bus, and fails if either cannot initialize. CI supplies
Xvfb and `dbus-run-session`; macOS and Windows report that GTK test as
inapplicable. Upstream tests and examples retained in registry archives are
not automatically part of GitPulse's test suite.
