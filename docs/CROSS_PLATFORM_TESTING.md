# Cross-Platform Testing (Linux & Windows)

GitPulse ships native installers for macOS (universal), Linux (`x86_64` and `aarch64` on the Ubuntu 22.04 glibc/WebKitGTK 4.1 floor), and Windows (`x64` and `arm64` on WebView2 / ConPTY). This guide documents how to run the exact Linux and Windows engines locally and how GitHub Actions enforces them before every release.

## 1. Local Linux Testing in Podman (Ubuntu 22.04 Floor)

`Containerfile.linux-test` reproduces the Ubuntu 22.04 release floor (`libwebkit2gtk-4.1-dev`, `webkit2gtk-driver`, `libgtk-3-dev`, `xvfb`, headless `weston`, `dbus-x11`, software Mesa GL/EGL, Git >= 2.36 from the `git-core` PPA, Node 22, Bun, Rust stable, `cargo-nextest`, `tauri-driver`, and pinned `devmap` + `manvi` CLIs matching `ci.yml`/`release.yml`).

`scripts/linux-podman.mjs` drives the image on your running Podman machine (including macOS `libkrun`) with named volumes for `/cargo-target`, `/cargo-home`, `/workspace/node_modules`, `/workspace/dist`, and `/bun-cache`, so your macOS `target/` and `node_modules/` directories are never overwritten.

```bash
# 1. Build (or refresh) the Ubuntu 22.04 test image
bun run test:linux:podman -- build-image

# 2. Real GTK3 + WebKitGTK 4.1 smoke test (X11 and headless Wayland)
bun run test:linux:podman -- gtk-runtime --display both

# 3. Native WebKitGTK 4.1 UI regression harnesses (via framework/tao + framework/wry)
bun run test:linux:podman -- webview --harness diagnostics --display both
bun run test:linux:podman -- webview --all --display x11

# 4. Real-binary tauri-driver E2E smoke + PNG screenshots (X11 and headless Wayland)
bun run test:linux:podman -- e2e --display both

# 5. Linux fmt + clippy and nextest Rust suites (also supports --filter <expr> and `shell`)
bun run test:linux:podman -- clippy
bun run test:linux:podman -- rust-test --shard 1/3
```

Pass `--platform linux/amd64` on Apple Silicon when you specifically need x86_64 emulation (uses its own `gitpulse-linux-amd64-*` named volumes).

## 2. Native-Engine UI Regression Runner (`framework/tao` + `framework/wry`)

`src-tauri/examples/webview_regressions.rs` compiles against GitPulse's vendored `framework/tao` and `framework/wry` crates and is wired directly into `scripts/browser-regressions.mjs`:

| Platform | Command | Engine |
| :--- | :--- | :--- |
| macOS | `bun run test:native` / `bun run test:webkit:all` | System WKWebView (`webkit-regressions.swift`) |
| Linux | `bun run test:native` / `bun run test:native:all` | WebKitGTK 4.1 (`webview_regressions`, Xvfb or headless Wayland) |
| Windows | `bun run test:native` / `bun run test:native:all` | Edge WebView2 (`webview_regressions.exe`) |
| Any OS | `bun run test:browser:all` | Headless Chrome baseline |

Use `bun run test:native -- --harness <slug>` for one page or `bun run test:native:all` (`--all`) for the full registered harness suite. Pass `bun run test:browser -- --webview` on macOS to exercise the Rust `tao`/`wry` binary there as well.

## 3. Real-Binary `tauri-driver` E2E Smoke + Screenshots

`scripts/e2e-driver-smoke.mjs` launches the real compiled `gitpulse` binary through W3C WebDriver (`tauri-driver` + `WebKitWebDriver` on Linux, or `tauri-driver` + `msedgedriver` on Windows) against an isolated profile directory (`HOME`/`XDG_*` on Linux, `USERPROFILE`/`APPDATA`/`LOCALAPPDATA`/`WEBVIEW2_USER_DATA_FOLDER` on Windows) and verifies:

1. Cold start with isolated profile and zero uncaught page errors (`01-cold-start.png`)
2. Real Git repository + OS file watcher (`notify`) + live diff (`02-repo-watcher-diff.png`)
3. Real PTY (Linux) / ConPTY (Windows) terminal round-trip (`03-terminal-pty.png`)
4. `tauri-plugin-single-instance` second-launch handoff (`04-single-instance-handoff.png`)
5. Clean shutdown with zero orphaned app processes (`summary.json`)

