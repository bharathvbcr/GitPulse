# Waylode Specifications

7-layer DevCouncil specifications for **Waylode** (`/Users/bharath/Code/devtools/GitPulse`), paired with the cross-repo master ownership contract in `DevCouncil/specs/003-stack-boundaries-and-non-overlap` ([cross-repo master spec](https://github.com/bharathvbcr/DevCouncil/blob/main/specs/003-stack-boundaries-and-non-overlap/spec.md)).

| Spec | Scope | Canonical Owner | Status |
| --- | --- | --- | --- |
| [001 - Local-First ADE, Git Client & Worktree Orchestration](001-local-first-ade-and-worktrees/spec.md) | 5-view/11-section UI, Cobalt Way-Arc brand & presets, Git sandbox, zero-copy worktrees & radar, `waylode-ptyd` protocol 5, Mavi threads, `waylode-mcp` (66 tools), load governor & leases | Waylode | Implemented (`5fcea3dbc`) with tracked doc/harness follow-ups |
| [002 - Host Integration & Cross-Repo Seams](002-host-integration-and-cross-repo-seams/spec.md) | `src-tauri/vendored/` read-side crates, 11-tool `devmap_*` parity, `manvi serve` sidecar, `agent_connect` / `agy_plugin` vs `devmap integrate` audit, `waylode-hook`, `parse_git_version`, `check-schema-parity.mjs`, read-only `contracts/` | Waylode (cross-repo coordinated) | Active seams implemented; host-registration overlap closure tracked |

## Related Architecture & Contracts

- [Waylode philosophy](../docs/PHILOSOPHY.md)
- [Documentation index](../docs/README.md)
- [Module integration](../docs/MODULE_INTEGRATION.md)
- [Contracts overview](../contracts/README.md)
- Master cross-repo boundary spec (sibling DevCouncil repository): `DevCouncil/specs/003-stack-boundaries-and-non-overlap/contracts/ownership-matrix.json`
- [Cross-repo drift audit & 4-lane work split](../../DevCouncil/specs/003-stack-boundaries-and-non-overlap/WORK_SPLIT_AND_DRIFT_AUDIT.md) (sibling DevCouncil repo)
- [Executable stack-boundary gate](../../DevCouncil/scripts/check-stack-boundaries.mjs) (`--lanes`, `--route <repo>:<path>`)

## Validation

```sh
devcouncil spec validate 001 --strict --project-root /Users/bharath/Code/devtools/GitPulse
devcouncil spec validate 002 --strict --project-root /Users/bharath/Code/devtools/GitPulse
devcouncil spec validate 003 --strict --project-root /Users/bharath/Code/devtools/GitPulse
node ../DevCouncil/scripts/check-stack-boundaries.mjs --lanes
```
