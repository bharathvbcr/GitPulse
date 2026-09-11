# Code navigation for every agent

Start with DevMap in this repository. From the repository root, run
`devmap paths --json` to resolve the database and `repo_map` paths, then run
`devmap status --json` before relying on graph answers. Read the resolved
`repo_map` file for ownership; do not assume the legacy
`.devcouncil/repo_map.json` location.
Generated state is per-worktree and is not copied by Git. If the store or map
is missing, run `devmap build --manifest` from this worktree's root, then check
status and read the map again. A plain build updates only the database.
Never copy another worktree's database to conceal missing local state.
The CLI and GitPulse MCP both query the DevMap store:

- Use `gitpulse_codeintel_search` with this repository's absolute `repo_path`,
  or `devmap search <name> --json`, for symbol discovery.
- Use `devmap explore <name> --json` for callers and callees and
  `devmap impact <target> --json` before editing the implementation.
- Use `devmap affected <target> --json` for candidate tests. Read `truncated`,
  `shown`, `total`, and `walk_incomplete`; partial results never justify skipping
  the full required checks.
- If DevMap is unavailable or stale, state the reason and use source inspection
  or GitNexus while rebuilding with `devmap build --manifest`. An unavailable
  query is not evidence that there are no callers or affected tests.

## Tool precedence and skills

This maintained section takes precedence over the generated GitNexus block below,
including its unconditional MUST/NEVER instructions. DevMap is the primary index.
GitNexus is optional: use it when DevMap lacks a required capability, is unavailable,
or leaves a material evidence gap, or when the user explicitly requests it. State
that reason before switching. Do not routinely run or rebuild both indexes.

Before editing, use DevMap impact and source inspection to assess callers and risk.
Before committing, review the full Git diff and query impact/affected tests for the
changed symbols. If GitNexus is used as a fallback, follow its applicable impact and
change checks and report HIGH/CRITICAL risk. Neither index replaces source review,
regression tests, or the repository's required verification commands.

Use these installed skills for the matching task:

- General navigation: [.agents/skills/devmap/SKILL.md](.agents/skills/devmap/SKILL.md)
- Debugging: [.agents/skills/devmap-debugging/SKILL.md](.agents/skills/devmap-debugging/SKILL.md)
- Exploration: [.agents/skills/devmap-exploring/SKILL.md](.agents/skills/devmap-exploring/SKILL.md)
- Impact: [.agents/skills/devmap-impact/SKILL.md](.agents/skills/devmap-impact/SKILL.md)
- Refactoring: [.agents/skills/devmap-refactoring/SKILL.md](.agents/skills/devmap-refactoring/SKILL.md)

Keep this section outside the generated block so reindexing preserves precedence.
Do not add DevMap's whole-file ownership marker to this mixed, maintained guide.

<!-- gitnexus:start -->
# GitNexus — Code Intelligence

This project is indexed by GitNexus as **GitPulse** (11294 symbols, 29940 relationships, 300 execution flows). Use the GitNexus MCP tools to understand code, assess impact, and navigate safely.

> Index stale? Run `node .gitnexus/run.cjs analyze` from the project root — it auto-selects an available runner. No `.gitnexus/run.cjs` yet? `npx gitnexus analyze` (npm 11 crash → `npm i -g gitnexus`; #1939).

## Always Do

- **MUST run impact analysis before editing any symbol.** Before modifying a function, class, or method, run `impact({target: "symbolName", direction: "upstream"})` and report the blast radius (direct callers, affected processes, risk level) to the user.
- **MUST run `detect_changes()` before committing** to verify your changes only affect expected symbols and execution flows. For regression review, compare against the default branch: `detect_changes({scope: "compare", base_ref: "main"})`.
- **MUST warn the user** if impact analysis returns HIGH or CRITICAL risk before proceeding with edits.
- When exploring unfamiliar code, use `query({search_query: "concept"})` to find execution flows instead of grepping. It returns process-grouped results ranked by relevance.
- When you need full context on a specific symbol — callers, callees, which execution flows it participates in — use `context({name: "symbolName"})`.
- For security review, `explain({target: "fileOrSymbol"})` lists taint findings (source→sink flows; needs `analyze --pdg`).

## Never Do

- NEVER edit a function, class, or method without first running `impact` on it.
- NEVER ignore HIGH or CRITICAL risk warnings from impact analysis.
- NEVER rename symbols with find-and-replace — use `rename` which understands the call graph.
- NEVER commit changes without running `detect_changes()` to check affected scope.

## Resources

| Resource | Use for |
|----------|---------|
| `gitnexus://repo/GitPulse/context` | Codebase overview, check index freshness |
| `gitnexus://repo/GitPulse/clusters` | All functional areas |
| `gitnexus://repo/GitPulse/processes` | All execution flows |
| `gitnexus://repo/GitPulse/process/{name}` | Step-by-step execution trace |

## CLI

| Task | Read this skill file |
|------|---------------------|
| Understand architecture / "How does X work?" | `.claude/skills/gitnexus/gitnexus-exploring/SKILL.md` |
| Blast radius / "What breaks if I change X?" | `.claude/skills/gitnexus/gitnexus-impact-analysis/SKILL.md` |
| Trace bugs / "Why is X failing?" | `.claude/skills/gitnexus/gitnexus-debugging/SKILL.md` |
| Rename / extract / split / refactor | `.claude/skills/gitnexus/gitnexus-refactoring/SKILL.md` |
| Tools, resources, schema reference | `.claude/skills/gitnexus/gitnexus-guide/SKILL.md` |
| Index, status, clean, wiki CLI commands | `.claude/skills/gitnexus/gitnexus-cli/SKILL.md` |

<!-- gitnexus:end -->
