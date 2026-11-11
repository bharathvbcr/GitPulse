import type { PrAction, PullRequestDetail } from "./types";
import { shortHash } from "../format";
import { invoke } from "../ipc/invoke";
import { askConfirm } from "../stores/modalStore";
import { harnessStore, verdictLabel, type Guarded, type PolicyVerdict } from "../stores/harnessStore";
import { worktreeSetupNotices, type WorktreeCreated } from "../branches/types";

/** A pull request checked out in its own worktree. Mirrors `github::pr_worktree::PrWorktreeOpened`. */
export interface PrWorktreeOpened {
  path: string;
  branch: string;
  reused: boolean;
  head_oid: string;
  cross_repository: boolean;
  head_ref: string;
  head_owner: string;
  /** Present when this call created the worktree; null when one was reused. */
  created: WorktreeCreated | null;
  /** Why the branch was not brought up to the pull request head; null when it was. */
  update_error: string | null;
}

/**
 * The notice for a worktree that was created or reused: where it is, and
 * every step that did not happen. A branch that could not be updated is said
 * first, so a stale `pr/N` is never read as the pull request's current head.
 */
export function describePrWorktree(opened: PrWorktreeOpened): string {
  const verb = opened.reused ? "Reused" : "Opened";
  const parts = [`${verb} worktree ${opened.branch} at ${opened.path}`];
  if (opened.update_error) {
    parts.push(`${opened.branch} was not brought up to the pull request head: ${opened.update_error}`);
  }
  const created = opened.created;
  if (created) {
    parts.push(...worktreeSetupNotices(created, "create"));
    if (created.low_disk) {
      parts.push(`Build caches were not copied for want of disk: ${created.low_disk.skipped.join(", ")}`);
    }
    if (created.disk_unknown) parts.push("Free disk space could not be read, so no cache copy was limited on its account");
    if (created.devmap?.status === "failed") parts.push(`Code map setup failed: ${created.devmap.error}`);
    if (created.devmap?.status === "skipped") parts.push(`Code map setup was skipped: ${created.devmap.reason}`);
  }
  return parts.join("; ");
}

function withVerdict(message: string, policy: PolicyVerdict | null | undefined): string {
  return policy ? `${message} — ${verdictLabel(policy)}` : message;
}

/**
 * Opens the pull request in a new worktree. Records the gate verdict once.
 * Callers must not record it again.
 */
export async function openPullRequestWorktree(
  repoPath: string,
  number: number,
): Promise<Guarded<PrWorktreeOpened>> {
  const result = await invoke<Guarded<PrWorktreeOpened>>("cmd_github_open_pr_worktree", {
    repoPath,
    number,
    cowCaches: true,
    runHooks: false,
  });
  harnessStore.recordVerdict(result.policy, repoPath);
  return result;
}

/**
 * Checks the pull request out in the open checkout. Records the gate verdict
 * once. Callers must not record it again.
 */
export async function checkoutPullRequestInPlace(
  repoPath: string,
  number: number,
): Promise<Guarded<string>> {
  const result = await invoke<Guarded<string>>("cmd_github_checkout_pr", {
    repoPath,
    number,
  });
  harnessStore.recordVerdict(result.policy, repoPath);
  return result;
}

export { withVerdict };

/**
 * Confirms, sends and journals one pull-request action — the only way the
 * panel sends one. Resolves `null` when the person cancels; a refusal or a
 * gh failure rejects with gh's own message.
 */
export async function confirmAndRunPrAction(
  repoPath: string,
  action: PrAction,
  slug: string,
  context: { pr?: PullRequestDetail | null; headBranch?: string | null },
): Promise<Guarded<string> | null> {
  const confirmation = describePrAction(action, slug, context);
  const confirmed = await askConfirm(confirmation);
  if (!confirmed) return null;
  const label = action.kind === "create" ? `pr create ${action.base}` : `pr ${action.kind} #${action.number}`;
  try {
    const result = await invoke<Guarded<string>>("cmd_github_pr_action", { repoPath, action });
    harnessStore.recordVerdict(result.policy, repoPath);
    harnessStore.recordAction({ repoPath, kind: `pr-${action.kind}`, label, ok: true });
    return result;
  } catch (error) {
    harnessStore.recordAction({ repoPath, kind: `pr-${action.kind}`, label, ok: false });
    throw error;
  }
}

export interface PrActionConfirmation {
  title: string;
  message: string;
  confirmLabel: string;
  destructive: boolean;
}

const VERDICT_LABEL = {
  approve: "Approve",
  request_changes: "Request changes on",
  comment: "Comment on",
} as const;

const METHOD_LABEL = {
  merge: "a merge commit",
  squash: "one squashed commit",
  rebase: "rebased commits",
} as const;

/**
 * The confirmation every pull-request action shows before it is sent.
 *
 * Each of these is published on GitHub under the person's account and is
 * visible to everyone with access to the repository, which every message
 * says. `slug` names where it lands; `pr` is the detail the reader looked at
 * (for review and merge), so the message can name the exact head commit a
 * merge is pinned to.
 */
export function describePrAction(
  action: PrAction,
  slug: string,
  context: { pr?: PullRequestDetail | null; headBranch?: string | null },
): PrActionConfirmation {
  const published = `This is published on GitHub (${slug}) under your account.`;
  switch (action.kind) {
    case "create":
      return {
        title: "Open Pull Request",
        message: [
          `Open ${action.draft ? "a draft" : "a"} pull request from ${context.headBranch ?? "the current branch"} into ${action.base}?`,
          `Title: ${action.title.trim()}`,
          "GitPulse does not push: the branch must already be on GitHub, or gh refuses.",
          published,
        ].join("\n\n"),
        confirmLabel: "Open Pull Request",
        destructive: false,
      };
    case "review": {
      const subject = context.pr ? `#${action.number} ${context.pr.title}` : `#${action.number}`;
      return {
        title: action.verdict === "approve" ? "Approve Pull Request" : "Submit Review",
        message: [
          `${VERDICT_LABEL[action.verdict]} ${subject}?`,
          action.body.trim() ? `Review text:\n${action.body.trim()}` : "No review text.",
          published,
        ].join("\n\n"),
        confirmLabel: action.verdict === "approve" ? "Approve" : "Submit Review",
        destructive: false,
      };
    }
    case "merge": {
      const pr = context.pr;
      const lines = [
        `Merge #${action.number}${pr ? ` ${pr.title}` : ""}${pr ? ` from ${pr.head_ref} into ${pr.base_ref}` : ""} as ${METHOD_LABEL[action.method]}?`,
        `Only if its head is still ${shortHash(action.head_oid)} — a newer push makes gh refuse rather than merge commits not shown here.`,
      ];
      if (action.delete_branch) {
        lines.push(
          `The branch${pr ? ` ${pr.head_ref}` : ""} is then deleted on GitHub and locally; if you have it checked out, gh switches you to the base branch.`,
        );
      }
      lines.push(`${published} A merge cannot be undone from here.`);
      return {
        title: "Merge Pull Request",
        message: lines.join("\n\n"),
        confirmLabel: "Merge",
        destructive: true,
      };
    }
  }
}
