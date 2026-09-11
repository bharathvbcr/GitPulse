---
name: devmap-exploring
description: Use DevMap to explain code structure, architecture, callers, and execution
  paths.
---

# Exploring with DevMap

Resolve this checkout with `devmap paths --json`, then check `devmap status --json`. Read the returned `repo_map` path; do not assume `.devcouncil` rather than `.devmap`. A missing, stale, or partially parsed index is not complete evidence. Rebuild with `devmap build --manifest` when needed and authorized, then recheck status.

Use the connected DevMap MCP tools if they target this checkout; otherwise use the CLI from its root. GitPulse's `gitpulse_codeintel_*` tools also query DevMap: pass the absolute `repo_path`. Discover the actual tool names and schemas; a host need not expose every CLI capability.

1. Run `devmap explore <name> --json` for definitions, callers, callees, and blast radius.
2. Use `devmap trace <from> <to> --json` for a specific entry-to-symbol path.
3. Use the resolved map's subsystems, entry points, and critical files for ownership.
4. Confirm the explanation against source and call sites. Graph paths describe static relationships, not proof that a path executed.

Read `shown`, `total`, `truncated`, and `walk_incomplete`. Increase query bounds only within supported limits. When DevMap cannot answer, explain the limitation and use source inspection or a suitable available fallback, subject to user and repository instructions.
