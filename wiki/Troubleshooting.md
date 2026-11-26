# Troubleshooting

## macOS: app is damaged / cannot be opened

The release binaries are unsigned. After installing to `/Applications`:

```sh
xattr -dr com.apple.quarantine /Applications/GitPulse.app
```

See [[Installation]].

## GitHub PRs, Actions, or Dependabot are empty

Those features use the **local `gh` CLI**, not a Waylode token.

```sh
gh auth status
gh auth login
```

Waylode never stores GitHub credentials. If `gh` is missing, the rest of the app still works.

## Policy shows Unchecked

MANVI is optional. **Unchecked** means the harness was absent or that rung could not run. It is not a pass. Install from **Settings → Agents** (or the policy card in Work → Overview) when a sibling `Manvi` checkout is available, or run `go -C manvi install ./cmd/manvi` yourself, then press Reconnect. Otherwise treat Unchecked as "this check did not run". See [[Policy and AI]].

## Local AI will not connect

Only loopback is allowed: `127.0.0.1`, `localhost`, `[::1]`. A LAN or cloud base URL is rejected on purpose so diffs do not leave the machine. Point Ollama / LM Studio at a loopback listen address.

## Coverage / Health / Storage look empty

Coverage and Storage stay **on-demand**. Health runs a local audit when you open the section, and fetches GitHub Dependabot and code scanning alerts when a repository opens (Settings → Analysis). A cell that was not scanned is *not scanned*, not `0%` or `0` vulnerabilities.

## Fleet shows "not scanned" or "could not read"

Same honesty rule. Click the cell to scan that repository for that column. A total such as "counted across 14 of 21, 1 failed, 6 not scanned" is complete accounting, not a truncated label. See [[Views]].

## Code map / Map panel says unavailable

DevMap answers from the store resolved by `devmap paths --json` (the vendored reader is store schema **29**; default `.devmap/codeintel/devmap.sqlite`, legacy `.devcouncil` supported). When DevMap is missing, Health, Suspects, the diff impact panel, palette symbol search and the Map show **Set up code map** (or **Build index** when it is installed but unindexed); use that button, or Settings → Agents. For now the install builds `devmap` from source, so it needs a Rust toolchain (cargo); no published DevMap release is pinned yet (devmap v0.2.3 writes store schema 22, Waylode reads 29 or newer). A schema mismatch names both versions rather than looking like an empty all-clear. `walk_incomplete` and truncated samples are floors, not complete coverage. See [[Views]].

Use **Code → Map → Copy DevMap logs** to copy the current Map status, binary
lookup, build/background-refresh state, visible errors, and recent DevMap entries
from Waylode's session and durable logs. Copy remains available during a build
and when DevMap is missing. Log-read failures and timeouts are included in the
report, and clipboard failures are shown in the panel.

Native entries include timestamps, `run_id`, repository, binary, effective
arguments, deadline, elapsed time, exit status, stderr and selected report
diagnostics. Only stage numbers are recorded live; full captured stderr is
redacted together when the command ends, including an explicitly incomplete
prefix on timeout. Preview source and successful query payloads are excluded.
The report samples the last 500 backend entries from each log across repositories;
match the repository on the start entry and its `run_id`. Older entries may have
rotated out. Each copied section is capped at 64 Ki characters and says when cut.

## Terminal vanished after switching views

The dock is toggled with `Ctrl+\``. Hiding it or switching views or **repositories** does not kill the session: each repository tab has its own terminal dock and preserves its sessions until you close the terminal tab or repository tab (and on Unix, PTY sessions live in `gitpulse-ptyd` and survive app quit or crash unless stopped). Fleet hides the repo pane without unmounting it so PTYs survive Fleet. When expanded, the dock hides and marks `inert` the underlying view to prevent focus leaks; restore its size, double-click a page header/background, or toggle the dock to return to the repository view.

## Terminal shows non-GPU rendering or text glitching

If WebGL acceleration experiences repeated context losses (surpassing the 3-recovery budget) or a GPU reset occurs, Waylode falls back to DOM rendering (`data-renderer="dom"`) to prevent pane blanking or driver crashes. A footer indicator informs you when drawing without WebGL. If hardware text distortion occurs, disable GPU rendering globally in **Settings → Appearance → GPU terminal rendering**.

## Settings shows "N hook reports in the last 7 days got no reply from Waylode in time"

This notice indicates agent hook notifications were sent when Waylode was busy, shutting down, or temporarily unreachable. The hook waited out its timeout budget (300 ms for `SessionEnd`, 1.5 s for others), recorded a timed drop in `.drops`, and exited without waiting further. This does not mean the event was lost (the app may have applied it as the hook gave up). Records rotate to `.prev` at 32 KiB and expire automatically after 7 days.

## `gitpulse-mcp` is missing or stale

App install: open **Settings → Agents → Connect your agent**. It links the app's bundled `gitpulse-mcp` and `gitpulse-hook` into `~/.local/bin` and registers the server with Claude Code and Codex, so they follow the app when it updates. Check these first:

- `~/.local/bin` is not on the `PATH` of the shell your agent starts from. The Claude Code plugin starts both binaries by bare name.
- You ran `bun run mcp:install` before: `~/.cargo/bin/gitpulse-mcp` or `gitpulse-hook` may shadow the app's copies, or a registration may point at the cargo copy. Connect leaves those alone and shows them. Switch with `cargo uninstall gitpulse` and the remove/add commands it shows.
- Claude Code runs with a non-default `CLAUDE_CONFIG_DIR`: Connect uses the `claude` CLI's default profile, so use **Copy setup prompt** instead.

Source checkout only:

```sh
bun run mcp:install
bun run mcp:doctor
```

Doctor reports absent / unresponsive / stale / matching for **both** `gitpulse-mcp` and `gitpulse-hook`. Agents spawn the bare names out of `.mcp.json` and `hooks/hooks.json`, so both must be on `PATH`; `GITPULSE_MCP_PATH` only moves the binary Waylode's own Settings panel reports and is not read by any MCP client. See [[MCP and Agents]].

## Manvi is not installed

Settings shows "Manvi is not installed" with **Set up Manvi**, which builds Manvi from source with Go (Settings → Agents → Manvi policy). Until then Git actions run without policy checks. No published Manvi release is pinned yet (v0.0.5 links an older dc-store).

## Dev app will not start

- Node 22+ and stable Rust with clippy/rustfmt.
- `bun install` then `bun run tauri dev`.
- Port 5173 busy: Waylode walks 5174–5193, or set `GITPULSE_DEV_PORT`.
- Linux: WebKitGTK 4.1 and GTK 3 dev packages (see [[Development]]).

## `bun run ci:local` fails on coverage

Floors: frontend 90% lines / 85% branches, Rust 80% lines. `ci:local` regenerates both LCOV reports; a stale `lcov.info` on disk cannot pass the gate. Need `cargo-llvm-cov` and `actionlint`.

## Release tag did not build what I committed

`release.yml` builds the commit the `v*` tag points at. A tag left on an older commit produces a self-consistent older build. The pre-push hook (`git config core.hooksPath .githooks`) is there to refuse that. See [[Development]].

## Still stuck

- [Open an issue](https://github.com/bharathvbcr/GitPulse/issues) with OS, Waylode version, and what the UI actually said (including *could not read* / *unchecked* wording).
- Security: [[Security]] — advisory, not a public issue.
