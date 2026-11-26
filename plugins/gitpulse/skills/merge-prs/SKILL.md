---
name: merge-prs
description: Merge a repository's open pull requests on its forge (GitHub, through the gh CLI), safely and in order. Lists them, sorts out drafts, conflicts, failing or missing checks and requested changes, reviews what has not been reviewed, shows the plan, and after the owner's yes merges each one pinned to the head commit that was checked, so nothing pushed later slips in. Head branches that are locked-in (see gitpulse:commit-and-merge) are never deleted. Use when asked to "merge the PRs", "merge the open pull requests", or "land what is green on GitHub". It never uses admin overrides, never force-pushes, and never merges without the owner's yes on the list.
license: MIT
compatibility: The gh CLI, authenticated for the repository (gh auth status), git, and gitpulse-mcp on PATH for the board steps.
metadata:
  mcp-protocol: "2026-07-28"
  agent-plugins: "1.0.0"
---

# Merge pull requests

Merging a pull request changes the remote, which other people and CI see. So this skill shows its plan and waits for the owner's yes every run. Invoking the skill does not count as that yes.

**What this skill never does:**
- `gh pr merge --admin`, or any other way past a branch protection;
- push to a pull request's branch;
- update a pull request's branch from its base, or change its base, without the owner's yes for that pull request;
- delete a head branch that is locked-in (as `gitpulse:commit-and-merge` defines it), or any branch when the owner did not ask;
- turn on auto-merge unless the owner asks.

## Before you start

1. **Check the CLI and the repository.**
   - `gh auth status` must pass.
   - Read the repository with `gh repo view --json nameWithOwner,defaultBranchRef,mergeCommitAllowed,squashMergeAllowed,rebaseMergeAllowed,deleteBranchOnMerge,viewerPermission`.
   - A `viewerPermission` below `WRITE` cannot merge. Say so and stop.
2. **Orient locally.** Call `gitpulse_insights` with the absolute `repo_path`, then `git fetch origin`.
   - Work out the locked-in list the way `gitpulse:commit-and-merge` does, including its remote expansion (`refs/remotes/origin/<value>`) and the protected names. A pull request's head is locked-in when it matches either.
3. **Watch for automatic branch deletion.** If `deleteBranchOnMerge` is true, GitHub deletes a pull request's head branch when it merges, with or without `--delete-branch`.
   - A pull request whose head is locked-in then needs the owner's explicit yes, given with that warning.
   - The other way out is for the owner to turn the setting off first.
4. **Take the integration lock** as `gitpulse:commit-and-merge` "Before you start" says, unless a calling skill (`gitpulse:wrap-up`) holds it. You need it for local verification (step 2) and for moving the local default branch (step 6).
5. **Pick the merge method.** It must be one the repository allows.
   - Prefer the one its recent history uses: `gh pr list --state merged --limit 20 --json number,mergeCommit`, plus the default branch's `git log --first-parent` (merge commits or single squashed commits).
   - If two are allowed and history does not settle it, ask.

## 1. List

Run `gh pr list --state open --limit 200 --json number,title,author,headRefName,headRefOid,baseRefName,isDraft,isCrossRepository,mergeable,mergeStateStatus,reviewDecision,labels,updatedAt`.
- If 200 come back, there may be more. Say so, rather than presenting the list as complete.
- `mergeable` can read `UNKNOWN` while GitHub computes it. Re-read that pull request after a few seconds, at most three times, and then report it as unknown.

## 2. Sort

**Read the checks.** Run `gh pr checks <n> --required --json name,state,bucket` and read the `bucket` of each row. With `--json` the command exits 0 even while checks are pending, so never judge by its exit status.
- An empty list, or an error saying there are no required checks, means **no required checks**.
- In that case, read `gh pr checks <n> --json name,state,bucket` too, and report what ran.

Then check each pull request in order, and put it in the first bucket that fits.

| Bucket | When |
|---|---|
| skip: draft | `isDraft` |
| skip: not into the default branch | `baseRefName` is neither the default branch nor another open pull request's head (a stack) |
| skip: conflicts | `mergeable` is `CONFLICTING` (`mergeStateStatus` `DIRTY`) |
| skip: changes requested | `reviewDecision` is `CHANGES_REQUESTED` |
| checks failing | a required check's bucket is `fail` or `cancel` |
| checks pending | a required check's bucket is `pending` |
| no checks | no required checks: nothing has verified it |
| behind | `mergeStateStatus` is `BEHIND`: the base protection wants it up to date |
| blocked | `mergeStateStatus` is `BLOCKED`: a required review, a required conversation resolution or another rule is unmet. Name which one; GitHub's branch protection decides, not this skill. |
| needs review | no person has approved the diff (`reviewDecision` empty or `REVIEW_REQUIRED`), and nothing above applies |
| ready | `mergeStateStatus` `CLEAN`, `HAS_HOOKS` or `UNSTABLE`, every required check passing, and the diff approved by a person or reviewed in step 3 |

