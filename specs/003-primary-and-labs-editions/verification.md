# Verification Plan: 003 - Waylode Primary (Free) & Waylode Labs Editions

## Deterministic Gates

1. **Specification validation**
   - `devcouncil spec validate 003 --strict --json --project-root /Users/bharath/Code/devtools/GitPulse`
2. **Host edition gates** (Rust)
   - `cargo test --manifest-path src-tauri/Cargo.toml free_tier_and_disabled_opt_in_refuse_with_exit_4`
   - `cargo test --manifest-path src-tauri/Cargo.toml primary_offline_and_tier_crypto_gates_enforce_zero_calls`
   - `cargo test --manifest-path src-tauri/Cargo.toml pro_team_and_enterprise_byoc_bundle_validation`
   - `cargo test --manifest-path src-tauri/Cargo.toml remote`
3. **Disclosure contract**
   - `bun run vitest run scripts/update-privacy-contract.test.ts`
4. **Contract parity** (after `[T-08]` and `[T-09]` land)
   - `cargo test --manifest-path src-tauri/Cargo.toml edition_contract`

## Manual Review

- Confirm `git diff -- specs/003-primary-and-labs-editions/contracts/` is intentional whenever a capability or tier changes, and that the same change appears in `src-tauri/src/remote/config.rs`.
- Confirm no Labs-only file or link is added to the public tree; `labs/**` stays in the private `GitPulse-labs` repository.

## Not Covered Here

Relay server behavior, billing, R2 storage and the external E2EE review are outside this repository and are listed under "Unverified" in `research.md`.
