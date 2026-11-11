---
name: gitpulse-collisions
description: Detect overlapping uncommitted edits across GitPulse worktrees and agent sessions. Use when two agents, worktrees, or checkouts might be touching the same files, before merging or continuing a parked rebase, or when the user asks who is editing what.
license: MIT
compatibility: Requires gitpulse-mcp on PATH and an absolute git repository path.
metadata:
  mcp-protocol: "2026-07-28"
---

# Parallel worktrees and collisions

Coding agents isolate work under `<repo>/.<agent>/worktrees/<slug>`. GitPulse labels those from directory layout (`claude`, `cursor`, `codex`, …). Git's own `.git/worktrees/` metadata is never labelled an agent session.

## Procedure

1. `gitpulse_insights` — how many worktrees, how many agent sessions, how many blocked operations. `agents.live` lists the running sessions per worktree path.
2. `gitpulse_worktree_radar` — 3-layer parallel-agent radar combining pre-edit file/symbol intent claims (`claims.json`), 1-hop DevMap CSR call-graph semantic adjacency across dirty worktrees, and read-only quarantined `git merge-tree` checks across active branches.
3. `gitpulse_collision_risk` — files dirty in more than one worktree (`items`), and files dirty in a worktree two or more live sessions share (`shared_worktrees`). Two sessions in one checkout never show in `overlapping_files`: there is only one worktree to compare.
4. Before starting edits in a parallel lane, declare intent with `gitpulse_worktree_claim` (`claimed_files`, `claimed_symbols`), check `gitpulse_worktree_inbox` for handoff or `interface_change` notices from sibling agents, broadcast signature changes via `gitpulse_worktree_send_message`, and call `gitpulse_worktree_release` when done.
5. For each overlapping path, `gitpulse_change_context` on each involved worktree before editing.

If `unscanned_worktrees` is non-zero, the overlap list is incomplete. Do not treat it as "no collision".

The overlap list needs two dirty worktrees, so from a clean worktree it is empty even when another worktree holds the file you are about to edit. Before the first edit of a file, call `gitpulse_collision_risk` with `path` (repo-relative): it returns the one row for that file, including a single other worktree holding it, plus the scan coverage counts and `file_claims` for that path. `ok: false` or `truncated: true` means a worktree was not read, and an empty `items` is then not evidence that nobody holds the file.

`file_claims` are not a collision and there is no `gitpulse_claim_files` write. A declared claim is recorded from a card's planned files when its run starts (`source: declared`, `claimed by run <id>`). An observed claim is recorded by the edit hook when it lets a write through (`source: observed`, `holder: session <id>`). A dirty file in a shared worktree with no claim is owner unknown, not a collision with every session there. `claims_error` non-empty means the claim store was not read in full (it failed, unreadable lines were skipped, or older live claims past the store cap were not kept; the next claim write repairs the store, a read never writes); an empty `file_claims` is "none" only when that field is empty. The repo-wide list keeps the newest 256 claims, one per holder and path: `claims_truncated` is true and `claims_total` is the count when it was cut. A repository GitPulse does not trust has no claim files and no claim answers. A claim never holds a wave member back; the wave note tells you a claimed file instead. `gitpulse_worktree_claim` remains the separate lane-intent bus.

If `sessions_ok` is false, some agent kind could not be observed (the process table could not be read, a session's directory was unreadable, or — on a platform with no process scan — Codex, which keeps no registry of running sessions) or the hook claims that attribute a session to its worktree could not be read in full (`sessions_error` names which, under `attribution:`), so an empty `shared_worktrees` is not proof you are alone in your checkout. Each kind in `agents.live.kinds` says whether it was observed; one with `ok: false` is unknown, not zero.

A parked merge, rebase, cherry-pick or revert (`operation_kind` on a worktree, or `operation` on change context) is blocked on a person. Do not push more edits into that worktree until it is continued or aborted in GitPulse (Work → Overview leads with the conflict editor while one is parked).
