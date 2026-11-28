# Quickstart: 003 - Waylode Primary (Free) & Waylode Labs Editions

All commands run from `/Users/bharath/Code/devtools/GitPulse`.

1. Validate this spec:
   `devcouncil spec validate 003 --strict --json --project-root /Users/bharath/Code/devtools/GitPulse`
2. Run the edition gate tests (Rust build required):
   - `cargo test --manifest-path src-tauri/Cargo.toml free_tier_and_disabled_opt_in_refuse_with_exit_4`
   - `cargo test --manifest-path src-tauri/Cargo.toml primary_offline_and_tier_crypto_gates_enforce_zero_calls`
   - `cargo test --manifest-path src-tauri/Cargo.toml pro_team_and_enterprise_byoc_bundle_validation`
3. Run the privacy disclosure contract: `bun run vitest run scripts/update-privacy-contract.test.ts`
4. Read the contract: `specs/003-primary-and-labs-editions/contracts/edition-boundary.schema.json`. Its `examples[0]` is the reference document that `[T-08]` compares with `src-tauri/src/remote/config.rs`.
5. Run the whole spec gate: `bun run check:specs`
