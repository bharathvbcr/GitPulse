import type { PrAction, PullRequestDetail } from "./types";
import { shortHash } from "../format";
import { invoke } from "../ipc/invoke";
import { askConfirm } from "../stores/modalStore";
import { harnessStore, type Guarded } from "../stores/harnessStore";

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
