# Installation

The maintained installation guide lives in
[docs/INSTALLATION.md](https://github.com/bharathvbcr/GitPulse/blob/main/docs/INSTALLATION.md).
It covers platform packages, signing and quarantine, optional tools, updates,
and building from source.

[Download the Waylode 0.2.0 beta (`v0.2.0-beta.1`)](https://github.com/bharathvbcr/GitPulse/releases/tag/v0.2.0-beta.1). It is a
GitHub pre-release with macOS installers only. Betas are tagged `vX.Y.Z-beta.N` and stable releases `vX.Y.Z`; see
[versions and channels](https://github.com/bharathvbcr/GitPulse/blob/main/docs/INSTALLATION.md#versions-and-channels).
Use the selected release's assets and notes for availability and signing status.
A version in a repository checkout is not proof of a published release.

After installing the app, connect Claude Code or Codex from **Settings → Agents → Connect your agent**; see [[MCP and Agents]]. `bun run mcp:install` is only for a source checkout. DevMap (code map) currently builds from source and needs a Rust toolchain; Manvi builds from source with Go.

After installation, continue with [[Getting Started]]. For developer prerequisites,
see [[Development]].
