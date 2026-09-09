import { get } from "svelte/store";
import { invoke } from "@tauri-apps/api/core";
import { repoStore, type RepoState, type MutationOutcome } from "../stores/repoStore";
import { interfaceStore } from "../stores/interfaceStore";
import { toastStore } from "../stores/toastStore";
import { askText, askConfirm } from "../stores/modalStore";
import { actionConsequence, actionLabel, isDestructive, type OperationAction } from "../repos/operation";
import { checkForAppUpdate, describeUpdateCheck } from "../updates/updateCheck";
import { copyText } from "./clipboard";
import { openExternal } from "./openExternal";
import { revealRepository } from "./openInShell";
import { remoteWebsite } from "./remoteWebsite";
import { menuActivity, nativeMenuState } from "./menuStateStore";
import { menuActionEnabled } from "./menuState";
import { formatError } from "../ui/formatError";
import { promptQuickCommit } from "../commit/quickCommit";
import { buildMenuState } from "./menuState";
import { themeStore } from "../stores/themeStore";

type MenuRepo = Pick<typeof repoStore, "fetch" | "pull" | "push" | "stashSave" | "stashPop" | "stageAll" |
  "unstageAll" | "createBranch" | "renameBranch" | "operationAction" | "listRemotes" | "clearRecents" | "activateTab">;
export interface MenuCommandDeps {
  repo: MenuRepo;
  state: () => RepoState;
  enabled: (id: string) => boolean;
  ready: (id: string) => boolean;
  quickCommit: () => Promise<MutationOutcome>;
  askText: typeof askText;
  askConfirm: typeof askConfirm;
  copy: typeof copyText;
  reveal: typeof revealRepository;
  open: typeof openExternal;
  commitId: (path: string, ref: string) => Promise<string>;
  error: (message: string) => void;
  success: (message: string) => void;
  activity: (path: string, id: string | null) => void;
}

export function defaultMenuCommandDeps(): MenuCommandDeps {
  return {
    repo: repoStore, state: () => get(repoStore), enabled: (id) => menuActionEnabled(get(nativeMenuState), id),
    ready: (id) => menuActionEnabled(buildMenuState(get(repoStore), get(interfaceStore), themeStore.preference(), get(repoStore.mutationActivity), false), id),
    quickCommit: () => promptQuickCommit(),
    askText, askConfirm, copy: copyText, reveal: revealRepository, open: openExternal,
    commitId: async (repoPath, ref) => ref === "HEAD" ? invoke<string>("cmd_get_head_id", { repoPath }) : ref,
    error: (message) => toastStore.error(message), success: (message) => toastStore.success(message),
    activity: (path, id) => menuActivity.update((activity) => {
      const next = { ...activity };
      if (id) next[path] = [id]; else delete next[path];
      return next;
    }),
  };
}

/** Prompts capture an identity and revalidate it after every await before invoking Git. */
export function sameMenuContext(before: RepoState, after: RepoState): boolean {
  return before.currentPath === after.currentPath && before.generation === after.generation
    && before.currentBranch === after.currentBranch && before.activeTabId === after.activeTabId;
}

