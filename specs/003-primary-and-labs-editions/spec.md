# Feature: 003 - Waylode Primary (Free) & Waylode Labs Editions

> **Status (2026-11-28)**: The edition boundary is implemented in the Waylode host (`src-tauri/src/remote/config.rs`, `src-tauri/src/commands/labs.rs`) and pinned by `contracts/edition-boundary.schema.json`. The hosted side (`gitpulse-relay`, `GitPulse-labs`) lives in private repositories that this checkout does not contain, so claims about it are recorded as unverified in `research.md`. Canonical owner: **Waylode** (`/Users/bharath/Code/devtools/GitPulse`).

## 1. Problem Statement & Motivation

Waylode ships as one desktop app with two editions. **Waylode Primary** is free and open source (`MIT OR Apache-2.0`, see `LICENSE-MIT` and `LICENSE-APACHE`) and works with no account and no network beyond Git and the tools the user connects. **Waylode Labs** is a paid, signed-in subscription served by the separately licensed `gitpulse-relay` (tier IDs `free`, `pro`, `team`, `enterprise`).

Without an enforced boundary, three failures are likely: a local feature quietly gains a sign-in or paywall, the Primary app opens a socket or Keychain prompt before the user has opted in to Labs, or a Labs capability that depends on unreviewed end-to-end encryption (E2EE) runs against real customer data. This spec records the boundary as it exists in code, and the open work needed to make the contract and the code verifiably agree.

## 2. Canonical Ownership & Non-Overlap Boundary

| Boundary | Surfaces & Responsibilities | Source of Truth |
| --- | --- | --- |
| **Owns** | The edition contract (`specs/003-primary-and-labs-editions/contracts/edition-boundary.schema.json`); host-side tier and capability gating (`Tier`, `RelayCapability`, `evaluate_capability_gate`, `is_primary_offline` in `src-tauri/src/remote/config.rs`); the `wayloded --relay` refusal path (`src-tauri/src/remote/mod.rs`); the `cmd_labs_*` / `cmd_space_*` / `cmd_team_*` commands (`src-tauri/src/commands/labs.rs`); the `waylode_team_*` MCP surface (`src-tauri/src/mcp/team.rs`); the user-facing disclosures `PRIVACY.md` and `TELEMETRY.md`. | `/Users/bharath/Code/devtools/GitPulse` |
| **Delegates To** | Server-side entitlement resolution, billing, R2 storage and the relay routes → private `gitpulse-relay` (`internal/entitlements`). Labs feature registry and the `labs/**` modules → private `GitPulse-labs`. Policy evaluation inside an org-signed bundle → **Manvi**. | `gitpulse-relay`, `GitPulse-labs`, `Manvi` |
| **Non-Goals / Forbidden Overlaps** | Never add `requires_sign_in` or a paywall to a local or bring-your-own-key (BYOK) feature; never open a socket or touch the Keychain while Primary is offline; never ship `labs/**` code or links into the public build; never store or proxy the user's SSH, cloud or provider credentials; never carry billing logic into DevCouncil or Manvi. | `plan.md`, `src-tauri/src/remote/config.rs`, `docs/SUBSCRIPTION_IMPLEMENTATION_PLAN.md` |

## 3. User Stories & Acceptance Scenarios

### US1: Primary Works Offline With No Account (Priority: P1)
As a developer who has never signed in,
I want every local Waylode feature to work with no account, no paywall and no relay traffic,
So that the free edition is complete on its own and I am never prompted for a Keychain item I did not ask for.

**Acceptance Scenarios**:
- **Scenario 1: Default configuration is offline**
  - **Given** a default `RemoteControlConfig` (`tier = free`, `signed_in = false`, `remote_control_enabled = false`)
  - **When** `is_primary_offline` is evaluated and a relay capability gate or `RelayClient::list_spaces` is attempted
  - **Then** the gate returns `RelayGateError::FreeOrDisabled`, the client returns `RelayTransportError::PrimaryOffline`, and the mock transport records zero requests.
- **Scenario 2: `wayloded --relay` refuses before opening anything**
  - **Given** a `free` tier or `remote_control_enabled = false`
  - **When** `evaluate_relay_gate` runs
  - **Then** it fails with exit code `4` (`EXIT_CODE_FREE_OR_DISABLED`) before any Unix socket or network connection is opened.

### US2: Tier Gates Map Capabilities To Wire Tiers (Priority: P1)
As a Waylode Labs subscriber,
I want each hosted capability available only at its documented tier,
So that `free`, `pro` (Labs Geek), `team` (Labs Geek Squad) and `enterprise` behave exactly as the edition contract says.

**Acceptance Scenarios**:
- **Scenario 1: Signed-in free tier gets the share-receiving inbox only**
  - **Given** `tier = free`, `signed_in = true`, `remote_control_enabled = true`
  - **When** capability gates are evaluated
  - **Then** `ShareReceivingInbox` is allowed, and `MobileSteering` and `SpaceRadar` are refused with `FreeOrDisabled`.
