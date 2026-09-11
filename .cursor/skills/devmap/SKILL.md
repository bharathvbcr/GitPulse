---
name: devmap
description: Use DevMap first for code navigation, callers, blast radius, traces,
  dead code, and code structure. Report incomplete evidence and use a justified fallback
  when necessary.
---

# Code navigation with DevMap

Resolve this checkout with `devmap paths --json`, then check `devmap status --json`. Read the returned `repo_map` path; do not assume `.devcouncil` rather than `.devmap`. A missing, stale, or partially parsed index is not complete evidence. Rebuild with `devmap build --manifest` when needed and authorized, then recheck status.

Use the connected DevMap MCP tools if they target this checkout; otherwise use the CLI from its root. Always pass `repo_path` (the absolute repository path) on every `devmap_*` and `gitpulse_codeintel_*` call, and check `repository.root` in the envelope before trusting the answer — Cursor shares one `devmap mcp` process across tabs. Discover the actual tool names and schemas; a host need not expose every CLI capability.

| Question | CLI | DevMap MCP, when available |
|---|---|---|
| Definition, callers, and callees | `devmap explore <name> --json` | `devmap_explore` |
| Find a name | `devmap search <name> --json` | `devmap_search` |
| Dependencies | `devmap deps <target> --json` | `devmap_dependencies` |
| Blast radius | `devmap impact <target> --json` | `devmap_impact` |
| Call chain | `devmap trace <from> <to> --json` | `devmap_trace` |
| Candidate tests | `devmap affected <target> --json` | `devmap_affected_tests` |
| Dead-code candidates | `devmap dead --json` | `devmap_dead_symbols` |
| Proposed edit | `devmap preview --help` for installed syntax | `devmap_preview` |

Read `available` or `resolution`, `reason`, `shown`, `total`, `truncated`, and `walk_incomplete` wherever present. Dead rows carry their own confidence: high means no inbound evidence; `only_ambiguous_callers` / unresolved-namesake / coverage-capped rows are unconfirmed; `NoNamesake` is an explained ledger gap, not a dead finding. A partial walk can omit callers even when `truncated` is false. Candidate tests are not measured test coverage; direct callers may be affected, not necessarily broken. Check source and actual tests before claiming a cause or safe deletion.

Prefer DevMap for the questions it can answer. If a required capability is absent, the index is unusable, or the answer is materially incomplete, state the specific limitation and use direct source inspection. Do not run a second index routinely. Follow explicit user and repository requirements. Check `devmap --help` and connected tool schemas before declaring a capability absent; CLI and MCP capabilities differ.

Report gaps in the task's audit or response. A skill does not authorize creating telemetry files, starting watchers, rebuilding repeatedly, or changing another host's configuration.
