# GitPulse: adoption gaps, the collision-radar wedge, and cloud conflict warnings

Written 2026-11-22 from two audit passes (Sonnet agents) and a Fable strategy review. Labels:

- **verified**: checked against a file or command that day.
- **vendor**: from the competitor's own site.
- **inferred**: reasoning, not yet checked.

The DevMap/DevCouncil half is in DevCouncil's `docs/launch/adoption-gaps.md`. The positioning and
recording plan is [LAUNCH_PLAN.md](LAUNCH_PLAN.md).

## Where GitPulse stands

- **Traction:** 18 GitHub stars (verified). Herdr, the closest launched comparison, reports ~25k
  stars and a $6M seed (vendor).
- **Signing:** `v0.2.0-beta.1` is a macOS DMG, ad-hoc signed and not notarized.
  INSTALLATION.md:32-45 tells users to run `xattr -dr com.apple.quarantine` (verified).
- **Setup:** the install path expects GitPulse, DevCouncil and Manvi, and a schema-parity check.
- **Docs:** the README links 40 or more docs (verified).
- **Strengths:**
  - Collision awareness at file, symbol (DevMap spans) and committed-work level (pairwise
    `merge-tree`).
  - A command gate with undo snapshots.
  - A task board that agents write to.
  - A full Git client.

## Wedge: the collision radar, usable without the app (decided by the owner, 2026-11-22)

The pitch is *"your agents are about to overwrite each other; one command shows where."* The
read-only audit found it is mostly there (verified unless noted):

- **It already runs headless.**
  - `insights::collision_risk` (src-tauri/src/insights/mod.rs:2448), `entity_collision`,
    `merge_conflicts` (insights/merge_conflicts.rs:92) and file claims import no `AppHandle`.
  - The app mailbox is an optional cache (insights/app_facets.rs:1-35).
  - The edit-time guard exists (hooks/mod.rs:1051, calling `collision_for_path` at :2798).
- **What couples it to the app:**
  1. `gitpulse_lib` links the forked Tauri, so a headless build compiles wry.
  2. Symbol verdicts need a DevMap index at the worktree HEAD, and nothing refreshes it without the
     app, so verdicts fall to `FileLevel`.
  3. Claims see Claude Code's registry and hook writes only.
  4. Install is `cargo install` from a checkout (`bun run mcp:install`).

Card: **ft-ea6ae562** covers a gitpulse-core split, a release binary, a `collisions` exit code for
CI and pre-push, a scripted two-agent demo, and honest degradation.

## Cloud conflict warnings for teams

What exists in `gitpulse-relay` (verified from its README and `presence.go`):

- GitHub device-flow sign-in and Ed25519 device keys.
- Spaces of up to 25 members.
- Heartbeats with HMAC-SHA-256 path hashes under a space key the relay never holds. They live in
  memory for 2 minutes, capped at 16 lanes and 500 hashes.
- Cross-account overlaps.

What is missing:

- Overlap is matched by path only, which is the false-positive fatigue the ConE research reports.
- Branch names travel in clear text.
- There is no hosted deployment and no GitHub App, and billing is pending.

Order of work (card **ft-6727833f**):

1. **One user, many machines** (laptop, VPS, SSH runners). Heartbeats gain HMAC symbol ids and the
   base SHA, so verdicts survive the trip.
2. **A PR check** that comments when a PR touches symbols held by another open PR or a live agent
   branch. Start as a GitHub Action that runs the standalone CLI, then an App. GitPulse card
   ft-52bd0659 covers posting verify results to GitHub and shares the integration.
3. **Live team presence**, only after stage 2 shows demand.

### The labs branches are tested before merge or launch

This is the owner's instruction, and card **ft-07313bab** (p0) gates every labs merge and every
cloud claim. What the git audit found (verified):

- `feat/labs-cloud-unified` is 52 commits behind main and 17 ahead, across 403 files
  (+139,862/-1,258).
- `feat/labs-cloud-m2` (5 ahead) and `feat/labs-cloud-e2e` (0 ahead) are already inside it.
- Two more labs lines are **not** inside it: `labs` (worktree .claude/worktrees/labs-runners; 114
  ahead and 246 behind main), and `free-labs/snapshot-vault` (worktree .claude/worktrees/free-labs;
  1 WIP commit ahead and 179 behind).
- The `labs-cloud-unified` worktree also has 26 uncommitted files.
- The card lists the exact suites and commands: the cloud-labs Rust modules, vitest, the
  check:ipc/vendor-schema/schema-parity checks, `tests/e2e-labs-cloud` tiers 1-5 against a local
  relay, and `go test ./...` in gitpulse-relay.
- It adds a test that secret-like and gitignored paths never enter a heartbeat. The MEDIUMs on
  ft-77583041 must close first.
- None of this was run or merged on 2026-11-22: the machine was under build-slot contention.

## Other gaps and their cards

| Gap | Card |
|---|---|
| Ad-hoc-signed DMG and the `xattr` instruction; no Homebrew cask | ft-2009bb7f (updated with a Gatekeeper acceptance criterion) |
| gitpulse.vbcr.dev serves the author's portfolio HTML, so links do not unfurl as GitPulse | DevCouncil ft-07d78669 |
| Concept load (lanes, claims, governor, ledger…) and three-product setup | ft-1943c402 (onboarding follow-ups) and the wedge card |
| SSH runners and the mobile companion | ft-5b7e7fed (keep SSH; Fable advised pausing mobile until after launch) |

## Features, ranked by the second audit

1. **Install and prove the edit-time symbol warning (S).** It is already built (hooks/mod.rs:1051).
   What is missing is a one-command install of gitpulse-hook and a `--demo` that stages two
   worktrees. Risk: false positives destroy trust, so the HEAD-mismatch `FileLevel` fallback must
   stay visible.
2. **Standalone gitpulse-core plus a release binary (M).** Card ft-ea6ae562.
3. **A PR check (L as an App, M as a GitHub Action running the CLI).** No webhook or comment code
   exists yet. Build on merge_conflicts.rs.
4. **Cross-machine presence through the relay (M).** The relay side is done. The client is only on
   the labs branches, and it gives file-level overlap only.
5. **Show file claims in the session brief (S).** hooks/mod.rs:3400 already reads them.
6. **A `collisions` CLI with exit codes for CI and pre-push (S).** It reuses collision_risk. The
   unified `gp` CLI card ft-996a0d27 is a natural home.
7. **A terminal lane launcher, for Herdr parity (M).** It exists in the app only.
8. **GitLive-style indicators in the IDE (L).** Low priority.
9. **Suggest auto-resolve for DisjointSymbols pairs (M).** Risk: it implies a merge promise.
10. **Claims for non-Claude agents (M).** Extend live_sessions.rs.

## Marketing, from the second audit

- **Headline:** "Herdr runs your agents. GitPulse knows what they're changing." Pair it with a 10 s
  recording of the radar warning before the second agent's edit lands.
- **Release order:** ship the radar as a standalone install first, so the launch post needs no DMG.
  The desktop app is the upsell.
- **Cloud claims:** no launch material names team or cloud features until ft-07313bab is green.
