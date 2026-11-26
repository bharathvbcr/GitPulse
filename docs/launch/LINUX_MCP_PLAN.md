# Linux `waylode-mcp` and `waylode-hook` (pre-rename `gitpulse-*`): plan

Read-only planning pass, 2026-11-23 (before the Waylode binary rename to `waylode-mcp` / `waylode-hook` in `5fcea3dbc`). Nothing here is built. The goal: Claude Code sessions on
Linux, in the cloud, over SSH and on BYOK runner boxes, get Waylode Insights and DevMap tools.

Some answers below are incomplete. `gitpulse_insights` timed out. DevMap answered with
`walk_incomplete` set, so the impact counts are lower bounds. The claim that DevCouncil `v0.2.3`
"writes schema 22" could not be checked at that tag.

## 1. Build the CLIs without Tauri, using a `desktop` cargo feature

Both bins link `waylode_lib`, which depends on `tauri` unconditionally. That pulls in
`framework/tauri` with GTK3 and `webkit2gtk`, plus `rfd`.

`mcp/`, `hooks/` and `repository_trust.rs` have no direct tauri use. About 60 of the 592 source
files do. The ones on the bins' paths are:

- `supervisor.rs:277`
- `logging.rs:1061-1073`
- `terminal/orchestrate.rs:147`
- `harness/steward.rs:797`
- `devmap/diagnostics.rs:31`
- `tool_install/mod.rs:300`
- `watcher/mod.rs:33`
- `daemon_lock.rs:294`
- `host_platform.rs:63`
- `pools.rs`
- `idle_trim.rs`

**Recommended:** gate tauri behind a feature rather than split the crate. A split has to move
`repository_trust`, `hooks`, `mcp` and the ledger, and `supervisor::spawn_once` alone has 252+
callers.

1. In `src-tauri/Cargo.toml`, make `tauri` and its plugins `optional`, and add
   `default = ["desktop"]`. The desktop bins get `required-features = ["desktop"]`;
   `gitpulse-mcp` and `gitpulse-hook` get none.
2. In `lib.rs`, put `#[cfg(feature = "desktop")]` on `commands`, `desktop`, `pools`, `idle_trim`,
   `native_rendering`, `run`/`context` and the plugin wiring.
3. In each leaf file above, use `cfg_attr(feature = "desktop", tauri::command)` or gate the item.
   `AppHandle` emit paths get a no-op when the feature is off. Gate `tauri-build` in `build.rs`.
4. Verify with:
   - `cargo build --locked --no-default-features --bin gitpulse-mcp --bin gitpulse-hook`
   - `cargo tree --no-default-features -i tauri` and `-i webkit2gtk` (both should come back empty)
   - `ldd` on the built binaries (no libwebkit or libgtk)
   - `bun run check:ipc` and `bun run check:types`, because `generate_handler!` must stay in
     lockstep with the commands

## 2. CI and release

1. Add a `cli-linux` job for `ubuntu-22.04` and `ubuntu-22.04-arm`. It installs no webkit or GTK
   and builds with `--no-default-features` for the two bins. It packages
   `gitpulse-cli_${version}_linux_{x86_64,aarch64}.tar.gz`, containing both bins and `LICENSE`.
2. Add a check that the bins link no webkit or GTK, using `readelf -d`.
3. `scripts/check-release-assets.mjs` (`expectedInstallerNames`, `supplementNames`) and
   `scripts/release-state.mjs` fail on assets they don't expect. Update both, and the asset counts
   in `release-workflow.test.ts` and `check-release-assets.test.ts`.
4. The existing `attest` job will then cover the tarballs' SHA-256 checksums.
5. Add a `ci.yml` leg that does the webkit-free build, to guard the feature split.

## 3. Schema parity

DevCouncil `main` and Waylode's vendored copy are both at `CURRENT_SCHEMA_VERSION = 29`. **No
DevCouncil release tag contains schema 29 yet**, so a new tag (v0.2.4 or later) has to ship
first. Then:

1. Re-vendor.
2. Run `bun run check:schema-parity` and `check:vendor-schema`.
3. Check `gitpulse-mcp --version` on a Linux box.

Also check dc-store's `SCHEMA_VERSION`, `WORKBENCH_SCHEMA` and `STACK_FORMAT_VERSION`.

## 4. Headless trust (owner decision, not decided)

Today trust can be granted only from the desktop app (`repository_trust::grant`, `:1109`). The
grant is stored in `default_config_dir()/repository-trust-v1`, with scopes
`None < Checkout < Repository`.

| Option | What | Risk |
|---|---|---|
| A | The user runs `gitpulse-mcp trust grant <path>` in their own shell. It needs a TTY and an interactive confirmation, refuses under CI or agent environment variables, and stores the grant in `$HOME`. | It is a grant path outside the desktop's confirmation, and an agent with shell access could try it. |
| B | The BYOK runner setup writes the grant for its one cloned repo. | The grant is made at a moment the user didn't approve one by one. It must bind to the repo's identity (`common_dir`), not its path. |
| C | An environment allowlist (`GITPULSE_TRUSTED_REPOS`). | A repo or an agent can most easily influence it. Not recommended. |
| D | No headless trust; document the limitation. | Safest, but Insights stays blank on Linux. |

Whichever option is chosen, trust must never be read from inside the repo or from `.claude/`. It
must be revocable (`revoke`, `:1320`) and default to `Checkout` scope.

## 5. Order

1. Claim `src-tauri/Cargo.toml` and `.github/workflows/release.yml`. Both have uncommitted edits
   on main from an unknown session.
2. Tag DevCouncil with schema 29, in parallel with 3.
3. The `desktop` feature, on a `feat/headless-cli` branch.
4. The webkit-free CI leg (after 3).
5. The `cli-linux` release job (after 3).
6. Re-vendor (after 2).
7. The trust CLI, once option A, B, C or D is picked.
8. Parity and a smoke test on an EC2 runner.
9. Install the CLIs on the BYOK runner during setup.
