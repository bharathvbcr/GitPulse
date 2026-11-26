# Verification Plan: 001 - Local-First ADE, Git Client & Worktree Orchestration

## Deterministic Contract & Validation Gates

1. **Specification Validation**:
   - `devcouncil spec validate 001 --strict --json --project-root /Users/bharath/Code/devtools/GitPulse`
2. **Documentation Drift & Contract Suite**:
   - `devmap doc-drift --json` (confirm `0` findings across active in-scope maintained docs)
   - `bun run vitest run scripts/brand-rename-contract.test.ts scripts/architecture-docs-contract.test.ts scripts/documented-counts-contract.test.ts scripts/archive-separation-contract.test.ts scripts/adv-l5-cursor-docs.test.ts scripts/l5-cursor-docs-claims.test.ts scripts/verdict-contract.test.ts scripts/agent-guidance.test.ts scripts/release-workflow.test.ts scripts/plugin-contract.test.ts scripts/bench-gate-adversary.test.ts`
3. **IPC & Type Surface Contracts**:
   - `bun run check:ipc` (`370` registered handlers)
   - `bun run check:types` (`141` contracts, `372` structs, `2614` fields)
4. **Targeted Runtime & Crate Verification**:
   - `cargo test --manifest-path crates/waylode-worktree/Cargo.toml`
   - `cargo test --manifest-path src-tauri/Cargo.toml --lib terminal`
   - `cargo test --manifest-path src-tauri/Cargo.toml --lib mavi`
   - `cargo test --manifest-path src-tauri/Cargo.toml --lib sysload`
