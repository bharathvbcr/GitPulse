# Conflict editor verification

Run the production Vite configuration from the repository root:

```sh
node node_modules/vite/bin/vite.js --config vite.config.ts --host 127.0.0.1 --port 5197 --strictPort
```

Open `http://127.0.0.1:5197/harness/conflicts.html` for the interactive preview.
Add `?theme=light` for light mode or `?check=1` to run 43 browser regressions.
The checks must report **43/43**, with no crashes or unconfigured IPC calls,
in `window.__gpResult` and the visible result badge. A missing result is not a pass.

The harness mounts the production component and uses explicit IPC fixtures.
It covers file and repository switching, choice/custom draft retention, source
changes, empty output, preview/load retries, delayed responses, save locking,
staging failure, automatic file advancement, and keyboard focus after conflict
navigation. It also exercises undo/redo, bulk actions, 100 rapid inputs, binary
and external resolution, draft export, 2,000 conflicts in 25-block pages,
saves after unmount, and final staged review. Desktop dark and 600×700 light
layouts were visually inspected.

Native behavior is checked separately:

```sh
npm run test:browser -- --harness conflicts
npm run test:webkit -- --harness conflicts
npm test -- src/lib/components/ConflictEditor.test.ts src/lib/diff/conflictSession.test.ts src/lib/diff/conflictPresentation.test.ts src/lib/files/editorDraftRegistry.test.ts
cargo test --manifest-path src-tauri/Cargo.toml --lib diff::conflict
cargo test --manifest-path src-tauri/Cargo.toml --test conflict_hardening --test conflict_save_integration --test repo_operation_integration --test stress_test
```

Browser fixtures do not prove IPC delivery or the installed Tauri application's
behavior. This preview performs no writes to a real Git repository.
WebKit uses the system WKWebView; the fixture flushes browser tasks without
depending on background timer rates. See [the archived audit](../docs/archive/CONFLICT_RESOLUTION_AUDIT.md)
for native evidence and platform limits.
