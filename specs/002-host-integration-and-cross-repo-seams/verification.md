# Verification Plan: 002 - Host Integration & Cross-Repo Seams

## Deterministic Cross-Repo & Contract Gates

1. **Specification Validation**:
   - `devcouncil spec validate 002 --strict --json --project-root /Users/bharath/Code/devtools/GitPulse`
2. **Schema, Vendoring & Contract Parity**:
   - `bun run vendor:check`
   - `bun run check:vendor-schema`
   - `bun run check:schema-parity`
   - `bun run vitest run scripts/check-schema-parity.test.ts scripts/verdict-contract.test.ts scripts/plugin-contract.test.ts scripts/agent-guidance.test.ts`
3. **Documentation & Rust Seam Suites**:
   - `devmap doc-drift --json`
   - `cargo test --manifest-path src-tauri/Cargo.toml devmap_parity`
   - `cargo test --manifest-path src-tauri/Cargo.toml preflight`
   - `cargo test --manifest-path src-tauri/Cargo.toml agent_connect`
4. **Upstream Alignment Check**:
   - Cross-check `specs/002-host-integration-and-cross-repo-seams/contracts/cross-repo-seam-manifest.json` against `DevCouncil/specs/003-stack-boundaries-and-non-overlap/contracts/ownership-matrix.json` and confirm `git diff -- contracts/ src-tauri/vendored/` is empty.
