# Data Model: 001 - Local-First ADE, Git Client & Worktree Orchestration

## 1. Frontend & Appearance Schemas

- **`ViewTab` & `VIEW_REGISTRY`** (`src/lib/repos/persist.ts`, `src/lib/views/viewRegistry.ts`):
  - Views (`5`): `work`, `mavi`, `code`, `history`, `insights`.
  - Sections (`11` total across views; `Mavi` has `0` sub-sections); Health sub-sections (`13`, `src/lib/health/sections.ts`).
- **Theme Schema (`waylode-theme/1`, accepting `gitpulse-theme/1`)** (`src/lib/ui/appearance/themeSchema.ts`, `presets.ts`, `brandLogo.ts`, `brand/waylode-mark.json`):
  - `schema`: `"waylode-theme/1" | "gitpulse-theme/1"`
  - `appearance`: `"dark" | "light"`
  - `colors`: `{ base, accent, contrast?, ok?, warn?, danger?, info?, text?, textMuted? }`
  - `brandLogo`: style ∈ `{ "way-arc", "folded-ribbon", "lode-compass" }`, palette ∈ `{ "cobalt", "lodestar", "oxblood", "gilded" }`
  - Built-in gallery: `21` presets (`4` Waylode Brand + `17` existing), `11` wallpapers (`backdrops.ts`).

## 2. Worktree, Agent Bus & Build Cache State

- **Worktree Checkouts**: `.gitpulse/worktrees/<task>-<id>` on branch `gitpulse/<task>-<id>` (excluded via `.git/info/exclude`; manual creation refused via `refuse_gitpulse_lane_target`).
- **Agent Coordination Bus** (`<git-common-dir>/gitpulse/lanes/`):
  - Lock file: `.bus.lock` (`flock(2)`).
  - Limits: `64` active lanes, `128` claimed paths/symbols per lane, `256` retained messages, `16 KiB` body cap, `64 KiB` radar wire cap (`truncated: true` when cut).
- **Compiler Cache** (`<git-common-dir>/gitpulse/build-cache/` via `waylode-rustc`):
  - Stage-1/Stage-2 Blake3 keying over compiler args, `.d` dependency files, and `# env-dep:` variables; Pure-AST proc-macro allowlist + `CARGO_MANIFEST_DIR` lexical guard (`crates/waylode-worktree/src/build_cache.rs`).

## 3. Terminal Host Protocol (`waylode-ptyd`) & Mavi Payloads

- **Frame Layout** (`src-tauri/src/terminal/host/proto.rs`, `PROTOCOL = 5`):
  - `[4B BE header_len][JSON header (<= 512 KiB)][4B BE payload_len][raw bytes (<= 64 KiB)]`
  - Protocol 5 additions: `screen` notice channel (`note_prompt_notice` -> `agent_prompt` -> `waylode_terminal_wait` `matched_reason: "needs_input"`), `Telemetry` prompt fields, `compat_v4.rs` fallback.
- **Spawn Options** (`src-tauri/src/terminal/orchestrate.rs`):
  - `model`: validated model identifier string.
  - `mode`: `inspect | ask | edit | auto_review | preapproved` (`bypass` never permitted via spawn tool).
  - `settings`: repo-relative JSON path (`<= 64 KiB`, rejecting `bypassPermissions` or `disableAllHooks`).
- **Mavi Thread & Question Contract** (`src-tauri/src/mavi/`, `src-tauri/src/commands/terminals.rs`):
  - `StartRequest.threadKey` -> `WAYLODE_ORCHESTRATE_CALLER=ext-mavi-<uuid>`.
  - `AskUserQuestion` card: decline/close emits `QUESTION_DECLINED` + `AGENT_GUIDANCE` (unanswered/declined is never consent).
  - `cmd_mavi_close_thread_children` -> `MaviThreadChildrenClosed { closed, skippedProtected }`.

## 4. Host-Load Snapshot & Leases (`src-tauri/src/sysload/`)

- **Snapshot (`~/Library/Application Support/Waylode/host-load.v1`)**:
  - `816`-byte fixed-size sequence-locked binary record (`GPHL` header, `GPX3` governor/segment block `664..712`, `GPXM` metrics block `712..816`), `0600` regular file (`O_NOFOLLOW`), read-only for `waylode-mcp` and `waylode-hook`.
- **Leases (`<config>/Waylode/leases/` & `lease-bin/{cargo,go}`)**:
  - `slot-1..slot-16` POSIX record locks (`F_SETLK`), `queue/<mono_ns>-<pid>.json`, `WAYLODE_LEASE_HELD` / legacy `GITPULSE_LEASE_HELD`, `WAYLODE_RUN_ID` / `WAYLODE_SESSION_ID`.
