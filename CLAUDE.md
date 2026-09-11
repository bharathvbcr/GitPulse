# Code navigation for every agent

## Product stack

DevCouncil is **components and modules**. Manvi wraps them. GitPulse uses
Manvi for policy, workbench, and agent hosting, and DevCouncil components
(`devmap` CLI, vendored `devmap-*` crates, verification reads) for code
intelligence. Prefer the existing module rather than copying it. Update one
module at a time; do not assume the whole suite is required. Canonical write-up:
[docs/MODULE_INTEGRATION.md](docs/MODULE_INTEGRATION.md).

Start with DevMap in this repository. From the repository root, run
`devmap paths --json` to resolve the database and `repo_map` paths, then run
`devmap status --json` before relying on graph answers. Read the resolved
`repo_map` file for ownership; do not assume the legacy
`.devcouncil/repo_map.json` location.
Generated state is per-worktree and is not copied by Git. If the store or map
is missing, run `devmap build --manifest` from this worktree's root, then check
status and read the map again. A plain build updates only the database.
Never copy another worktree's database to conceal missing local state.
The CLI and GitPulse MCP both query the DevMap store. Always pass this
repository's absolute `repo_path` on every `devmap_*` and
`gitpulse_codeintel_*` call, and check `repository.root` in the envelope
before trusting the answer — Cursor shares one `devmap mcp` process across
workspace tabs.

- Use `devmap_search` / `gitpulse_codeintel_search` with this repository's
  absolute `repo_path`, or `devmap search <name> --json`, for symbol discovery.
- Use `devmap_explore` / `devmap explore <name> --json` for callers and callees
  and `devmap_impact` / `devmap impact <target> --json` before editing.
- Use `devmap_affected_tests` / `devmap affected <target> --json` for candidate
  tests. Read `truncated`, `shown`, `total`, and `walk_incomplete`; partial
  results never justify skipping the full required checks.
- Prefer the full `devmap_*` tool set on `gitpulse-mcp` (status, search,
  dependencies, impact, trace, neighbors, dead_symbols, clones, preview,
  explore, affected_tests). Preview needs the parse frontend: when
  `gitpulse-mcp` reports it unavailable, use `devmap preview` or the global
  `devmap mcp` server.
- If DevMap is unavailable or stale, state the reason and use source inspection
  while rebuilding with `devmap build --manifest`. An unavailable query is not
  evidence that there are no callers or affected tests. A local GitNexus
  install may remain as a manual fallback only when the user explicitly asks
  for it; it is not part of product guidance.

## Tool precedence and skills

DevMap is the primary index. Before editing, use DevMap impact and source
inspection to assess callers and risk. Before committing, review the full Git
diff and query impact/affected tests for the changed symbols. Neither the
index nor a skill replaces source review, regression tests, or the
repository's required verification commands.

Use these installed skills for the matching task:

- General navigation: [.agents/skills/devmap/SKILL.md](.agents/skills/devmap/SKILL.md)
- Debugging: [.agents/skills/devmap-debugging/SKILL.md](.agents/skills/devmap-debugging/SKILL.md)
- Exploration: [.agents/skills/devmap-exploring/SKILL.md](.agents/skills/devmap-exploring/SKILL.md)
- Impact: [.agents/skills/devmap-impact/SKILL.md](.agents/skills/devmap-impact/SKILL.md)
- Refactoring: [.agents/skills/devmap-refactoring/SKILL.md](.agents/skills/devmap-refactoring/SKILL.md)

Also available under `.cursor/skills/` and `.cursor/rules/devmap.mdc` for Cursor.
Do not add DevMap's whole-file ownership marker to this mixed, maintained guide.
