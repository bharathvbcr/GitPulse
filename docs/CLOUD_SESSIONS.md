# Cloud, SSH and Remote Control sessions

What a Claude Code session can use of Waylode when it is not running on your
Mac. Short version: DevMap yes (if installed), Waylode Insights no.

Platform facts below are cited to the Claude Code documentation as read on
2026-11-23 (`cloud-environments`, `settings`, `hooks`, `claude-code-on-the-web`
under `https://code.claude.com/docs/en/`). Anything not cited is under
[Unverified](#unverified).

## What works where

| Surface | Runs on | DevMap | `gitpulse-mcp` / Insights |
| --- | --- | --- | --- |
| Cloud session (claude.ai/code) | Anthropic VM, Ubuntu 24.04 x86_64 ([cloud-environments, What's available in cloud sessions](https://code.claude.com/docs/en/cloud-environments.md#whats-available-in-cloud-sessions)) | Yes, if the environment setup script installed it | No |
| SSH session | Your Linux host, its own `~/.claude` | Yes, after running `scripts/cloud/setup.sh` there | No |
| Remote Control | Your Mac; the phone/web is only a window ([claude-code-on-the-web](https://code.claude.com/docs/en/claude-code-on-the-web.md)) | Yes (it is your local session) | Yes (it is your local session) |

A cloud session reads the repository's committed `.claude/settings.json` (in a
session with one repository) and never your `~/.claude/settings.json`
([settings, Settings in cloud sessions](https://code.claude.com/docs/en/settings.md#settings-in-cloud-sessions)).
That is why the hook lives in the repository and the install lives in the
environment.

The SessionStart hook does **not** run in a session with several repositories,
or in a cloud thread inside a project; install DevMap in the setup script for
those and the agent will find it on PATH
([cloud-environments, Limitations in cloud sessions](https://code.claude.com/docs/en/cloud-environments.md#limitations-in-cloud-sessions)).

## Pieces

- `scripts/cloud/setup.sh`: installs `devmap` at the commit pinned by
  `DEVMAP_REV` in `.github/workflows/ci.yml` (the single source of truth; the
  script reads it, it does not copy it). Idempotent. It exits 0 even when the
  install fails, because a non-zero setup script stops the session from
  starting properly
  ([Script requirements](https://code.claude.com/docs/en/cloud-environments.md#script-requirements)).
  The last line is `devmap: AVAILABLE <version> ...` or
  `devmap: UNAVAILABLE: <reason>`, and it says where the pin came from. `--strict`
  exits 1 instead.
  - As root (a cloud VM) devmap is a real 0755 copy in `/usr/local/bin` and the
    stamp is in `/usr/local/share/gitpulse-devmap`, so the session user can run
    and check it. As a normal user it is `$HOME/.local/bin`.
- `scripts/cloud/session-start.sh`, wired as a `SessionStart` hook (matcher
  `startup|resume`, the matcher the cloud-environments page uses) in
  `.claude/settings.json`:
  - Does nothing unless `CLAUDE_CODE_REMOTE=true` or `GITPULSE_SESSION_BOOTSTRAP=1`.
  - Never downloads or installs anything.
  - Runs `devmap build --manifest` (bounded to 120 s).
  - Compares the installed build's stamp with the pin in the repo's own `ci.yml`
    and tells the agent when it is NOT at the pin, is the stale release, or has
    no stamp. When devmap is missing it says DevMap and Waylode tools are
    unavailable and to use source inspection.
- `.devcontainer/setup.sh` calls the same installer (`--strict`).

`.claude/settings.json` is un-ignored in `.gitignore` for this: it has to be
committed for a cloud session to see it.

## Cloud environment setup

1. At claude.ai/code open the environment settings and the **Setup script** row.
   You can paste the script or choose a file up to 64 KB
   ([Add a setup script](https://code.claude.com/docs/en/cloud-environments.md#add-a-setup-script)).
2. Paste the full contents of `scripts/cloud/setup.sh`. It is a Bash script run
   as root on Ubuntu 24.04 before Claude Code launches
   ([Setup scripts](https://code.claude.com/docs/en/cloud-environments.md#setup-scripts)).
   When the repository is not beside it, the script fetches `ci.yml` from
   `raw.githubusercontent.com` (on the default allowlist) to read the pin and
   says `pin ... from github main ci.yml`; set `DEVMAP_REV` at the top of the
   pasted script to avoid that.
3. Rust (`rustc` and `cargo`) and Go are preinstalled
   ([Installed tools](https://code.claude.com/docs/en/cloud-environments.md#installed-tools)),
   so there is no toolchain step. The install is a source build
   (`cargo install --git ... --rev <pin> --locked`), bounded by
   `DEVMAP_INSTALL_TIMEOUT` (default 1800 s).

### Caching: read this before relying on the build

The environment is cached only when the setup script finishes within roughly
five minutes; the snapshot is reused for later sessions and rebuilt after about
seven days or when the script or allowed hosts change
([Environment caching](https://code.claude.com/docs/en/cloud-environments.md#environment-caching)).
A cold `cargo install` of devmap may well take longer than five minutes on the
VM (not measured, see Unverified). If it does, the environment is **not**
cached, the build runs on every new session, and the 1800 s bound is what stops
it. Lowering `DEVMAP_INSTALL_TIMEOUT` below the cache limit does not help,
because a build that is cut off leaves devmap unavailable.

### The stale release fallback

The only Linux release is DevCouncil v0.2.3, x86_64 only, which writes store
schema 22 while the current schema is 29. `setup.sh` uses it only when you pass
`--allow-stale-release` (or `DEVMAP_ALLOW_STALE_RELEASE=1`), after the source
build failed, and only when the tarball's sha256 equals the value pinned in the
script (taken from the published `.sha256` and cross-checked against the GitHub
release asset's own digest). A mismatch discards the download and reports
UNAVAILABLE. The session hook warns the agent when the stale build is the one
installed.

## SSH hosts

An SSH session runs on your host with that host's `~/.claude`; the cloud
`CLAUDE_CODE_REMOTE` variable is not set there. To get the same index build:

1. Run `scripts/cloud/setup.sh` once on the host (as your user, it installs
   under `~/.local`).
2. Opt in to the hook by setting `GITPULSE_SESSION_BOOTSTRAP=1` for sessions on
   that host: either `export GITPULSE_SESSION_BOOTSTRAP=1` in the remote shell
   profile, or put it in the remote `~/.claude/settings.json` under the `env`
   key (`"env": { "GITPULSE_SESSION_BOOTSTRAP": "1" }`;
   [`env` is a documented settings key](https://code.claude.com/docs/en/settings-reference#env)).
   It is deliberately not keyed off `SSH_CONNECTION`.

The hook comes from the repository's committed `.claude/settings.json`, so the
checkout on the host needs it, and Claude Code asks you to trust that folder
first.

## Not available, and why

`gitpulse-mcp` (the `gitpulse_*` tools: insights, collision risk, ledger, task
board) is not provided in cloud or SSH sessions:

- no Linux build is published (the only release, v0.2.0-beta.1, shipped macOS
  assets only, although `release.yml` has Ubuntu legs), and
- it refuses repositories that have not been trusted, and that trust grant is
  made in the desktop app on the machine that holds it.

The setup script says so in its output and does not try to install it.

## Unverified

- Whether `cargo install --git https://github.com/bharathvbcr/DevCouncil` works
  from the cloud VM. `github.com` and `static.crates.io`/`index.crates.io` are on
  the default allowlist, but GitHub traffic goes through a proxy scoped to the
  repositories attached to the session
  ([GitHub proxy](https://code.claude.com/docs/en/cloud-environments.md#github-proxy)),
  and DevCouncil is not one of them. The release download is likewise
  unconfirmed. If blocked the script reports `devmap: UNAVAILABLE: ...` and the
  session proceeds without it. Check the first session's setup log.
- How long the cold source build takes on the VM, and so whether the environment
  gets cached (see Caching).
- Whether the repository is already cloned when the setup script runs (the
  `ci.yml` fallback covers both cases).
- That `devmap build --manifest` finishes inside the hook's bound on a cold VM
  (about 20 s on an Apple-silicon Mac; not measured on the VM, and a run under
  amd64 emulation hit the 120 s bound).
- Whether `env` in a remote `~/.claude/settings.json` reaches hook processes (it
  is documented as a settings key; the hook-environment behaviour was not
  checked). The shell-profile route does not depend on it.
