---
name: devmap-debugging
description: Use DevMap when tracing a bug, locating an error source, or investigating
  why code fails.
---

# Debugging with DevMap

Resolve this checkout with `devmap paths --json`, then check `devmap status --json`. Read the returned `repo_map` path; do not assume `.devcouncil` rather than `.devmap`. A missing, stale, or partially parsed index is not complete evidence. Rebuild with `devmap build --manifest` when needed and authorized, then recheck status.

Use the connected DevMap MCP tools if they target this checkout; otherwise use the CLI from its root. Always pass `repo_path` (the absolute repository path) on every `devmap_*` and `gitpulse_codeintel_*` call, and check `repository.root` in the envelope before trusting the answer — Cursor shares one `devmap mcp` process across tabs. Discover the actual tool names and schemas; a host need not expose every CLI capability.

1. Reproduce the symptom with a bounded command or existing test and record the actual failure.
2. Use `devmap search <name> --json` and `devmap explore <name> --json` to locate suspect symbols and callers. For literal error text that is not a symbol, search the source.
3. Use `devmap trace <entry> <suspect> --json` when the call chain is the question. Confirm execution through call sites, logs, or instrumentation.
4. Challenge the diagnosis, then add a regression that fails before the fix.
5. Run `devmap impact <target> --json` before editing and `devmap affected <target> --json` for candidate tests; include the repository's required checks.

For a recent regression, inspect `git diff` and `git log`, then query changed symbols. Read `shown`, `total`, `truncated`, and `walk_incomplete`; an unresolved dynamic call is an evidence gap. If DevMap cannot answer a required question, state why and use source inspection or an available fallback. Explicit user and repository requirements take precedence.
