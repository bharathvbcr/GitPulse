# Overview preview

Run `GITPULSE_DEV_PORT=5194 npm run dev -- --host 127.0.0.1`, then open
`http://127.0.0.1:5194/harness/overview.html`.

This mounts production `WorkView` through the real repository store with
explicit IPC fixtures. Wait for **Ready**, then use **Run interaction checks**.
The result tooltip contains the full JSON report. Every scenario checks for
browser crashes and unconfigured commands. **Switch theme** and resize the
pane to inspect both themes.

| Query | Checks |
| --- | --- |
| Default | Filtering, attention, search/reset, repository switching, local review after commit inspection, Resolve navigation, failed opens, delayed collision scanning and cached remounts |
| `?scenario=detached` | Detached status and review action |
| `?scenario=bare` | Bare repository without a working-tree review action |
| `?scenario=task` | Mixed clean/unscanned worktrees on one task |
| `?scenario=partial` | GitHub warnings/truncation, failed workflow, known overlaps alongside failed probes |
| `?scenario=failed` | Source failures do not claim an empty workspace |
| `?scenario=stress` | 250 worktrees, progressive rows and search beyond the rendered page |
| `?scenario=empty` | Empty workspace |
| `?scenario=closed` | No repository open |
| `?scenario=unavailable` | Absent GitHub and failed overlap scan |
| `?scenario=long` | Long repository path; the normal interactions also run |

Focused checks:

```sh
npm test -- src/lib/work src/lib/components/WorkView.test.ts src/lib/stores/__tests__/repoStore.test.ts
npm run check
npm run check:ipc
npm run check:types
npm run build
cargo test --manifest-path src-tauri/Cargo.toml --lib insights::tests::collision_risk -- --test-threads=2
```

Browser checks use fixtures; the two Rust collision tests use real temporary
Git repositories. Neither verifies an installed application's native webview
or installs a new build. See [the hardening contracts](../docs/OVERVIEW_HARDENING.md)
for limits and evidence boundaries.
