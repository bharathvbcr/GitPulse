# Reviewing agent output before it is merged

Status: **built behind a per-repository opt-in, 2026-10-08.** Written
2026-10-07 as a design spike for the "code review" item of the Agent
supervision row in [QUALIFICATION.md](QUALIFICATION.md); the open questions it
ended on are answered in [Decisions](#decisions), and [What was built](#what-was-built)
says where each part lives. "What happens today" below is the state the spike
started from, kept because the design argues from it.

## What happens today

- A task attempt runs in its own linked worktree on a `gitpulse/<task>-<id>`
  branch (`src-tauri/src/workbench/agent_worktree.rs`; the spike said
  `agent/<attempt-id>`, the renderer's older naming), or in the checkout the
  person chose.
- When the run ends, the store records *how the process ended* —
  `exited`/`failed`/`unresolved`, an exit code, a reason — and nothing about
  what it changed. dc-store says so in its own header: run records "never
  complete or accept a task" (`vendored/dc-store/src/workbench/runs.rs`).
- The agent's commits then reach the base branch the way any branch does: the
  person merges, rebases or cherry-picks it through GitPulse's ordinary Git
  actions, each of which passes the command gate (`harness::guard_command`).
  Nothing ties that merge to the run, and nothing records that anybody looked.

So "code review for agent output" is missing as a **record**, not as a UI: the
diff views exist; what does not exist is a durable statement that *this* head
of *this* attempt's branch was reviewed and by whom, or a gate that consults it.

## The seam it belongs on

The plan already names the shape — "Durable questions, approvals, plan
reviews, change reviews and blocked states"
([AGENTIC_WORKSPACES_PLAN.md](AGENTIC_WORKSPACES_PLAN.md)) — and dc-store
already implements two of those kinds in `work_decisions`: `permission`
(decided `allow_once`/`deny`) and `question` (decided `answer`/`deny`). Each
binds a `payload_digest`, the run attempt, session and request, expires, and is
claimed and resolved exactly once. A change review is a third kind on the same
table, not a new subsystem.

| | `permission` (today) | `change_review` (proposed) |
|---|---|---|
| Raised by | the provider callback | the host, when an attempt ends with commits on its branch |
| Payload | the tool call | `{repository_id, base_oid, head_oid, branch, files_changed, diff_digest}` |
| `payload_digest` binds | the exact action | the exact `base_oid..head_oid` range |
| Decisions | `allow_once` / `deny` | `approve` / `request_changes` / `deny`, with an optional note |
| Consumed by | the provider, once | the merge gate, once per merge of that range |

## The gate

GitPulse's guarded merge or cherry-pick of a `gitpulse/<task>-<id>` attempt
branch (and of any commit such a branch contains that `HEAD` does not) asks the store for a decided
`approve` whose `head_oid` equals the tip being merged:

- **No review** — the merge is refused with a reason naming the attempt and a
  "Review changes" action. Not an allow with a warning: an unreviewed merge
  must not look like a reviewed one (the honesty invariant).
- **Reviewed, but the branch moved since** — refused as stale. A digest over
  `base_oid..head_oid` is what makes "the agent pushed one more commit after
  approval" visible.
- **`request_changes`** — refused, and the note is what a follow-up attempt's
  brief carries.
- **An explicit person override** is an ordinary second decision kind
  (`merge_unreviewed`), recorded with who and why — never a setting that turns
  the gate off silently.

The gate lives where every mutating Git action already passes —
`harness::guard_command` — as a host check before the harness is asked, so it
covers the terminal and the panels alike. It does not cover a `git merge` the
person types in an external terminal; the review record still exists, and the
Work View can show a merged range that has none.

## Automated pre-review (optional, second step)

A reviewer agent may be launched on the range before a person looks, through
the existing managed lane, with three constraints the plan already states:

- `inspect` mode only. "Bypass never silently propagates to retries, children,
  reviewers or defaults."
- Its findings attach to the `change_review` decision as advisory text; it can
  never decide one. A person decides.
- Its run is an attempt like any other, so its own crash, receipts and
  reconciliation are the machinery above.

## What it would take (the spike's plan)

1. dc-store: `change_review` (and `merge_unreviewed`) in `decisions.create`'s
   kind list and `decisions.decide`'s decision table, with the payload schema
   above. Upstream in DevCouncil first, then re-vendored — the vendored copy is
   not edited in place (see [MODULE_INTEGRATION.md](MODULE_INTEGRATION.md)).
2. Host: raise the review when an attempt ends and its branch has commits past
   the base it started from; the run record already carries the checkout and
   branch.
3. Host: the merge gate in `harness::guard_command`'s caller set, keyed on
   branch → attempt.
4. UI: "Review changes" on a finished attempt — the existing diff view over
   `base_oid..head_oid`, plus approve / request changes / deny.

## Decisions

The spike ended on three open questions. Each is settled here, with the
reason, before anything was built on it.

- **Which merges count, and squash merges.** The gate judges the *input* of a
  landing, never its output, so a squash is covered exactly like a merge: it
  reads the revisions a `git merge` (every form — fast-forward, `--no-ff`,
  `--squash`) or `git cherry-pick` names, and asks whether any of them is an
  attempt branch or a commit an attempt branch contains that `HEAD` does not.
  The commits a squash creates have new oids, but the range it consumed is
  the attempt's, and that range is what the review binds
  (`base_oid..head_oid` plus a SHA-256 over its binary diff). `--continue`,
  `--abort`, `--skip` and `--quit` land nothing new and are not judged.
  Not counted: `git rebase` (it moves the attempt branch, not the base),
  `git pull` (it lands a remote's commits), and `gh pr merge` (GitHub merges,
  and the branch protection there is the gate) — named here so their absence
  is a decision, not an oversight.
- **Agent-authored commits outside an attempt branch.** The spike said the run
  record does not store the run's start oid. It does: `runs.prepare` records
  `head_oid` and `head_ref` of the checkout (dc-store `runs.rs`), which is the
  review's `base_oid`. What the gate cannot do for a run in the person's own
  checkout is *key a merge to it*: the agent committed straight onto the
  person's branch, so no later merge moves those commits, and there is nothing
  to stop. Those runs are therefore not gated; their range
  (`head_oid..` the branch tip) is reviewable from the run's record. Running
  an agent in its own worktree is how its output becomes gateable, and the
  launch form already defaults to that whenever the checkout is busy.
- **Repository-level opt-in.** Off unless turned on, per repository, with a
  host-wide default (`review_gate` in `tools.json`: `default_on` and a
  `repositories` map keyed by the canonical common Git directory, so every
  linked worktree of one repository answers alike). It is stored where the
  Rust gate can read it — the hygiene settings live in the renderer's
  `localStorage`, which a Git action cannot consult. An unreadable setting is
  never read as "off": attempt merges are refused until it is fixed, and the
  block is written back unchanged by unrelated saves. The per-repository
  switch is on the attempt's **Merge review** panel; the host-wide default is
  set in `tools.json` only.
- **The override is not consumed.** `merge_unreviewed` is decided `allow_once`
  with a required note and binds exactly one head, as an approval does. It is
  not claimed by the gate: the store allows one decision per kind per head per
  run, so an override consumed by a merge that then failed could never be
  recorded again for that range. Re-merging the same head is idempotent, so a
  head-bound override lands that range once in effect.

## What was built

- **Store** (DevCouncil `feat/change-review-decisions`, re-vendored):
  `change_review` (`approve` / `request_changes` / `deny`, optional note) and
  `merge_unreviewed` (`allow_once` with a required note, or `deny`) are
  host-raised kinds on `work_decisions`. They are created only for a run that
  has ended (`exited`, `failed`, `cancelled`), with the run's own owner and
  session; their payload is validated at the store boundary; they last up to
  30 days rather than a live callback's 300 seconds; and their freshness does
  not depend on the run still running. Permission and question decisions are
  unchanged. The renderer cannot create either kind (`decisions.create` is
  host-only), and the provider-request list leaves both out.
- **Gate** (`src-tauri/src/harness/review.rs`): runs first in
  `harness::guard_command`, before the harness is asked, so the panels, the
  attempt's **Merge** button (`cmd_worktree_merge_teardown`) and the GitPulse
  terminal meet it alike. A refusal is GitPulse's own (`checked: false`), is
  recorded in the ledger like every gate decision, and names its rule:
  `review.unreviewed`, `review.stale` (approved, then the branch moved),
  `review.refused` (changes requested or denied, with the note) or
  `review.unavailable` (the gate could not tell, which refuses).
- **Recording** (`src-tauri/src/workbench/review.rs`, `cmd_attempt_review`,
  `cmd_attempt_review_record`, `cmd_review_gate_save`): the host creates and
  decides a review in one step on the person's action, so no undecided review
  sits waiting. A decided range cannot be decided again; a new commit is a new
  range.
- **UI** (`AttemptReview.svelte`): **Merge review** on an attempt's own
  worktree shows the range and its status, the per-repository switch, and
  approve / request changes / deny / merge without review.
- **Not built:** the automated pre-review below, and native notifications for
  a review waiting (the run's own exit notice already points at the attempt).

## Security impact

Tightening only. With the gate off — the default — nothing changes. With it
on, a guarded merge or cherry-pick that would land an agent attempt's commits
is refused unless a person approved exactly that range or recorded an
explained override for it. No path is added that permits an action that was
refused before: the gate runs ahead of the harness and can only refuse, and a
gate that cannot decide refuses. It does not reach a `git merge` typed into a
terminal GitPulse does not host.