Repository trust for the disposable E2E fixture repos is persisted inside the isolated profile via the test-only `e2e_seed_trust` example binary, keeping the shipping binary's trust boundary unchanged. On Windows, close any running GitPulse instance before launching E2E smoke because `tauri-plugin-single-instance` coordinates via a Win32 named mutex keyed on `com.gitpulse.desktop`.

```bash
# Build frontend dist, the app (with embedded assets via custom-protocol),
# the terminal host, and the trust helper, then run under Xvfb, Wayland, or Windows:
bun run build
cargo build --manifest-path src-tauri/Cargo.toml --locked \
  --features tauri/custom-protocol \
  --bin gitpulse --bin gitpulse-ptyd \
  --example e2e_seed_trust
bun run test:e2e:driver -- --artifacts .gitpulse/scratch/e2e-artifacts
```

## 4. Local Windows MSVC Cross-Check (`cargo-xwin`)

Before pushing changes that touch `#[cfg(windows)]` code or cross-platform tests, cross-check against Windows MSVC from macOS or Linux. Because `src-tauri` builds C crates (`rusqlite`, `tree-sitter`), embeds `src-tauri/tests.manifest` (comctl32 v6), and runs `tauri-winres` (`embed-resource` `.manifest_required()`), LLVM's MSVC cross tools (`clang-cl`, `lld-link`, `llvm-lib`, `llvm-rc`) are required — Apple's Xcode CLT `/usr/bin/clang` does not ship them.

```bash
# One-time setup (macOS):
brew install llvm lld                        # auto-discovered from /opt/homebrew/opt/{llvm,lld}/bin (or /usr/local/opt/{llvm,lld}/bin)
rustup target add x86_64-pc-windows-msvc     # also add aarch64-pc-windows-msvc for --arch arm64
cargo install --locked cargo-xwin

# One-time setup (Ubuntu/Debian):
# sudo apt-get install -y clang lld llvm

# Ensure dist/index.html exists (embedded by src-tauri), then cross-check into
# .gitpulse/scratch/target-win-cross:
bun run build
bun run check:windows:cross
bun run check:windows:cross -- --clippy
bun run check:windows:cross -- --tests
bun run check:windows:cross -- --arch arm64
```

`scripts/check-windows-cross.mjs` auto-discovers Homebrew's keg-only LLVM and split LLD (`clang-cl`, `lld-link`, `llvm-lib`, `llvm-rc` under `/opt/homebrew/opt/{llvm,lld}/bin` or `/usr/local/opt/{llvm,lld}/bin`) on macOS when absent from `PATH`, and fails closed with exit code `2` whenever `dist/index.html`, `src-tauri/tests.manifest`, the Windows MSVC rustup target, `cargo-xwin`, or any required LLVM tool (`clang-cl`, `lld-link`, `llvm-lib`, `llvm-rc` or `$RC_<target>`/`$RC`) is absent.

## 5. Full Windows & Linux VM Testing

For real WebView2 GPU rendering, ConPTY behavior, NSIS/MSI install & upgrade flows, and `windows_runtime`:

- **Apple Silicon Macs (Windows 11 ARM64 VM via UTM or Parallels)**:
  1. Install Rust (`msvc` toolchain), Node 22, Bun, Git for Windows, Visual Studio Build Tools (C++ workload + Windows SDK), `cargo-nextest`, `tauri-driver`, and `msedgedriver` whose major version matches the installed Edge WebView2 Runtime.
  2. Build the frontend once (`bun run build`), then run `cargo test --manifest-path src-tauri/Cargo.toml --locked --test windows_runtime -- --nocapture` to exercise real WebView2 + custom URI scheme + Win32 menus + owner child window + UTF-8 JS evaluation.
  3. Run `bun run test:native:all` for native WebView2 regressions and `bun run test:e2e:driver` (after the `--features tauri/custom-protocol` build in Section 3) for real-binary E2E + screenshots.
- **Linux VMs (Ubuntu 22.04 / Fedora Wayland)**:
  - Install the system packages from `Containerfile.linux-test`, run `bun run build`, then run `gtk_runtime`, `bun run test:native:all`, and `bun run test:e2e:driver` natively on Wayland or X11.

## 6. CI & Release Verification Coverage