- **Scenario 2: Pro allows personal capabilities but not Space capabilities**
  - **Given** `tier = pro`, signed in, `allow_unreviewed_crypto = true`
  - **When** `R2DevmapCache` and `TeamBlast` are evaluated
  - **Then** `R2DevmapCache` is allowed and `TeamBlast` is refused because `allows_space_labs` is false below `team`.

### US3: Unreviewed E2EE Capabilities Fail Closed (Priority: P1)
As a security reviewer,
I want device sync, Space presence, team blast, cloud A2A and the team kill switch to stay disabled until the external cryptographic review is recorded,
So that no real customer payload flows through unreviewed encryption.

**Acceptance Scenarios**:
- **Scenario 1: Team tier without the audit flag is refused**
  - **Given** `tier = team`, signed in, remote control enabled, `allow_unreviewed_crypto = false`
  - **When** a capability for which `requires_crypto_audit_gate` is true is evaluated
  - **Then** the gate returns `RelayGateError::CryptoAuditGateActive` (exit code `4`) and no relay call is made.

### US4: Enterprise Policy Bundles Are Signature-Verified (Priority: P2)
As an organization administrator,
I want a policy bundle to take effect only when it carries a valid Ed25519 signature from the pinned organization key inside its time window,
So that a forged, expired or wrong-tier bundle cannot change how agents are approved.

**Acceptance Scenarios**:
- **Scenario 1: Invalid bundles are rejected**
  - **Given** an organization bundle with `tier = enterprise` and `sig_ed25519_b64`, a pinned verifying key, and a TTL of at most `MAX_ORG_BUNDLE_TTL_SECS`
  - **When** the bundle is verified
  - **Then** a missing or bad signature, a non-enterprise tier, or an expired or too-long window yields `RelayGateError::InvalidEnterpriseBundle` (exit code `1`).

### US5: The Contract And The Code Cannot Silently Drift (Priority: P2)
As a Waylode maintainer,
I want a test that compares `contracts/edition-boundary.schema.json` with the `Tier` and `RelayCapability` definitions,
So that renaming a capability or flipping a tier's `requires_sign_in` fails a gate instead of shipping.

**Acceptance Scenarios**:
- **Scenario 1: Parity check between schema example and Rust**
  - **Given** the `examples[0]` document in the schema and `RelayCapability::as_str`, `RelayCapability::minimum_tier` and `Tier::edition`
  - **When** the parity test runs
  - **Then** every capability string in the example is produced by a Rust capability at the same tier and edition, and every Rust capability appears in the example or in a documented exception list.

## 4. Success Metrics & Testability Criteria

- **SC-001**: `cargo test --manifest-path src-tauri/Cargo.toml free_tier_and_disabled_opt_in_refuse_with_exit_4` passes, pinning the exit-code `4` refusal before any socket opens.
- **SC-002**: `cargo test --manifest-path src-tauri/Cargo.toml primary_offline_and_tier_crypto_gates_enforce_zero_calls` passes, asserting `request_count() == 0` in Primary offline mode and the free, pro and team capability matrix.
- **SC-003**: `cargo test --manifest-path src-tauri/Cargo.toml pro_team_and_enterprise_byoc_bundle_validation` passes, covering organization bundle validation.
- **SC-004**: `bun run vitest run scripts/update-privacy-contract.test.ts` passes, covering the disclosed network paths in `PRIVACY.md` and `TELEMETRY.md`.
- **SC-005**: `devcouncil spec validate 003 --strict --project-root /Users/bharath/Code/devtools/GitPulse` returns `"valid": true` with no blocking findings.
- **SC-006**: After `[T-07]` lands, the edition-contract parity test fails when a capability string, its minimum tier or its edition differs between the schema and `src-tauri/src/remote/config.rs`.

## 5. Edge Cases, Exclusions & Open Questions

- **Edge Case - Free inbox needs sign-in**: The contract sets `requires_sign_in: true` for the `free` wire tier because the inbox is a relay route. In code this is `EffectiveRelayConfig::allows_capability`, which refuses a `free` tier that is not `signed_in`. Local features are unaffected.
- **Edge Case - Contract is narrower than code**: `RelayCapability` includes `team_presence`, `team_blast` and `cloud_a2a`, which the schema example does not list. `[T-07]` decides whether the schema gains them or the parity test lists them as documented exceptions.
- **Exclusion - Hosted side**: Stripe billing, R2 offload, the `gitpulse-relay` route table and the `labs/**` modules are not in this checkout. This spec does not assert their behavior.
- **Exclusion - Unreviewed E2EE**: The external cryptographic review is a scheduled item in `docs/SUBSCRIPTION_IMPLEMENTATION_PLAN.md`. It is not complete, so the crypto-audit gate stays active.
- **Exclusion - Capabilities with no host code**: `unlimited_private_pr_gates`, `team_spend_rollups`, `siem_soc2_audit_export`, `oidc_scim` and `mdm` appear in the contract but have no host-side gate in `src-tauri/src/remote/config.rs`.
