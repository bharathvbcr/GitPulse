# Installation

Download a pre-built installer from the [latest release](https://github.com/bharathvbcr/GitPulse/releases/latest).

| Platform | Format | Architecture | Notes |
| --- | --- | --- | --- |
| **macOS** | `.dmg` | Universal (Apple Silicon & Intel) | Unsigned. Clear quarantine after install (below). |
| **Linux** | `.AppImage`, `.deb` | x86_64 | Built on Ubuntu 22.04 (glibc 2.35+) |
| **Windows** | `.msi`, `.exe` | x64 | Windows 10 / 11 |

Current packaged version is **1.0.0** (`package.json` / release manifests). Prefer the GitHub Releases page over a cached copy.

## macOS Gatekeeper

macOS quarantines unsigned downloads. After dragging GitPulse to `/Applications`:

```sh
xattr -dr com.apple.quarantine /Applications/GitPulse.app
```

Then open the app from Finder or Spotlight.

## Updates

GitPulse does **not** auto-update and does not check for updates unless you ask it to.

**Settings → Updates**:

- **Off by default.** With the toggle off, GitPulse makes no network request about itself.
- **When enabled**, it compares public release tags at most once a day via `git ls-remote`. No account, no token, and nothing about your repositories is sent.
- **Check now** runs a single check on demand.
- It never downloads or installs binaries. The notification links to the release page.

A check that cannot complete says so. "Could not check" is never reported as "up to date".

## Runtime dependencies

GitPulse shells out to tools you already have:

| Tool | Required? | Used for |
| --- | --- | --- |
| `git` | Yes | Every repository operation |
| `gh` | Optional | PRs, issues, workflow runs, Dependabot and code scanning alerts (fetched when a repository opens) |
| Local LLM (Ollama, LM Studio, llama.cpp, vLLM) | Optional | Commit messages, explanations, health/coverage suggestions |
| `manvi` sidecar | Optional | Manvi wrap around DevCouncil modules: policy gate. Absent → verdicts are **unchecked**, never silently allowed |
| `devmap` CLI | Optional | DevCouncil code-intelligence module for Code → Map. Absent is named; it is not an all-clear |

Setup can install **DevMap only** (default — enough for Code → Map), the **analysis suite** (`devmap`, `dcstore`, `dcverify`, `dcgrep`), or the **full DevCouncil host**. Copy the command, or with a repository open click **Run in terminal** (Console, not the Manvi gate). Settings can disable a tool or uninstall a binary GitPulse itself placed in its app bin directory. These are native Go/Rust binaries — there is no `uv` / Python install path. Standalone from a DevCouncil checkout:

```sh
bash scripts/install.sh --only=devmap
bash scripts/install.sh --help
```

## Build from source

If you want a development build, see [[Development]]. Short version:

```sh
git clone https://github.com/bharathvbcr/GitPulse.git
cd GitPulse
npm ci
git config core.hooksPath .githooks
npm run tauri dev
```

Prerequisites: Node.js 22.x (22.12 or newer), stable Rust, platform native toolchains (Xcode CLT / MSVC + WebView2 / WebKitGTK).
