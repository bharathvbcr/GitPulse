import type { Repository, Scope } from "./client";

/**
 * Whether a board scope can hold a new task, and what a new one starts as.
 *
 * Four surfaces used to answer this independently — the header button, the
 * board's empty state, quick add, and the seed handed to the sheet — and they
 * disagreed. The empty state offered New task where the header refused it, and
 * the sheet it opened could never be saved, because nothing had linked a
 * repository. One decision, read by all four, is what stops that.
 */
export interface CreationSeed {
  repositoryIds: string[];
  primaryRepositoryId: string;
  homeWorkspaceId: string | null;
}

export interface TaskCreation {
  /** Whether the scope may open a new task at all. */
  allowed: boolean;
  /** Why it may not. Non-null exactly when `allowed` is false. */
  blocked: string | null;
  /**
   * Allowed, but the scope seeds no repository, so the sheet has to ask.
   * Never a substitute for `blocked`: a caveat means the action still works.
   */
  caveat: string | null;
  seed: CreationSeed;
}

export interface CreationContext {
  /** False until the catalog has loaded; the board is not refusing, just not ready. */
  initialized: boolean;
  repositories: readonly Pick<Repository, "id">[];
  /** Member ids of a workspace scope; `null` means not read yet, or the read failed. */
  workspaceMembers: readonly string[] | null;
  /** Name of the workspace scope, so the caveat can name it. */
  workspaceName?: string;
}

const NO_SEED: CreationSeed = { repositoryIds: [], primaryRepositoryId: "", homeWorkspaceId: null };

/** Repository the scope links a new task to before the reader chooses. */
function seedFor(scope: Scope, context: CreationContext): CreationSeed {
  const home = scope.kind === "workspace" ? scope.id : null;
  const primary = scope.kind === "repository"
    ? scope.id
    : scope.kind === "workspace"
      ? context.workspaceMembers?.[0] ?? ""
      : context.repositories[0]?.id ?? "";
  return { repositoryIds: primary ? [primary] : [], primaryRepositoryId: primary, homeWorkspaceId: home };
}

export function taskCreation(scope: Scope, context: CreationContext): TaskCreation {
  if (!context.initialized) {
    return { allowed: false, blocked: "Tasks are still loading.", caveat: null, seed: NO_SEED };
  }
  if (context.repositories.length === 0) {
    return { allowed: false, blocked: "Add a repository to create tasks.", caveat: null, seed: NO_SEED };
  }
  const seed = seedFor(scope, context);
  if (scope.kind !== "workspace" || seed.primaryRepositoryId) {
    return { allowed: true, blocked: null, caveat: null, seed };
  }
  // An empty workspace is not a reason to refuse: the profile has repositories,
  // and the sheet's first control is where one gets linked. Refusing here was
  // the dead end — a disabled button with nothing on screen saying why.
  const name = context.workspaceName?.trim();
  const caveat = context.workspaceMembers === null
    ? "This workspace's repositories could not be read. Choose one in the task."
    : `${name ? `${name} has` : "This workspace has"} no repositories yet. Choose one in the task.`;
  return { allowed: true, blocked: null, caveat, seed };
}

/**
 * Why a typed quick-add line could not be saved in this scope.
 *
 * Quick add cannot open a picker, so when the scope seeds no repository the
 * line needs the `^` marker. Naming it is the difference between a refusal and
 * a dead end.
 */
export function quickAddRefusal(creation: TaskCreation): string {
  if (creation.blocked) return creation.blocked;
  if (creation.caveat) return "Name a repository with ^name, or press Shift+Return to choose one.";
  return "Add a title before this line can be saved.";
}
