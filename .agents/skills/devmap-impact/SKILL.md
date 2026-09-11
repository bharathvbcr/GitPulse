---
name: devmap-impact
description: Use DevMap before changing code to identify callers, dependencies, candidate
  tests, and the limits of blast-radius evidence.
---

# Impact analysis with DevMap

Resolve this checkout with `devmap paths --json`, then check `devmap status --json`. Read the returned `repo_map` path; do not assume `.devcouncil` rather than `.devmap`. A missing, stale, or partially parsed index is not complete evidence. Rebuild with `devmap build --manifest` when needed and authorized, then recheck status.

Use the connected DevMap MCP tools if they target this checkout; otherwise use the CLI from its root. GitPulse's `gitpulse_codeintel_*` tools also query DevMap: pass the absolute `repo_path`. Discover the actual tool names and schemas; a host need not expose every CLI capability.

1. Run `devmap impact <target> --json` on the exact symbol or file.
2. Inspect direct callers first, then transitive paths. Explain what may change; a dependency does not by itself prove a break.
3. Run `devmap affected <target> --json` to locate candidate tests and inspect their assertions. These are graph-reachable tests, not a coverage guarantee.
4. Use `devmap preview --help` or the exposed preview schema to check proposed source when useful.
5. Review `git diff` against the intended scope and run relevant tests plus required repository checks.

Read `shown`, `total`, `truncated`, and `walk_incomplete`. Never turn an incomplete or unavailable answer into a zero-risk verdict. For capabilities not exposed by this host, state the gap and use source inspection or an available fallback; follow user and repository requirements.
