# Data Model: 003 - Waylode Primary (Free) & Waylode Labs Editions

## 1. Wire Tier (`Tier`, `src-tauri/src/remote/config.rs:47`)

| Wire ID | Edition (`Tier::edition`) | Contract display name | Remote control allowed | Space capabilities allowed |
| --- | --- | --- | --- | --- |
| `free` | `primary` | Waylode Primary / Free Inbox | no | no |
| `pro` | `labs` | Labs Geek | yes | no |
| `team` | `labs` | Labs Geek Squad | yes | yes |
| `enterprise` | `labs` | Enterprise | yes | yes |

## 2. Relay Capability (`RelayCapability`, `config.rs`)

| Capability string | Minimum tier | Crypto-audit gate |
| --- | --- | --- |
| `share_receiving_inbox` | `free` | no |
| `device_sync` | `pro` | yes |
| `task_sharing_5`, `r2_devmap_cache`, `mobile_steering`, `private_pr_gates` | `pro` | no |
| `space_radar`, `team_presence`, `team_blast`, `cloud_a2a`, `team_killswitch` | `team` | yes |
| `org_signed_policy_bundles`, `byoc_relay` | `enterprise` | no |

## 3. Persisted And Effective Configuration

- **`RemoteControlConfig`** (file `remote-control.json`, `REMOTE_CONFIG_FILE`): `tier`, `signed_in`, `remote_control_enabled`, `allow_unreviewed_crypto`, `allow_teammate_auto_wake`, `relay_url`, `keychain_access_group`, `space_bindings`. The default is `free`, signed out, disabled.
- **`EffectiveRelayConfig`**: the result of a passed gate, carrying the resolved relay URL, approval policy, `pty_input_entitled`, `org_id` and `workspace_roots`.
- **`SpaceRepoBinding`**: `{ space_id, repo_root, canonical_remote?, opt_in_share }`. `opt_in_share` defaults to false and `repo_root` is never sent to the relay.
- **`RelayGateError`**: `FreeOrDisabled` and `CryptoAuditGateActive` (exit `4`), `InvalidEnterpriseBundle` and `InvalidRelayUrl` (exit `1`).
- **Enterprise bundle**: `{ org_id, tier = enterprise, relay_url?, remote_approval, allow_pty_input, issued_at, expires_at, key_id, sig_ed25519_b64 }`, signed over a `gitpulse-enterprise-policy/v1` domain-separated payload.

## 4. Edition Contract (`contracts/edition-boundary.schema.json`)

JSON Schema 2020-12 document with `schema_version = 1`, `primary_edition` (`requires_sign_in = false`, `paywalled = false`, `byok_compute_allowed = true`), exactly four `wire_tiers`, and four `invariants` all fixed to true: `zero_local_paywalls`, `zero_telemetry_in_primary`, `relay_auth_required_for_labs`, `e2ee_audit_required_before_customer_payloads`.