export function createMenuCommands(deps: MenuCommandDeps = defaultMenuCommandDeps()) {
  const pending = new Set<string>();
  function stillCurrent(state: RepoState, id?: string) {
    if (sameMenuContext(state, deps.state()) && (!id || deps.ready(id))) return true;
    deps.error("Repository changed while the action was open. Run the command again.");
    return false;
  }
  async function run(id: string, action: (state: RepoState) => Promise<void>) {
    const state = deps.state();
    const path = state.currentPath;
    if (!path || pending.has(path) || !deps.enabled(id)) return;
    pending.add(path);
    deps.activity(path, id);
    try { await action(state); }
    catch (error) { deps.error(formatError(error)); }
    finally { pending.delete(path); deps.activity(path, null); }
  }
  async function outcome(work: () => Promise<MutationOutcome>, success: string) {
    const result = await work();
    if (result.ok) deps.success(success);
    else if (result.error) deps.error(result.error);
  }
  const mutation = (id: string, work: () => Promise<MutationOutcome>, success: string) => () => run(id, () => outcome(work, success));
  const operate = (action: OperationAction) => () => run(`operation-${action}`, async (state) => {
    const operation = state.operation.operation;
    if (!operation?.available.includes(action) || state.operation.probeFailed) return;
    if (isDestructive(action) && !await deps.askConfirm({
      title: actionLabel(operation.kind, action), message: `${state.currentPath}\n\n${actionConsequence(operation.kind, action)}`,
      confirmLabel: actionLabel(operation.kind, action),
    })) return;
    if (!stillCurrent(state, `operation-${action}`)) return;
    // The native command re-detects under its repository lock as well.
    if (JSON.stringify(operation) !== JSON.stringify(deps.state().operation.operation)) {
      deps.error("The Git operation changed. Review its current state and try again.");
      return;
    }
    await outcome(() => deps.repo.operationAction(action), `${actionLabel(operation.kind, action)} completed`);
  });
  const copy = (id: string, label: string, value: (state: RepoState) => Promise<string | null>) => () => run(id, async (state) => {
    const text = await value(state);
    if (!stillCurrent(state)) return;
    if (!text) throw new Error(`${label} is unavailable.`);
    if (!await deps.copy(text)) throw new Error("Could not copy to the clipboard.");
    deps.success(`${label} copied`);
  });
  return {
    fetch: mutation("fetch", () => deps.repo.fetch(), "Fetched remote updates"),
    pull: mutation("pull", () => deps.repo.pull(), "Pulled changes from remote"),
    push: mutation("push", () => deps.repo.push(), "Pushed commits to remote"),
    stash: mutation("stash", () => deps.repo.stashSave(), "Stashed uncommitted changes"),
    stashPop: mutation("stash-pop", () => deps.repo.stashPop(), "Popped latest stash"),
    stageAll: mutation("stage-all", () => deps.repo.stageAll(), "Staged changes"),
    unstageAll: mutation("unstage-all", () => deps.repo.unstageAll(), "Unstaged changes"),
    quickCommit: mutation("quick-commit", () => deps.quickCommit(), "Committed changes"),
    createBranch: () => run("create-branch", async (state) => {
      const name = (await deps.askText({ title: "Create Branch", message: `Create a branch at HEAD in ${state.currentPath}.`,
        placeholder: "feature/name", confirmLabel: "Create" }))?.trim();
      if (!name || !stillCurrent(state, "create-branch")) return;
      await outcome(() => deps.repo.createBranch(name), `Created branch ${name}`);
    }),
    renameBranch: () => run("rename-branch", async (state) => {
      const branch = state.currentBranch;
      if (!branch) return;
      const name = (await deps.askText({ title: "Rename Current Branch", message: state.currentPath ?? undefined,
        initialValue: branch, confirmLabel: "Rename" }))?.trim();
      if (!name || name === branch || !stillCurrent(state, "rename-branch")) return;
      await outcome(() => deps.repo.renameBranch(branch, name), `Renamed branch to ${name}`);
    }),
    operationContinue: operate("continue"), operationAbort: operate("abort"), operationSkip: operate("skip"),
    copyRepoPath: copy("copy-repo-path", "Repository path", async (state) => state.currentPath),
    copyBranch: copy("copy-branch", "Branch name", async (state) => state.currentBranch),
    copyCommit: copy("copy-commit", "Commit SHA", async (state) => {
      if (!state.currentPath) return null;
      const sha = await deps.commitId(state.currentPath, state.selectedCommitId ?? "HEAD");
      if (!/^(?:[0-9a-f]{40}|[0-9a-f]{64})$/i.test(sha)) throw new Error("The commit could not be resolved to a full SHA.");
      return sha;
    }),
    revealRepo: () => run("reveal-repo", async (state) => { if (state.currentPath) await deps.reveal(state.currentPath); }),
    openRemote: () => run("open-remote", async (state) => {
      const { remotes, truncated } = await deps.repo.listRemotes();
      if (!stillCurrent(state)) return;
      const remote = remotes.find((remote) => remote.is_default) ?? remotes.find((remote) => remote.name === "origin")
        ?? (!truncated && remotes.length === 1 ? remotes[0] : undefined);
      if (!remote) throw new Error(remotes.length || truncated ? "Choose a default remote in Work → Remote, then try again." : "This repository has no remote.");
      const url = remoteWebsite(remote.fetch_url ?? remote.push_url);
      if (!url) throw new Error("The default remote has no supported HTTP or SSH website address.");
      await deps.open(url);
    }),
    clearRecents: () => deps.repo.clearRecents(),
    activateRepo: async (path: string) => {
      const tab = deps.state().openTabs.find((tab) => tab.path === path);
      try { if (tab) await deps.repo.activateTab(tab.id); }
      catch (error) { deps.error(formatError(error)); }
    },
  };
}

export async function checkUpdatesFromMenu(): Promise<void> {
  const result = await checkForAppUpdate();
  const status = describeUpdateCheck(result);
  if (status.kind === "failed") { toastStore.error(status.message); return; }
  if (result.updateAvailable) {
    interfaceStore.dismissUpdateVersion(result.latestVersion);
    toastStore.action(status.message, "View release", () => {
      void openExternal(result.releaseUrl).catch((error) => toastStore.error(formatError(error)));
    }, 12000);
  } else toastStore.success(status.message);
}
