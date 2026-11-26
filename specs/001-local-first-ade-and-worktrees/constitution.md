# Constitution & Invariants: 001 - Local-First ADE, Git Client & Worktree Orchestration

1. **Truthful Incomplete State**: Missing DevMap indexes, unreadable `host-load.v1` snapshots, unscanned worktrees, and absent `manvi` harnesses MUST render explicit incomplete/unchecked states (`Unchecked`, `not scanned`, `could not read`), never `Allowed` or `0`.
2. **Typed IPC & Schema Lockstep**: Every `cmd_*` handler (`370`) and Serde IPC payload MUST stay verified by `bun run check:ipc` and `bun run check:types` without orphaned handlers or runtime-only shadow payloads.
3. **Host Safety Boundaries**: Repository trust is granted exclusively by the desktop app (`repository_trust`); `waylode_terminal_spawn` MUST reject `bypass` mode; `AskUserQuestion` in `Mavi` MUST never treat unanswered or declined prompts as consent.
4. **Compatibility Without Shadow Implementations**: Legacy identifiers (`.gitpulse/`, `refs/gitpulse/*`, `WAYLODE_MCP_EXPOSE_LEGACY=1`, `GITPULSE_*` env fallbacks) are compatibility seams over one canonical Waylode engine, never separate code paths.
5. **Cross-Repo Non-Overlap**: Code-intelligence engine ownership stays with `DevCouncil`, policy decision/agent runtime ownership stays with `Manvi`, and Waylode owns desktop ADE presentation, Git sandbox, PTY host, and worktree/load UX.
