# Development

Canonical guide: [CONTRIBUTING.md](https://github.com/bharathvbcr/GitPulse/blob/main/CONTRIBUTING.md). This page is the short path.

**Product stack.** DevCouncil is components and modules. Manvi wraps them. GitPulse uses Manvi for policy, workbench, and agent hosting, and selected DevCouncil modules for code intelligence. Update them independently; this app does not require the whole suite. See [Module integration](https://github.com/bharathvbcr/GitPulse/blob/main/docs/MODULE_INTEGRATION.md).

## Prerequisites

| Tool | Version | Why |
| --- | --- | --- |
| Node.js | 22.x, at least 22.12 | CI major, with the locked Vite/Vitest engine floor |
| Rust | stable, edition 2021 | `clippy` + `rustfmt` required |
| cargo-llvm-cov | latest | Rust LCOV for coverage floors |
| actionlint | latest | Only local gate that reads `release.yml` before a tag |
| Git | maintained | Runtime, not just VCS |
| `gh` | optional | GitHub panel only |

Platform:

- **macOS:** Xcode Command Line Tools. Universal release builds need both Darwin targets.
- **Windows:** MSVC build tools + WebView2.
- **Linux (Debian/Ubuntu):** `libwebkit2gtk-4.1-dev`, `libgtk-3-dev`, `libssl-dev`, `libayatana-appindicator3-dev`, `librsvg2-dev`, and friends — see CONTRIBUTING.md.

```sh
rustup component add clippy rustfmt llvm-tools-preview
cargo install cargo-llvm-cov --locked
```

## Clone and run

```sh
git clone https://github.com/bharathvbcr/GitPulse.git
cd GitPulse
npm ci
git config core.hooksPath .githooks
npm run tauri dev
```

The pre-push hook refuses a release tag that would publish the wrong tree. `release.yml` builds whatever commit the `v*` tag points at; only the machine holding both the tag and the work can know which commit you meant. Deliberate re-release of an older commit: `git push --no-verify`.

Vite picks a free port from 5173 (then 5174–5193). Pin with `GITPULSE_DEV_PORT`.

## Repository navigation

Run `devmap paths --json` and `devmap status --json` from each checkout. Read the
resolved `repo_map`; if its store or map is missing, run `devmap build --manifest`
and check again. Generated state is local to that worktree. Follow the repository
[agent guide](https://github.com/bharathvbcr/GitPulse/blob/main/AGENTS.md) for impact
checks and incomplete-result handling.

## The gate

```sh
npm run ci:local
```

Run it before opening a PR. It exercises this machine's toolchain; a local pass
does not prove the macOS, Linux and Windows jobs. Check the remote CI and coverage
results for the commit being reviewed or released.

While iterating:

| Command | Scope |
| --- | --- |
| `npm test` | Vitest |
| `npx vitest run src/lib/graph` | One directory |
| `cargo test --manifest-path src-tauri/Cargo.toml` | Rust suite |
| `npm run check` | svelte-check (TS 6 compatibility API) + stable TypeScript 7 `tsc` (`tsconfig.node.json`) |
| `npm run check:ipc` | Rust `cmd_*` registry ↔ frontend `invoke()` |
| `npm run check:vendor-schema` | Vendored store schema ↔ installed `devmap` CLI |
| `npm run check:types` | serde structs ↔ TypeScript interfaces |
| `npm run check:release` | Version manifests agree |
| `npm run mcp:install` | Put this tree's `gitpulse-mcp` on PATH |
| `npm run mcp:doctor` | PATH binary matches this tree (not in ci:local) |

Every bug fix ships with a test that fails against the unfixed code.

## Pull requests

1. Branch from `main`.
2. One coherent concern.
3. `npm run ci:local` green.
4. Conventional Commits (`feat:`, `fix:`, `docs:`, `test:`, `chore:`, `perf:`).
5. Fill in the PR template.

Security issues: do **not** file a public issue. See [[Security]].

## Recognition

GitPulse follows [All Contributors](https://allcontributors.org/). Comment `@all-contributors please add @username for code, doc` on any issue or PR.
