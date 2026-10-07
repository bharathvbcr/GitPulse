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
2. `gitpulse_collision_risk` — files dirty in more than one worktree (`items`), and files dirty in a worktree two or more live sessions share (`shared_worktrees`). Two sessions in one checkout never show in `overlapping_files`: there is only one worktree to compare.
3. For each overlapping path, `gitpulse_change_context` on each involved worktree before editing.

If `unscanned_worktrees` is non-zero, the overlap list is incomplete. Do not treat it as "no collision".

If `sessions_ok` is false, some agent kind could not be observed (Codex keeps no registry of running sessions), so an empty `shared_worktrees` is not proof you are alone in your checkout. Each kind in `agents.live.kinds` says whether it was observed; one with `ok: false` is unknown, not zero.

A parked merge, rebase, cherry-pick or revert (`operation_kind` on a worktree, or `operation` on change context) is blocked on a person. Do not push more edits into that worktree until it is continued or aborted in GitPulse's Resolve view.
