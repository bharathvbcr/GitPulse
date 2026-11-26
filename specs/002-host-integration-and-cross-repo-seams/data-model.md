# Data Model: 002 - Host Integration & Cross-Repo Seams

## 1. Vendored Manifest (`src-tauri/vendored/VENDOR.json`)

- **Structure**: `{ note: string, crates: [{ name, origin: { repo, root_env, path, commit }, omitted: string[], rewrites: string[], files: Record<string, sha256> }] }`
- **Inventory**: `16` crates total (`12` DevCouncil: `dc-evidence`, `dc-glob`, `dc-proc`, `dc-redact`, `dc-regress`, `dc-regress-store`, `dc-store`, `devmap-analyze`, `devmap-extract`, `devmap-query`, `devmap-resolve`, `devmap-store`; `4` MarkDev: `markdev-highlight`, `markdev-html`, `markdev-md`, `markdev-vault`).

## 2. Five-Schema Parity Matrix (`scripts/check-schema-parity.mjs`)

| Schema Key | Constant | Upstream Owner File | Checked Readers |
| --- | --- | --- | --- |
| `store` | `CURRENT_SCHEMA_VERSION` (`29`) | `DevCouncil/rust/devmap-store/src/schema.rs` | `src-tauri/vendored/devmap-store`, `waylode-mcp` (PATH & app bundle), `devmap` (PATH), Manvi & Jarvis pins |
| `dcStore` | `SCHEMA_VERSION` | `DevCouncil/rust/dc-store/src/lib.rs` | `src-tauri/vendored/dc-store`, `waylode-mcp`, Manvi pin |
| `workbench` | `WORKBENCH_SCHEMA` | `DevCouncil/rust/dc-store/src/workbench/mod.rs` | `src-tauri/vendored/dc-store`, `waylode-mcp`, `dcstore` (PATH), Manvi pin |
| `extract` | `EXTRACTION_SCHEMA_VERSION` | `DevCouncil/rust/devmap-extract/src/cache.rs` | `src-tauri/vendored/devmap-extract`, `waylode-mcp`, `devmap` (PATH), Manvi & Jarvis pins |
| `lease` | `STACK_FORMAT_VERSION` | `DevCouncil/rust/devmap-query/src/stacked.rs` | `src-tauri/vendored/devmap-query`, `waylode-mcp`, `devmap` (PATH), Manvi pin |

- **Verdict Lattice**: `match | stale | unverifiable | absent` (only `match` and `absent` exit `0`).
- **Contract Parity**: `contracts/serve.ops.schema.json` compared byte-for-byte with `Manvi/contracts/serve.ops.schema.json`.

## 3. Sidecar & Host Registration Payloads

- **`HarnessStatus`** (`src-tauri/src/harness/mod.rs:44`): `{ available, binary, protocol, posture, ops: string[], error, error_code }`.
- **`IntegrationHost` & `IntegrationKind`** (`src-tauri/src/devmap/integrate.rs:46,69`): `Claude | Cursor | Codex` × `Guide | ProjectMcp | GlobalMcp | Hook | Skill`.
- **`ConnectPlan` / `ConnectAction`** (`src-tauri/src/agent_connect.rs:59`): `{ id, kind: Link | Register, agent?, title, target, state: Todo | Done | Blocked, detail, commands }`, pairing `SERVER_NAME = "waylode"` and `LEGACY_SERVER_NAME = "gitpulse"`.
- **Antigravity Managed Bundle** (`src-tauri/src/workbench/agy_plugin.rs`): `~/.gemini/config/plugins/waylode/` with `.waylode-managed` sentinel, `./bin/waylode-mcp`, `./bin/waylode-hook`, and `PostToolUse`-only `hooks.json`.
