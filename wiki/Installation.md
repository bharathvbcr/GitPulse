# Installation

The maintained installation guide lives in
[docs/INSTALLATION.md](https://github.com/bharathvbcr/GitPulse/blob/main/docs/INSTALLATION.md).
It covers platform packages, signing and quarantine, optional tools, updates,
and building from source.

[Download the GitPulse 2.0 beta (`v1.99.0`)](https://github.com/bharathvbcr/GitPulse/releases). It is a pre-release, so GitHub's
[Latest release](https://github.com/bharathvbcr/GitPulse/releases/latest) stays on `v1.3.1` until the stable 2.0 release ships.
Use the selected release's assets and notes for availability and signing status.
A version in a repository checkout is not proof of a published release.

After installing the app, connect Claude Code or Codex from **Settings → Agents → Connect your agent**; see [[MCP and Agents]]. `bun run mcp:install` is only for a source checkout. DevMap (code map) currently builds from source and needs a Rust toolchain; Manvi builds from source with Go.

After installation, continue with [[Getting Started]]. For developer prerequisites,
see [[Development]].
