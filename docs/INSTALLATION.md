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

## Build from source

Follow [Contributing](../CONTRIBUTING.md) for platform toolchains and verification.
The short path, after installing its prerequisites, is:

```sh
git clone https://github.com/bharathvbcr/GitPulse.git
cd GitPulse
npm ci
git config core.hooksPath .githooks
npm run tauri dev
```

Use `npm run tauri build` for host-platform bundles. `npm run build` builds only
the frontend; it is not an installer or a native qualification run.
