# Troubleshooting

## macOS: app is damaged / cannot be opened

The release binaries are unsigned. After installing to `/Applications`:

```sh
xattr -dr com.apple.quarantine /Applications/GitPulse.app
```

See [[Installation]].

## GitHub PRs, Actions, or Dependabot are empty

Those features use the **local `gh` CLI**, not a GitPulse token.

```sh
gh auth status
gh auth login
```

GitPulse never stores GitHub credentials. If `gh` is missing, the rest of the app still works.

## Policy shows Unchecked

MANVI is optional. **Unchecked** means the harness was absent or that rung could not run. It is not a pass. Install from **Settings → Agents** (or the MANVI panel's Install button) when a sibling `Manvi` checkout is available, or run `go -C manvi install ./cmd/manvi` yourself, then press Reconnect. Otherwise treat Unchecked as "this check did not run". See [[Policy and AI]].

## Local AI will not connect

Only loopback is allowed: `127.0.0.1`, `localhost`, `[::1]`. A LAN or cloud base URL is rejected on purpose so diffs do not leave the machine. Point Ollama / LM Studio at a loopback listen address.

## Coverage / Health / Storage look empty

Those Insights sections are **on-demand**. Nothing is scanned until you run it. A cell that was not scanned is *not scanned*, not `0%` or `0` vulnerabilities.

## Fleet shows "not scanned" or "could not read"

Same honesty rule. Click the cell to scan that repository for that column. A total such as "counted across 14 of 21, 1 failed, 6 not scanned" is complete accounting, not a truncated label. See [[Views]].

## Code map / Map panel says unavailable

DevMap answers from `.devcouncil/codeintel/devmap.sqlite` (store schema **19**). Install the `devmap` CLI from **Settings → Agents** or Code → Map when missing (`cargo install --path …/devmap-cli --locked --force` from a sibling DevCouncil checkout), so Build / Refresh can index. A schema mismatch names both versions rather than looking like an empty all-clear. `walk_incomplete` and truncated samples are floors, not complete coverage. See [[Views]].

## Terminal vanished after switching views

The dock is toggled with `Ctrl+\``. Hiding it does not kill the session. Switching **repositories** does. Fleet hides the repo pane without unmounting it so PTYs survive Fleet.

## `gitpulse-mcp` is missing or stale

```sh
npm run mcp:install
npm run mcp:doctor
```

Doctor reports absent / unresponsive / stale / matching. Agents need the binary on `PATH` (or `GITPULSE_MCP_PATH`). See [[MCP and Agents]].

## Dev app will not start

- Node 22+ and stable Rust with clippy/rustfmt.
- `npm install` then `npm run tauri dev`.
- Port 5173 busy: GitPulse walks 5174–5193, or set `GITPULSE_DEV_PORT`.
- Linux: WebKitGTK 4.1 and GTK 3 dev packages (see [[Development]]).

## `npm run ci:local` fails on coverage

Floors: frontend 90% lines / 85% branches, Rust 80% lines. `ci:local` regenerates both LCOV reports; a stale `lcov.info` on disk cannot pass the gate. Need `cargo-llvm-cov` and `actionlint`.

## Release tag did not build what I committed

`release.yml` builds the commit the `v*` tag points at. A tag left on an older commit produces a self-consistent older build. The pre-push hook (`git config core.hooksPath .githooks`) is there to refuse that. See [[Development]].

## Still stuck

- [Open an issue](https://github.com/bharathvbcr/GitPulse/issues) with OS, GitPulse version, and what the UI actually said (including *could not read* / *unchecked* wording).
- Security: [[Security]] — advisory, not a public issue.
