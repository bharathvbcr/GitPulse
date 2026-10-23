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

The overlap list needs two dirty worktrees, so from a clean worktree it is empty even when another worktree holds the file you are about to edit. Before the first edit of a file, call `gitpulse_collision_risk` with `path` (repo-relative): it returns the one row for that file, including a single other worktree holding it, plus the scan coverage counts. `ok: false` or `truncated: true` means a worktree was not read, and an empty `items` is then not evidence that nobody holds the file.

If `sessions_ok` is false, some agent kind could not be observed (the process table could not be read, a session's directory was unreadable, or — on a platform with no process scan — Codex, which keeps no registry of running sessions), so an empty `shared_worktrees` is not proof you are alone in your checkout. Each kind in `agents.live.kinds` says whether it was observed; one with `ok: false` is unknown, not zero.

A parked merge, rebase, cherry-pick or revert (`operation_kind` on a worktree, or `operation` on change context) is blocked on a person. Do not push more edits into that worktree until it is continued or aborted in GitPulse's Resolve view.
