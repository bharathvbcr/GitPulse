---
name: devmap-refactoring
description: Use DevMap to plan and verify renames, extractions, moves, splits, and
  structural code changes.
---

# Refactoring with DevMap

Resolve this checkout with `devmap paths --json`, then check `devmap status --json`. Read the returned `repo_map` path; do not assume `.devcouncil` rather than `.devmap`. A missing, stale, or partially parsed index is not complete evidence. Rebuild with `devmap build --manifest` when needed and authorized, then recheck status.

Use the connected DevMap MCP tools if they target this checkout; otherwise use the CLI from its root. GitPulse's `gitpulse_codeintel_*` tools also query DevMap: pass the absolute `repo_path`. Discover the actual tool names and schemas; a host need not expose every CLI capability.

1. Explore and assess impact with `devmap explore <name> --json` and `devmap impact <target> --json`.
2. Find all references using symbol search and direct source inspection, including registries, generated bindings, and dynamic calls the graph may miss.
3. Use the installed preview interface where supported. Do not invent a graph-coordinated rename command; use language tooling or a suitable available fallback when required.
4. Inspect the resolved map's unwired candidates before adding a module, and verify its real callers after the edit.
5. Review the complete diff and run candidate tests from `devmap affected <target> --json` plus the repository's required checks.

Read `shown`, `total`, `truncated`, and `walk_incomplete`; say which references remain unverified. Prefer DevMap where it answers the question and explain any fallback. Explicit user and repository requirements take precedence.
