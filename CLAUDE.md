<!-- agent-navigation:start -->
## Navigate with DevMap and GitPulse Insights

Ask these two before reading files. They answer different questions, neither substitutes for the other, and neither is `rg`.

**DevMap — where the code is and what it touches.** `devmap_search` to locate, `devmap_explore` for structure, `devmap_neighbors` and `devmap_impact` for callers and blast radius, `devmap_trace` for how two symbols connect, `devmap_affected_tests` for what to run, `devmap_dead_symbols` before deleting, `devmap_clones` before adding a near-copy, `devmap_preview` to ask what an unsaved edit would break. The `devmap` CLI answers the same questions when the MCP tools are not loaded. Pass `repo_path` (the absolute repository path) on every `devmap_*` call and check `repository.root` in the envelope. Read `truncated` and `walk_incomplete` before treating an empty list as "nothing exists" — on an unbuilt or degraded index every question answers "nothing", and `devmap_status` is what distinguishes the two. Generated state is per-worktree and is not copied by Git; if the store is missing, run `devmap build --manifest` from this worktree's root rather than copying a sibling's database.

**GitPulse Insights — who else is in this repository right now.** `gitpulse_insights` first: it names the other worktrees, the running agent sessions, uncommitted work, contended files and index health in one call. Then `gitpulse_collision_risk` before touching a file another worktree may hold, `gitpulse_active_changes` for what is in flight, `gitpulse_change_context` / `gitpulse_provenance` for what changed and why, and `gitpulse_ledger_events` for the recorded history. It is served over MCP only — there is no `gitpulse` CLI. Its facets fail independently: check each `ok`, because a facet that could not scan is not a facet that came back clean.

Navigating the graph perfectly and then editing a file a sibling lane is holding is still a collision, which is why both are named here.

This binds every agent working in this repository, not only the one reading this file. When you delegate — a subagent, a worktree lane, a handoff plan — name DevMap and GitPulse Insights in the prompt with the same precedence, rather than leaving that agent to grep its way in. When either tool cannot answer (truncated, `walk_incomplete`, empty on a built index, or a capability it does not have), record the gap and say so; do not silently fall back to grep and report the result as if the graph had confirmed it.
<!-- agent-navigation:end -->

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