- **`.github/workflows/ci.yml`** runs sharded `cargo nextest` across Linux (3 shards + extras, including `gtk_runtime` under Xvfb), Windows (4 shards + extras, including `windows_runtime`), and macOS (3 shards + extras) with pinned `devmap` (`DEVMAP_REV`) and `manvi` (`MANVI_REV`) on all three platforms, gated by `rust-test-complete` (`scripts/check-shard-coverage.mjs`) proving every listed test ran exactly once on all three platforms.
- **`.github/workflows/e2e-desktop.yml`** runs `gtk_runtime` (X11 + headless Wayland) and `windows_runtime` (`windows-latest`), native WebKitGTK/WebView2 harness checks (`all` — full 47-harness native suite — on pushes to `main`, nightly schedules, and `workflow_dispatch` with default `all`; `diagnostics` smoke on `pull_request` unless overridden via the `harnesses` input), and real-binary `tauri-driver` E2E smoke + PNG screenshot upload (`e2e-desktop-linux`, `e2e-desktop-windows`, with automatic `msedgedriver` <-> WebView2 major-version matching on Windows).
- **`.github/workflows/release.yml`** runs `preflight` with pinned `devmap` and `manvi`, `prepare` enforcing job-level CI provenance across all required `ci.yml` and `coverage.yml` jobs (`scripts/release-state.mjs`), per-leg bundle manifest checks (`scripts/check-release-assets.mjs`, including macOS universal `.app` verification) and `gitpulse-ptyd` release symbol verification (`scripts/check-ptyd-release.mjs`) on Unix legs, `attest` with SBOM JSON validation + bounded upload retries + canonical `SHA256SUMS.txt` manifest, and full draft download/unpack inspection in `verify` (downloading and verifying `SHA256SUMS.txt` against every uploaded asset digest).

## 7. Cross-Platform vs. Intentionally Unix-Gated Rust Tests

Shared helpers in `src-tauri/src/test_support.rs` (`sh_path`, `sh_quote`, `write_git_script`, `write_path_stub`, `prepend_path_dir`) un-gate shell-hook and PATH-stub tests in `tests/git_environment_pins.rs`, `src/engine/merge_tree_tests.rs`, `src/engine/git_writer.rs`, and `src/github/mod.rs` so they execute on Windows CI alongside macOS and Linux.

The remaining `#[cfg(unix)]` Rust test suites are intentionally Unix-gated because they assert POSIX-only primitives:

- **`tests/secrets_stress.rs`**: asserts POSIX signal delivery (`SIGKILL`, `SIGBUS`, `SIGABRT`), raw `waitpid` status (`0x0009`), `/dev/zero`/`dd`, `chmod 0o000`, and Unix-only scrubbed environment variables (`PWD`, `SHLVL`, `_`, `OLDPWD`).
- **`tests/terminal_*.rs` and `tests/adv_m3_pty_stress.rs`**: drive the Unix-only `gitpulse-ptyd` daemon over Unix domain sockets, `libc::kill(pid, 0)`, POSIX signals/kqueue, and `/bin/sh` job control. Windows ConPTY round-trip behavior is verified end-to-end by `scripts/e2e-driver-smoke.mjs` (`terminal-pty-roundtrip`).
- **`git_writer::a_cancelled_commit_leaves_no_index_lock`, `a_commit_stopped_at_its_deadline_leaves_no_index_lock`, `a_cancelled_commit_whose_hook_flooded_leaves_no_index_lock`, and `a_deadline_commit_whose_hook_traps_term_and_floods_leaves_no_index_lock`**: verify Git's graceful `.git/index.lock` cleanup on POSIX `SIGTERM` prior to `SIGKILL`; Windows terminates hook trees via `taskkill /T /F` (no graceful `SIGTERM`).
- **`git_writer::resolve_clone_destination_canonicalizes_and_rejects_symlinks`, `clone_repo_refuses_symlinked_destination_and_leaves_target_untouched`, and `directories_and_symlinks_are_reported_as_the_index_shows_them`**: create POSIX symlinks via `std::os::unix::fs::symlink` (unprivileged symlink creation is disabled by default on Windows).
- **`github::tests::{a_critical_alert_beyond_the_first_page_is_still_ranked_first, severity_tiers_are_fetched_concurrently_not_one_after_another, a_failing_tier_no_longer_discards_the_other_tiers, a_failed_tier_is_named_on_the_report, find_issue_by_marker_and_update_use_view_and_edit}`**: drive fake `gh` stubs with arguments containing Windows `cmd.exe` metacharacters (`&`, `?`, `=`, `"`, `<`, `>`) that Rust's `.cmd` argument hardening rejects or escapes differently from a native PE `gh.exe`; the underlying tier concurrency/deadline logic is tested cross-platform in `tiers_run_up_to_the_concurrency_cap_and_no_further` and `tiers_past_the_deadline_are_named_not_requested`.
- **`git_environment_pins::parsers_read_english_under_a_german_parent`**: probes Homebrew's gettext-enabled Git German message catalog.
