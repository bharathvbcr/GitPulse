# Install and update GitPulse

[Documentation index](README.md) · [Getting started](GETTING_STARTED.md)

Download from [GitHub Releases](https://github.com/bharathvbcr/GitPulse/releases/latest).
Choose an asset from the selected release; repository manifests describe the
checkout and do not establish which release is publicly available.

## Platform packages

| Platform | Release target | Package |
| --- | --- | --- |
| macOS | Universal: Apple Silicon and Intel | `.dmg` |
| Linux | x86_64; Ubuntu 22.04 build baseline, glibc 2.35+ | `.AppImage`, `.deb` |
| Windows | x64; Windows 10/11 | `.msi`, `.exe` |

These are the release workflow's targets. Check that the chosen release includes
the asset for your machine. Building an installer does not establish that every
workflow has been tested on that platform.

> [!NOTE]
> GitPulse is developed and hand-tested on macOS. Windows and Linux builds are
> produced and covered by the automated suites, but are not exercised against a
> running app by a person. Some features are deliberately macOS-only — desktop
> notifications among them — and others are simply untried elsewhere.
> [Platform coverage](QUALIFICATION.md#platform-coverage) lists which is which.

Git must be installed and available to the app. For native build dependencies,
see [Contributing](../CONTRIBUTING.md).

### macOS

Open the DMG and drag GitPulse into Applications. Bundles use an ad-hoc signature
by default to seal resources; that is not a Developer ID signature or
notarization. Check the release's signing status before installing it.

If macOS blocks a download you have chosen to trust, after installing it at the
path below you can remove its quarantine attribute:

```sh
xattr -dr com.apple.quarantine /Applications/GitPulse.app
```

This removes a local quarantine attribute; it does not verify or notarize the app.
Open GitPulse from Applications, Finder, or Spotlight.

#### Quitting

Cmd-Q, Dock → Quit, the status menu's Quit item, and a Quit Apple event
(`osascript -e 'quit app id "com.gitpulse.desktop"'`) all go through the same
confirmation. GitPulse asks first when Git actions are still running, editor saves
are in flight, or editor drafts are unsaved, and quits immediately otherwise.
A logout or restart waits while GitPulse asks, then continues if you quit, or
stops if you choose to keep working.

- **Quit again to force it.** A second Quit while the confirmation is open
  quits at once without asking.
- **A window that cannot answer does not block quitting.** If the window does
  not respond to a quit within 3 seconds, for example because its page crashed,
  is reloading, or has hung, GitPulse quits.
- **A crashed window reloads itself.** If the window's web content crashes,
  GitPulse reloads it and records the crash in the log. When crashes repeat,
  GitPulse waits longer before each reload, up to 30 seconds.

A script or agent that quits GitPulse with an Apple event needs macOS Automation
permission for the program that sends it, such as Terminal or an agent's own
binary. The first time, macOS shows a consent dialog. Until someone answers it, the
event never reaches GitPulse and `osascript` fails with `AppleEvent timed out
(-1712)`. GitPulse cannot see or answer that dialog. Allow or deny it, or change
it later in System Settings → Privacy & Security → Automation.

#### Local builds and duplicate registrations

`bun run tauri build` leaves a complete `GitPulse.app` in
`src-tauri/target/release/bundle/macos/`, and Spotlight registers every app bundle
it indexes. That copy then competes with the installed one for "Open with",
`open -a GitPulse`, and `.git` file associations. Building the DMG also mounts it
under `/Volumes/dmg.*`, which can leave a registration for each mount that no
longer exists.

After you copy a build into Applications, unregister the build copy and refresh
the installed one:

```sh
LSREGISTER=/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister
"$LSREGISTER" -u src-tauri/target/release/bundle/macos/GitPulse.app
"$LSREGISTER" -f /Applications/GitPulse.app
"$LSREGISTER" -dump | grep -E '^path: .*GitPulse\.app'
```

Spotlight re-registers the build copy the next time it indexes `target/`. To stop
that, use one of these:

- Add `src-tauri/target` to System Settings → Spotlight → Search Privacy. This
  also keeps the large build tree out of the index.
- Delete `src-tauri/target/release/bundle/macos/GitPulse.app` after installing it.
  The DMG beside it is the artifact to keep.

`lsregister -gc` compacts the database. It is not established here whether it also
drops the stale `/Volumes/dmg.*` records.

### Linux and Windows

On Linux, install the `.deb` with your distribution's package tools, or mark the
AppImage executable and run it. On Windows, run the selected MSI or EXE installer.
Follow platform prompts, then open GitPulse and choose a repository. See
[Troubleshooting](../wiki/Troubleshooting.md) for missing Git or native libraries.

## Optional tools

| Tool | Adds |
| --- | --- |
| GitHub CLI (`gh`) | GitHub pull requests, issues, Actions, releases, and security alerts; authenticate with your own account |
| DevMap (`devmap`) | Code intelligence and Code → Map |
| Manvi (`manvi`) | Policy, task enhancement, and agent integration according to configuration |
| Local model server | Built-in AI assistance through a configured loopback endpoint |
| Apple Intelligence | Optional task title/description drafting on supported Mac hardware, OS, settings, and builds |

**Help → Set Up Optional Tools** installs DevMap by default. The setup also offers
the analysis suite or full DevCouncil host; use only the components you need.
Copy the offered command or run it in the app's terminal with a repository open.
Tool installation is separate from GitHub authentication and provider setup.

A missing tool is reported as unavailable. A missing policy check is **unchecked**,
not a successful policy verdict. See [Module integration](MODULE_INTEGRATION.md)
and [Security](SECURITY.md).

## Updates

Updates are manual downloads. **Settings → Updates** provides:

- An opt-in periodic release check, off by default.
- **Check now**, which makes one check on demand.
- A link to the release page when an update is found.

The check reads public tags using `git ls-remote`, at most once a day for the
periodic path. It does not download or install a binary. A failed check reports
that it could not check, rather than claiming the app is up to date.

**Upgrading a profile that has opened a newer task store.** The DevCouncil task
store (dc-store) migrates one way: a profile migrated to a newer schema cannot be
read back by a host built against an older one. (The dc-store vendored in this
tree is store schema 10 with workbench schema 14.) GitPulse, Manvi and DevCouncil
share that profile, so update all three together, built from the same dc-store
schema, before any of them opens it. Reinstall the `gitpulse-mcp` and `gitpulse-hook` binaries too
(`bun run mcp:install`); a binary built against an older schema refuses the
migrated profile. The release notes in the CHANGELOG record each schema change.
After updating, `bun run check:schema-parity` asks each installed reader which
schemas it reads, including the app's bundled server, and names any that were
left behind.

## Build from source

Follow [Contributing](../CONTRIBUTING.md) for platform toolchains and verification.
The short path, after installing its prerequisites, is:

```sh
git clone https://github.com/bharathvbcr/GitPulse.git
cd GitPulse
bun install --frozen-lockfile
git config core.hooksPath .githooks
bun run tauri dev
```

Use `bun run tauri build` for host-platform bundles. `bun run build` builds only
the frontend; it is not an installer or a native qualification run.
