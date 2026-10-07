# Reviewing agent output before it is merged (design spike)

Status: **design only — nothing here is built.** Written 2026-10-07 to close
the "code review" item of the Agent supervision row in
[QUALIFICATION.md](QUALIFICATION.md). Every "today" statement below was read
from source on that date; re-read it before building on one.

## What happens today

- A task attempt runs in its own linked worktree on an `agent/<attempt-id>`
  branch (`src-tauri/src/workbench/agent_worktree.rs`), or in the checkout the
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

GitPulse's guarded merge/rebase/cherry-pick of a branch named `agent/<id>`
(and of any branch whose tip an attempt recorded) asks the store for a decided
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

## What it would take

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

## Open questions

- **Which merges count.** Merge, rebase, cherry-pick and fast-forward all move
  agent commits; squash merges change the oids. The gate needs the range, not
  the commit identities, to survive a squash.
- **Agent-authored commits outside an attempt branch.** An agent in the
  person's own checkout commits straight to their branch. Reviewing that needs
  the run's start oid, which the run record does not store today.
- **Repository-level opt-in.** Some repositories will want the gate always on,
  others only for managed runs; that is a per-repository setting with a
  host-wide default, the scope model [REPOSITORY_HYGIENE.md](REPOSITORY_HYGIENE.md)
  already uses.
