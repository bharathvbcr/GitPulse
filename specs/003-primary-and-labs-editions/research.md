# Research & Design Rationale: 003 - Waylode Primary (Free) & Waylode Labs Editions

## 1. Code-Verified Findings (2026-11-28)

1. **Primary offline is a single predicate.** `src-tauri/src/remote/config.rs:536-541` defines `is_primary_offline` as `(tier == Free && !signed_in) || !remote_control_enabled`. `src-tauri/src/commands/labs.rs:1-10` states that every read and action returns before any socket, HTTP request or Keychain prompt when it is true. The test `primary_offline_and_tier_crypto_gates_enforce_zero_calls` in `src-tauri/src/remote/tests.rs` asserts `mock.request_count() == 0`.
2. **The relay refuses free and disabled configurations with exit `4`.** `EXIT_CODE_FREE_OR_DISABLED` is defined at `config.rs:22` and returned from `src-tauri/src/remote/mod.rs` at several refusal points (lines 131, 437, 444, 451, 564, 565).
3. **Capability matrix.** `Tier::allows_capability` (`config.rs:83`) gives `ShareReceivingInbox` to every tier, personal capabilities (`DeviceSync`, `TaskSharing`, `R2DevmapCache`, `MobileSteering`, `PrivatePrGates`) to `pro` and above, Space capabilities to `team` and above, and `OrgSignedPolicyBundles` and `ByocRelay` to `enterprise` only.
4. **Crypto-audit gate.** `RelayCapability::requires_crypto_audit_gate` (`config.rs:143`) is true for `DeviceSync`, `SpaceRadar`, `TeamPresence`, `TeamBlast`, `CloudA2a` and `TeamKillswitch`. `EffectiveRelayConfig::allows_capability` also refuses a `free` tier that is not signed in.
5. **Enterprise bundles.** `config.rs:200-320` verifies an Ed25519 signature over a domain-separated payload (`ORG_BUNDLE_DOMAIN`), requires `tier == enterprise`, and applies `CLOCK_SKEW_SECS = 300` and `MAX_ORG_BUNDLE_TTL_SECS = 365 * 24 * 3600`.
6. **The schema is referenced by comment only.** `src-tauri/src/remote/config.rs:106` and `src-tauri/src/commands/labs.rs:3` name `edition-boundary.schema.json`. A search outside `specs/003-*` finds no test, script or build step that reads the schema file. This is the basis for US5 and `[T-08]`.
7. **Contract versus code differences.** `RelayCapability` has 13 variants. The schema example lists 15 capability strings across 4 tiers (10 shared with code). `team_presence`, `team_blast` and `cloud_a2a` exist only in code. `unlimited_private_pr_gates`, `team_spend_rollups`, `siem_soc2_audit_export`, `oidc_scim` and `mdm` exist only in the schema.8. **Disclosure.** `TELEMETRY.md` states there is no analytics or crash SDK and `PRIVACY.md` lists each network path. `PRIVACY.md` records that sign-in and account code (24 files matched) was not fully read, so the "no sign-in on `main`" statement is partly audited.

## 2. Alternatives Considered

| Decision | Chosen | Rejected | Why |
| --- | --- | --- | --- |
| Where the boundary is enforced | One predicate plus per-capability gate in the host (`config.rs`) | Per-feature checks scattered across commands | A single owner means a new Labs command inherits the offline guarantee. |
| Wire tier IDs | Keep `free`, `pro`, `team`, `enterprise` and map display names in the contract | Rename IDs to the Labs product names | Renaming would touch relay tables and shipped clients (`plan.md` Complexity Tracking). |
| Free inbox requires sign-in | Sign-in required on relay routes only | Anonymous inbox | Prevents anonymous abuse of relay storage; local use is unaffected. |

## 3. Unverified

- **Hosted relay behavior.** `gitpulse-relay` and `GitPulse-labs` are not in this checkout. Tier resolution, Stripe billing, R2 storage, `unlimited_private_pr_gates`, `team_spend_rollups`, SIEM export, OIDC/SCIM and MDM are described only in `docs/SUBSCRIPTION_IMPLEMENTATION_PLAN.md` and were not verified in code.
- **Test results.** The cargo tests named in `tasks.md` were located by name in `src-tauri/src/remote/tests.rs` but were not run for this spec. The shared checkout has uncommitted work from other sessions, and a Rust build there would contend with it.
- **Relay end-to-end test.** `src-tauri/tests/remote_relay_e2e.rs` builds the relay from a sibling directory (`locate_relay_dir`) and needs a Go toolchain. Whether it ran here was not checked.
- **Licensing of the relay.** The `FCL-1.0-MIT` license for `gitpulse-relay` is taken from `plan.md` and `docs/SUBSCRIPTION_IMPLEMENTATION_PLAN.md`, not from the relay repository.
- **Planned tier pricing and margin figures** in `docs/SUBSCRIPTION_IMPLEMENTATION_PLAN.md` are plans and are not asserted by this spec.