**Only ready pull requests merge.** A "needs review" pull request becomes ready only if step 3 finds nothing critical, and GitHub itself does not block it.

**Stacks.** A pull request whose base is another open pull request's head merges after that one.
- Once the lower one has merged, the upper one's base must change to the default branch (`gh pr edit <n> --base <default>`). Ask before doing that.
- Never delete the lower branch while the upper pull request still targets it.

**No checks.** A pull request nothing has verified is not ready. Offer to verify it locally:
1. Fetch its head into a local ref: `git fetch origin +pull/<n>/head:refs/remotes/pr/<n>`. The `+` lets it follow a force-push.
2. Check that the fetched sha equals the pull request's `headRefOid`. If it differs, the head moved: re-read the pull request.
3. Verify it as `gitpulse:commit-and-merge` step 4 does, with this skill holding the lock. That uses the repository's full gate, or `gitpulse:merge-train` for several pull requests.

## 3. Review

**What gets reviewed:** every pull request in "needs review", and any ready one whose diff no person has approved.
- Review `gh pr diff <n>` with the strongest model the host has. In Claude Code that is an `Agent` with `model: "opus"`.
- Give the reviewer the pull request's description, `devmap_impact` on the symbols the diff changes, and the repository's instructions.
- Ask for correctness bugs, regressions, missing tests, and anything that widens access or touches secrets.

**What a review can do:**
- A model review informs the owner. It never satisfies the repository's own review requirement.
- A critical finding moves that pull request to skip. Offer to post the finding as a pull request comment, but only post it with the owner's yes.

## 4. Plan, then get a yes

Show one row per pull request:
- number and title;
- bucket;
- the head sha to pin (`headRefOid` as read now);
- the method;
- the order;
- whether its head branch is locked-in.

**Order.** Stacks go bottom first. Otherwise the oldest ready pull request goes first, then the smallest. Wait for the owner's yes, and take the plan as they edit it.

## 5. Merge, one at a time

For each pull request in the approved order:
1. **Re-read it** (`gh pr view <n> --json headRefOid,mergeable,mergeStateStatus,reviewDecision`).
   - Stop on that pull request if its head is no longer the pinned sha, or if it is no longer mergeable.
   - The earlier merges moved the base, so `BEHIND` and new conflicts show up here.
2. **Merge:** `gh pr merge <n> --<method> --match-head-commit <pinned sha>`.
   - Add `--delete-branch` only if the owner asked for it and the head branch is not locked-in. That flag deletes the local branch as well as the remote one.
   - When `deleteBranchOnMerge` is on, the remote head is deleted anyway. Only merge a locked-in head after the owner's explicit yes from "Before you start".
   - In a merge queue, gh adds the pull request to the queue instead of merging it. Report it as queued, not merged.
3. **Read the result** with `gh pr view <n> --json state,mergeCommit`. Only `MERGED` with a merge commit counts.
4. **Next one.** If the next pull request is now behind, ask before `gh pr update-branch <n>`: it pushes a commit to that branch.
5. **Stacks.** After a lower pull request merges with its head branch deleted, GitHub may close or retarget the pull request stacked on it. Re-read the upper one before going on.

## 6. Bring the local repository along

1. `git fetch origin`.
2. Fast-forward the local default branch only if it has no local-only commits (`git rev-list --count origin/<default>..<default>` is 0). Do it the way `gitpulse:commit-and-merge` step 5 lands (compare-and-swap, or `--ff-only` in its checkout). Otherwise report the divergence and leave it.
3. **Cards.** For each card on the board (`gitpulse_list_tasks`) whose branch or pull request merged:
   - check the work is on the remote default branch with `gitpulse_landed`, using `base: origin/<default>`;
   - move the card with `gitpulse_complete_task`, with the pull request number in the summary. Done is evidence-gated, so a card may land in review instead; say which.

## 7. Report

Give the owner:
- the pull requests merged, with their merge commits;
- the ones queued;
- each one skipped, with its reason;
- the review findings;
- each local step taken or skipped.

A pull request that is "merged" only because auto-merge was turned on has not merged yet.

Release the lock, unless a calling skill holds it.
