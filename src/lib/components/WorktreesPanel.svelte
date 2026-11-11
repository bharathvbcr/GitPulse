<script lang="ts">
  import { untrack } from "svelte";
  import Checkbox from "./Checkbox.svelte";
  import Skeleton from "./Skeleton.svelte";
  import { worktreeSetupNotices, type WorktreeHooksPreview, type WorktreeInfo, type WorktreeRouteInfo } from "../branches/types";
  import { toastStore } from "../stores/toastStore";
  import type { TaskScope, TaskView } from "../tasks/types";
  import { loadWorktreeTasks } from "../tasks/bindings";
  import { loadTaskScopes } from "../tasks/scopes";
  import { invoke } from "../ipc/invoke";
  import { reportPanelError } from "../diagnostics/report";
  import { currentRepoPath, repoStore, type MutationOutcome, type MutationTarget } from "../stores/repoStore";
  import { repoScope } from "../repos/repoScope.svelte";
  import { reportFailure } from "../stores/mutationOutcome";
  import { createAsyncGuard, type AsyncGuard } from "../async/guard";
  import {
    FolderGit2,
    Plus,
    Trash2,
    ExternalLink,
    Lock,
    Unlock,
    Sparkles,
    Eraser,
    AlertTriangle,
    GitMerge,
    Globe,
    RefreshCw,
  } from "@lucide/svelte";
  import { openExternal } from "../desktop/openExternal";
  import { agentKind, agentKindLabel, agentSessionSlug, isAgentWorktree, isGitPulseLane } from "../work/agentWorktree";
  import { isCaseInsensitiveFs, sameRepo } from "../repos/paths";
  import { trustExtended } from "../repos/trustExtension";
  import { listAllLiveRuns, type TaskRun } from "../workbench/client";
  import { liveRunIn } from "../workbench/liveRun";
  import { squashesByDefault } from "../workbench/attemptBranch";
  import { MERGE_MODES } from "../branches/mergeSelection";
  import { runStateLabel } from "../workbench/taskHandoff";
  import { openTaskForRun } from "../workbench/taskOpen";


  let worktrees = $state<WorktreeInfo[]>([]);
  /**
   * DevCouncil state for this repository, and which worktree is bound to what.
   *
   * A worktree stops being a directory and becomes a task in flight once it is
   * bound: every mutation inside it is then judged against that task's planned
   * files rather than against no plan at all.
   */
  let taskView = $state<TaskView | null>(null);
  let bindings = $state<Record<string, string | null>>({});
  /**
   * Why a worktree's binding could not be read, keyed by path. A row listed
   * here is neither bound nor unbound: it says so rather than passing for
   * unbound, which is a different safety posture (no planned-files fence).
   */
  let bindingErrors = $state<Record<string, string>>({});
  /** Why the task store itself could not be read; null when it was, or was never asked. */
  let taskStateError = $state<string | null>(null);
  /** Declared scope per bound task, for the planned-file summary. */
  let scopes = $state<Record<string, TaskScope>>({});
  let isLoading = $state(false);
  let error = $state<string | null>(null);
  let isCreating = $state(false);
  let removingPath = $state<string | null>(null);
  let showAddForm = $state(false);
  let newPath = $state("");
  let newBranch = $state("");
  let startPoint = $state("");
  let cowCaches = $state(true);
  // The new worktree's DevMap index: a copy-on-write clone of the main
  // checkout's, then built for this worktree's own tree. The zero-copy pointer
  // is opt-in: only GitPulse reads it; the `devmap` agents run does not, and
  // DevMap's own hook cold-builds a full index beside it.
  let seedDevmap = $state(true);
  let zeroCopyDevmap = $state(false);
  let sparsePathsInput = $state("");
  // Repository hooks run shell commands from a file the working tree
  // controls, so they are off until asked for and, even then, run only when the
  // exact file text was approved here.
  let runHooks = $state(false);
  let hooksPreview = $state<WorktreeHooksPreview | null>(null);
  let hooksError = $state<string | null>(null);
  let hooksBusy = $state(false);

  async function readHooksPreview() {
    const repo = $repoStore.currentPath;
    // A tab switch ends this read's claim on the consent state: its finally
    // must not clear the busy flag of whatever the next repository started.
    const epoch = tabScope.epoch;
    hooksPreview = null;
    hooksError = null;
    if (!repo || !runHooks) return;
    hooksBusy = true;
    try {
      const preview = await invoke<WorktreeHooksPreview | null>("cmd_preview_worktree_hooks", { repoPath: repo });
      if ($repoStore.currentPath === repo) hooksPreview = preview;
    } catch (err: unknown) {
      if ($repoStore.currentPath === repo) hooksError = reportPanelError("worktrees", err);
    } finally {
      if (tabScope.isCurrent(epoch)) hooksBusy = false;
    }
  }

  async function approveHooks() {
    const repo = $repoStore.currentPath;
    const shown = hooksPreview;
    if (!repo || !shown) return;
    const epoch = tabScope.epoch;
    hooksBusy = true;
    hooksError = null;
    try {
      await invoke("cmd_approve_worktree_hooks", { repoPath: repo, content: shown.content });
    } catch (err: unknown) {
      const message = reportPanelError("worktrees", err);
      if (tabScope.isCurrent(epoch)) hooksError = message;
    } finally {
      if (tabScope.isCurrent(epoch)) hooksBusy = false;
    }
    if (tabScope.isCurrent(epoch)) await readHooksPreview();
  }
  let mergeConfirmPath = $state<string | null>(null);
  let mergingPath = $state<string | null>(null);
  /** Whether the armed merge lands as one commit; set when the confirm is armed, changeable until it is pressed. */
  let mergeSquash = $state(false);
  /**
   * The repository each armed confirm was raised in, captured on the first
   * click. The second click runs there or is refused: it never lands on
   * whichever repository is in front by then.
   */
  let removeTarget: MutationTarget | null = null;
  let mergeTarget: MutationTarget | null = null;
  /** Rows with a lock or unlock in flight: a second click waits for the first, whose answer decides which one it is. */
  const locking = new Set<string>();
  let mergeTimer: ReturnType<typeof setTimeout> | null = null;
  let aiSummaries = $state<Record<string, string>>({});
  let loadingSummaries = $state<Record<string, boolean>>({});
  let inflight: AsyncGuard | null = null;
  /**
   * Live task attempts, read once per load when this repository has a
   * GitPulse task worktree (the host's own attempt worktrees), so its row can say
   * which task it is and what its agent is doing — and open that task. Not a
   * poll: the panel reloads on the same triggers it always did.
   */
  let liveRuns = $state<TaskRun[]>([]);
  let confirmTimer: ReturnType<typeof setTimeout> | null = null;

  // Load trigger keyed on the real dependencies: the active repository and
  // its hydration epoch, both compared by value, so the store's other
  // publishes (every ~6s poll tick) neither cancel a load in flight nor disarm
  // the row-delete confirm. Extending trust widens what every row here can
  // report, and the offer that does it lives above the branch list rather
  // than in this panel — so the grant arrives as an announcement, and a new
  // count is a reason to reload for the same repository and generation.
  const generation = $derived($repoStore.generation);
  $effect(() => {
    void $currentRepoPath;
    void generation;
    void $trustExtended;
    untrack(() => void load());
    return () => {
      // A load that outlives its repo/generation must not apply: the next
      // effect run (or unmount) cancels it here, and load() re-checks below.
      inflight?.cancel();
      // Disarm any pending removal confirm; it must not leak across repos.
      if (confirmTimer !== null) clearTimeout(confirmTimer);
      confirmTimer = null;
      removingPath = null;
      removeTarget = null;
      mergeTarget = null;
      if (mergeTimer !== null) clearTimeout(mergeTimer);
      mergeTimer = null;
      mergeConfirmPath = null;
      mergingPath = null;
    };
  });

  /**
   * Mounted once per window, so a tab switch re-points the store and nothing
   * else. The list above is the previous repository's until `load()` answers,
   * and the effect that starts it deliberately leaves the list alone on a
   * same-repository reload (a refresh must not blank it) — so the repository
   * change itself is what clears the rows, the task bindings, summaries, the
   * add-worktree form and any error. It runs before the DOM updates, so the
   * old repository's worktrees are never painted under the new one's name.
   */
  const tabScope = repoScope(
    () => $currentRepoPath,
    (path) => {
      inflight?.cancel();
      worktrees = [];
      taskView = null;
      bindings = {};
      bindingErrors = {};
      taskStateError = null;
      scopes = {};
      liveRuns = [];
      aiSummaries = {};
      loadingSummaries = {};
      error = null;
      isLoading = path !== null;
      isCreating = false;
      showAddForm = false;
      newPath = "";
      newBranch = "";
      startPoint = "";
      cowCaches = true;
      seedDevmap = true;
      zeroCopyDevmap = false;
      sparsePathsInput = "";
      // The hooks consent belongs to the repository it was read for: a ticked
      // box and a preview of that repository's `.gitpulse/hooks` must not be
      // offered for approval under the next one.
      runHooks = false;
      hooksPreview = null;
      hooksError = null;
      hooksBusy = false;
    },
  );

  async function load() {
    const repo = $repoStore.currentPath;
    if (!repo) return;
    inflight?.cancel();
    const guard = createAsyncGuard();
    inflight = guard;
    isLoading = true;
    error = null;
    try {
      // The rows render diff stats, divergence from the default branch and dev-server
      // routes, which only the full scan measures; a listing that does not
      // ask is the cheap one.
      const next = await invoke<WorktreeInfo[]>("cmd_list_worktrees", { repoPath: repo, depth: "full" });
      if (!guard.isLive()) return;
      worktrees = next;
      await Promise.all([loadTaskState(repo, next, guard), loadLaneRuns(next, guard)]);
    } catch (err: unknown) {
      if (!guard.isLive()) return;
      error = reportPanelError("worktrees", err);
    } finally {
      if (guard.isLive()) isLoading = false;
    }
  }

  /**
   * Loads DevCouncil leases and each worktree's binding.
   *
   * Deliberately non-fatal: most repositories have no DevCouncil store, and
   * that is not an error. A failure here leaves the worktree list intact and
   * the panel's own job still works. It is not silent, though: a store or a
   * binding that could not be read says "task state unavailable" on the rows
   * it concerns, because a bound worktree that looks unbound reads as
   * unfenced.
   */
  async function loadTaskState(repo: string, list: WorktreeInfo[], guard: AsyncGuard) {
    try {
      const view = await invoke<TaskView>("cmd_task_view", { repoPath: repo });
      if (!guard.isLive()) return;
      taskView = view;
      taskStateError = null;
      const reads = await loadWorktreeTasks(invoke, repo, list.map((wt) => wt.path));
      if (!guard.isLive()) return;
      // Each read stands on its own: one failing marks its row and no other.
      const resolved: Record<string, string | null> = {};
      const unread: Record<string, string> = {};
      for (const read of reads) {
        if (read.ok) resolved[read.path] = read.taskId;
        else unread[read.path] = read.detail;
      }
      bindings = resolved;
      bindingErrors = unread;

      // The declared scope of each bound task, so a row can say how much this
      // worktree is authorised to touch. Fetched only for tasks actually bound
      // here — the store may hold hundreds.
      const wanted = Object.values(resolved).filter((t): t is string => !!t);
      const loaded = await loadTaskScopes(invoke, repo, wanted);
      if (!guard.isLive()) return;
      scopes = loaded;
    } catch (err: unknown) {
      if (!guard.isLive()) return;
      // Reported to diagnostics, not to the panel's error banner: task state is
      // an enrichment, and losing it must not read as "the worktrees failed".
      // Each row still says its task state is unavailable.
      taskStateError = reportPanelError("worktrees", err);
      taskView = null;
    }
  }

  /**
   * The live attempts GitPulse's own task worktrees belong to. Read only when
   * one is listed; a failed read is an enrichment lost, reported to
   * diagnostics, and the rows fall back to naming the worktree kind.
   */
  async function loadLaneRuns(list: WorktreeInfo[], guard: AsyncGuard) {
    if (!list.some((wt) => isGitPulseLane(agentKind(wt.path)))) {
      liveRuns = [];
      return;
    }
    try {
      const { runs } = await listAllLiveRuns();
      if (!guard.isLive()) return;
      liveRuns = runs;
    } catch (err: unknown) {
      if (!guard.isLive()) return;
      reportPanelError("worktrees", err);
      liveRuns = [];
    }
  }

  async function showLaneTask(run: TaskRun) {
    try {
      await openTaskForRun(run.id);
    } catch (err: unknown) {
      error = `That worktree's task could not be opened: ${reportPanelError("worktrees", err)}`;
    }
  }

  /** Active leases keyed by task, for the lease badge. */
  const leaseFor = $derived((taskId: string | null) =>
    taskId ? (taskView?.leases.find((l) => l.task_id === taskId) ?? null) : null,
  );

  async function bindTask(wt: WorktreeInfo, taskId: string) {
    const repo = $repoStore.currentPath;
    if (!repo) return;
    const epoch = tabScope.epoch;
    try {
      await invoke<number>("cmd_bind_worktree_task", {
        repoPath: repo,
        worktreePath: wt.path,
        taskId,
      });
      if (!tabScope.isCurrent(epoch)) return;
      bindings = { ...bindings, [wt.path]: taskId };
    } catch (err: unknown) {
      const message = reportPanelError("worktrees", err);
      if (tabScope.isCurrent(epoch)) error = message;
    }
  }

  async function unbindTask(wt: WorktreeInfo) {
    const repo = $repoStore.currentPath;
    if (!repo) return;
    const epoch = tabScope.epoch;
    try {
      await invoke<number>("cmd_unbind_worktree_task", {
        repoPath: repo,
        worktreePath: wt.path,
      });
      if (!tabScope.isCurrent(epoch)) return;
      bindings = { ...bindings, [wt.path]: null };
    } catch (err: unknown) {
      const message = reportPanelError("worktrees", err);
      if (tabScope.isCurrent(epoch)) error = message;
    }
  }

  /**
   * Says why a mutation did not run or did not finish. A refusal (the
   * repository changed) is a toast; any other failure is the panel's inline
   * error, once: the store was told the panel reports it (`handled`), so it is
   * not also an error banner.
   */
  function showFailure(outcome: MutationOutcome, what: string, epoch: number) {
    if (outcome.ok) return;
    if (outcome.refused) {
      reportFailure(outcome, what);
      return;
    }
    const message = reportPanelError("worktrees", outcome.cause ?? outcome.error);
    // The inline error belongs to the repository on screen. After a switch it
    // would be dropped with the old rows, and the store was told not to file a
    // banner, so a toast is the only place the failure can still be said.
    if (tabScope.isCurrent(epoch)) error = message;
    else toastStore.error(`${what}: ${message}`);
  }

  async function toggleLock(wt: WorktreeInfo) {
    const target = repoStore.captureTarget();
    if (!target || locking.has(wt.path)) return;
    const epoch = tabScope.epoch;
    locking.add(wt.path);
    try {
      const outcome = wt.is_locked
        ? await repoStore.unlockWorktree(wt.path, target, true)
        : await repoStore.lockWorktree(wt.path, target, true);
      if (!outcome.ok) return showFailure(outcome, wt.is_locked ? "Couldn't unlock the worktree" : "Couldn't lock the worktree", epoch);
      // `load()` reads the ACTIVE repository; after a switch that is not the
      // one this lock was for, and the new one has just loaded itself.
      if (!tabScope.isCurrent(epoch)) return;
      await load();
    } finally {
      locking.delete(wt.path);
    }
  }

  async function prune() {
    // The repository the click was made in, held across the queue.
    const target = repoStore.captureTarget();
    if (!target) return;
    const epoch = tabScope.epoch;
    const outcome = await repoStore.pruneWorktrees(target);
    if (!reportFailure(outcome, "Couldn't prune worktrees")) return;
    // `load()` reads the ACTIVE repository; after a switch that is not the
    // one this prune was for.
    if (!tabScope.isCurrent(epoch)) return;
    await load();
  }

  async function create() {
    const target = repoStore.captureTarget();
    const targetPath = newPath.trim();
    const branch = newBranch.trim();
    const base = startPoint.trim();
    const sparsePaths = sparsePathsInput
      .split(",")
      .map((s) => s.trim())
      .filter(Boolean);
    if (!target || !targetPath) return;
    const epoch = tabScope.epoch;
    isCreating = true;
    error = null;
    try {
      const outcome = await repoStore.addWorktree(
        {
          targetPath,
          newBranch: branch || null,
          startPoint: base || null,
          detach: !branch,
          cowCaches,
          runHooks,
          seedDevmap,
          zeroCopyDevmap,
          sparsePaths: sparsePaths.length > 0 ? sparsePaths : null,
        },
        target,
        true,
      );
      if (!outcome.ok) return showFailure(outcome, "Couldn't create the worktree", epoch);
      // Created, but not everything it was asked to do happened. Said now,
      // while the reader still knows which worktree they just made.
      for (const notice of worktreeSetupNotices(outcome.output ?? {}, "create")) toastStore.warning(notice);
      if (!repoStore.targetIsActive(target)) return;
      newPath = "";
      newBranch = "";
      startPoint = "";
      sparsePathsInput = "";
      showAddForm = false;
      await load();
    } finally {
      // A create started in the repository left behind has no say over the
      // new one's button; the switch already cleared the flag.
      if (tabScope.isCurrent(epoch)) isCreating = false;
    }
  }

  /** Armed-confirm label; names the discard cost when changes would be lost. */
  function removeArmTitle(wt: WorktreeInfo): string {
    if (removingPath !== wt.path) return "Remove this worktree";
    if (wt.dirty_files === null || wt.dirty_files === undefined) {
      return "Not scanned for changes — click again to try a clean remove";
    }
    return wt.dirty_files > 0
      ? `Discard ${wt.dirty_files} changed files? Click again to remove`
      : "Click again to remove";
  }

  async function remove(wt: WorktreeInfo) {
    const targetPath = wt.path;
    // Two-step confirm: the first click arms, the second removes. No native
    // dialog, so the flow stays keyboard-reachable and testable. The arming
    // timer is tracked so destroy (effect cleanup above) can clear it.
    if (removingPath !== wt.path) {
      const armed = repoStore.captureTarget();
      if (!armed) return;
      removeTarget = armed;
      removingPath = wt.path;
      if (confirmTimer !== null) clearTimeout(confirmTimer);
      confirmTimer = setTimeout(() => {
        confirmTimer = null;
        if (removingPath === wt.path) removingPath = null;
      }, 4000);
      return;
    }
    removingPath = null;
    const target = removeTarget;
    removeTarget = null;
    if (!target) return;
    const epoch = tabScope.epoch;
    // --force destroys uncommitted work. An unscanned worktree (null) is
    // not a verified-clean one: never force-remove it. A scanned dirty
    // tree is force-removed only after the armed confirm named the cost.
    const force = typeof wt.dirty_files === "number" && wt.dirty_files > 0;
    // The trust check can itself fail (the path is no longer a repository): said, not thrown away.
    let trusted: string | null;
    try {
      trusted = await repoStore.trustRepo(targetPath);
    } catch (cause) {
      return showFailure({ ok: false, cause, error: String(cause) }, "Couldn't remove the worktree", epoch);
    }
    if (!trusted) return;
    // The trust prompt can sit open while the reader goes elsewhere: the
    // removal runs only in the repository whose row was confirmed, and says so
    // when that is no longer in front.
    if (!repoStore.targetIsActive(target)) {
      toastStore.error("Couldn't remove the worktree. The repository changed while it was waiting; nothing was run.");
      return;
    }
    const outcome = await repoStore.removeWorktree(targetPath, force, target, true);
    if (!outcome.ok) return showFailure(outcome, "Couldn't remove the worktree", epoch);
    // The worktree is gone; files that could not be deleted after it are a notice, not a failure.
    for (const notice of worktreeSetupNotices(outcome.output ?? {}, "remove")) toastStore.warning(notice);
    // T-F09: a tab on the removed directory is stranded on a deleted path,
    // active or not. Only the active one used to close; a background tab
    // stayed open, held a tab slot, and was re-reported by every workspace
    // sync. Closing the active one activates a surviving neighbor.
    const stranded = $repoStore.openTabs.find((tab) => tab.path === targetPath);
    if (stranded) await repoStore.closeTab(stranded.id);
    if (!repoStore.targetIsActive(target)) return;
    await load();
  }

  function open(wt: WorktreeInfo) {
    void repoStore.openRepo(wt.path);
  }

  /**
   * Whether a tab is already open on this worktree. `openRepo` focuses that
   * tab rather than opening another, so the control has to say which of the
   * two it will do. Compared the way the tab strip compares paths.
   */
  const hasOpenTab = $derived((wt: WorktreeInfo) =>
    $repoStore.openTabs.some((tab) => sameRepo(tab.path, wt.path, { caseInsensitive: isCaseInsensitiveFs() })),
  );

  /** Nothing to list but the main checkout: the panel explains instead. */
  const onlyMain = $derived(
    !isLoading && !error && worktrees.length > 0 && worktrees.every((wt) => wt.is_main),
  );

  /** Keeps the armed merge confirm open for another few seconds; the squash choice counts as attention. */
  function holdMergeConfirm(path: string) {
    if (mergeTimer !== null) clearTimeout(mergeTimer);
    mergeTimer = setTimeout(() => {
      mergeTimer = null;
      if (mergeConfirmPath === path) mergeConfirmPath = null;
    }, 4000);
  }

  async function mergeTeardown(wt: WorktreeInfo) {
    if (mergingPath === wt.path) return;
    if (mergeConfirmPath !== wt.path) {
      const armed = repoStore.captureTarget();
      if (!armed) return;
      mergeTarget = armed;
      mergeConfirmPath = wt.path;
      mergeSquash = squashesByDefault(wt.branch);
      holdMergeConfirm(wt.path);
      return;
    }
    mergeConfirmPath = null;
    const target = mergeTarget;
    mergeTarget = null;
    // The choice the confirmed row was showing. `mergeSquash` belongs to
    // whichever row is armed, and arming another while the trust check below is
    // out would change what this row sends.
    const squash = mergeSquash;
    if (!target) return;
    const epoch = tabScope.epoch;
    mergingPath = wt.path;
    error = null;
    try {
      let trusted: string | null;
      try {
        trusted = await repoStore.trustRepo(wt.path);
      } catch (cause) {
        return showFailure({ ok: false, cause, error: String(cause) }, "Couldn't merge the worktree", epoch);
      }
      if (!trusted) return;
      // The trust prompt can sit open while the reader goes elsewhere. The
      // merge is the destructive one: it runs only in the repository whose
      // row was confirmed, and says so when that is no longer in front.
      if (!repoStore.targetIsActive(target)) {
        toastStore.error("Couldn't merge the worktree. The repository changed while it was waiting; nothing was run.");
        return;
      }
      const outcome = await repoStore.worktreeMergeTeardown(
        {
          worktreePath: wt.path,
          targetBranch: null,
          squash,
          // The commit this row showed: the host merges nothing if the branch moved since.
          expectedTip: wt.head,
        },
        target,
        true,
      );
      if (!outcome.ok) return showFailure(outcome, "Couldn't merge the worktree", epoch);
      const merged = outcome.output;
      if (!merged) {
        // The host answered without saying what it did: never read as a clean
        // merge, never silent, and the rows are reloaded to show what is there.
        showFailure({ ok: false, error: "The host did not report what the merge did; check the worktree list." }, "Couldn't confirm the merge", epoch);
        if (tabScope.isCurrent(epoch)) await load();
        return;
      }
      // The merge landed: a cleanup that did not finish is a notice below, not a failure.
      for (const notice of worktreeSetupNotices(merged, "merge")) toastStore.warning(notice);
      // The teardown removed the directory: a tab on it is stranded whether
      // or not it is the active one (T-F09, same as remove()). A worktree
      // that is still there keeps its tab.
      const stranded = merged.worktree_removed ? $repoStore.openTabs.find((tab) => tab.path === wt.path) : undefined;
      if (stranded) await repoStore.closeTab(stranded.id);
      if (!repoStore.targetIsActive(target)) return;
      await load();
    } finally {
      mergingPath = null;
    }
  }

  async function fetchAiSummary(wt: WorktreeInfo) {
    const repo = $repoStore.currentPath;
    if (!repo) return;
    const epoch = tabScope.epoch;
    loadingSummaries = { ...loadingSummaries, [wt.path]: true };
    try {
      const summary = await invoke<string>("cmd_worktree_ai_summary", {
        repoPath: repo,
        worktreePath: wt.path,
        branch: wt.branch ?? null,
      });
      if (!tabScope.isCurrent(epoch)) return;
      aiSummaries = { ...aiSummaries, [wt.path]: summary };
    } catch (err: unknown) {
      if (!tabScope.isCurrent(epoch)) return;
      error = reportPanelError("worktrees", err);
    } finally {
      if (tabScope.isCurrent(epoch)) loadingSummaries = { ...loadingSummaries, [wt.path]: false };
    }
  }

  async function syncCowCaches(wt: WorktreeInfo) {
    const target = repoStore.captureTarget();
    if (!target) return;
    const epoch = tabScope.epoch;
    const outcome = await repoStore.worktreeCowSync(wt.path, target);
    if (!reportFailure(outcome, "Couldn't sync the build cache")) return;
    if (!tabScope.isCurrent(epoch)) return;
    await load();
  }

  async function refreshRoutes(wt: WorktreeInfo) {
    try {
      const routes = await invoke<WorktreeRouteInfo[]>("cmd_worktree_routes", {
        worktreePath: wt.path,
        branch: wt.branch ?? null,
      });
      wt.active_routes = routes;
    } catch {
      // best-effort discovery
    }
  }

  async function openRoute(wt: WorktreeInfo, url: string) {
    try {
      await openExternal(url);
      void refreshRoutes(wt);
    } catch (err: unknown) {
      error = reportPanelError("worktrees", err);
    }
  }
</script>

<div>
  <div class="gp-label flex items-center justify-between px-2 mb-1">
    <span class="flex items-center gap-1.5">
      <FolderGit2 size={11} />
      <span>Worktrees ({worktrees.length})</span>
    </span>
    <div class="flex items-center gap-1">
      <button
        onclick={prune}
        title="Prune stale worktree metadata"
        aria-label="Prune stale worktree metadata"
        class="p-0.5 rounded-full hover:bg-surfaceHover hover:text-accent transition-colors"
      >
        <Eraser size={11} />
      </button>
      <button
        onclick={() => (showAddForm = !showAddForm)}
        title="Create a linked worktree for a parallel task"
        aria-label="Create worktree"
        class="p-0.5 rounded-full hover:bg-surfaceHover hover:text-accent transition-colors"
      >
        <Plus size={12} />
      </button>
    </div>
  </div>

  {#if showAddForm}
    <form
      class="gp-card mx-1 mb-1 p-2.5 space-y-1.5"
      onsubmit={(e) => {
        e.preventDefault();
        void create();
      }}
    >
      <input
        bind:value={newPath}
        placeholder="/absolute/path for the worktree"
        required
        class="gp-field w-full font-mono"
      />
      <div class="flex gap-1.5">
        <input
          bind:value={newBranch}
          placeholder="new branch (optional)"
          class="gp-field flex-1 min-w-0 font-mono"
        />
        <input
          bind:value={startPoint}
          placeholder="start point"
          class="gp-field flex-1 min-w-0 font-mono"
        />
      </div>
      <input
        bind:value={sparsePathsInput}
        placeholder="sparse paths, comma-separated (optional, e.g. src/auth)"
        aria-label="Sparse checkout paths"
        class="gp-field w-full font-mono"
      />
      <Checkbox label="Share build caches (reflink / clonefile)" checked={cowCaches} onchange={(next) => (cowCaches = next)} />
      <Checkbox label="Seed the code map index (copy-on-write clone, then built here)" checked={seedDevmap} onchange={(next) => (seedDevmap = next)} />
      <Checkbox
        label="Link to the main code map instead of copying it (GitPulse only)"
        description="Only GitPulse reads the link; the code map tool that agents run does not, and builds a full index beside it."
        checked={zeroCopyDevmap}
        onchange={(next) => (zeroCopyDevmap = next)}
      />
      <Checkbox
        label="Run repository hooks (.gitpulse/hooks)"
        checked={runHooks}
        onchange={(next) => {
          runHooks = next;
          readHooksPreview();
        }}
      />
      {#if runHooks}
        <div class="rounded-xl bg-surface p-1.5 text-micro text-textMuted" data-testid="worktree-hooks-consent">
          {#if hooksError}
            <p class="text-danger">{hooksError}</p>
          {:else if hooksBusy && !hooksPreview}
            <p>Reading the hooks file…</p>
          {:else if !hooksPreview}
            <p>This repository has no hooks file.</p>
          {:else}
            <p>
              {hooksPreview.file} runs these as shell commands in the new worktree and when merging:
            </p>
            <pre class="mt-1 max-h-32 overflow-auto whitespace-pre-wrap font-mono text-textPrimary">{hooksPreview.content}</pre>
            {#if hooksPreview.approved}
              <p class="mt-1">Approved. Editing the file asks again.</p>
            {:else}
              <p class="mt-1">Not approved: nothing runs until you approve exactly this text.</p>
              <button type="button" class="gp-btn mt-1" data-variant="primary" data-size="sm" disabled={hooksBusy} onclick={approveHooks}>
                Approve these hooks
              </button>
            {/if}
          {/if}
        </div>
      {/if}
      <div class="flex items-center justify-between">
        <span class="text-micro text-textMuted">No branch name creates a detached checkout.</span>
        <button type="submit" disabled={isCreating || !newPath.trim()} class="gp-btn" data-variant="primary" data-size="sm">
          {isCreating ? "Adding…" : "Add"}
        </button>
      </div>
    </form>
  {/if}

  {#if error}
    <div class="gp-notice mx-1 mb-1" data-tone="danger" title={error}>{error}</div>
  {/if}

  {#if isLoading && worktrees.length === 0}
    <div class="px-2 py-1"><Skeleton variant="tree-row" count={2} /></div>
  {:else}
    {#if onlyMain && !showAddForm}
      <div class="mx-1 mb-1 px-2 py-1.5 rounded-xl bg-surface/50 text-micro text-textMuted" data-testid="worktrees-empty">
        <p>No linked worktrees yet. A worktree checks out another branch in its own folder, so you can work on two things without stashing.</p>
        <button
          type="button"
          class="gp-btn mt-1"
          data-size="sm"
          onclick={() => (showAddForm = true)}
        >
          <Plus size={10} /> Create worktree
        </button>
      </div>
    {/if}
    <div class="space-y-0.5">
      {#each worktrees as wt (wt.path)}
        <div
          class="px-2 py-1 rounded-2xl flex flex-col hover:bg-surfaceHover group transition-colors
            {wt.is_main ? '' : 'ring-1 ring-inset ring-accent/25'}"
        >
         <!--
           Line one is the name's alone; line two carries everything that used
           to compete with it.

           The one-line row could not hold both. The name was the only flex
           item among shrink-0 chips, a branch capped at 90px and a
           permanently reserved action rail, so it was the only thing that
           could give — and it gave all of it. Measured at the 360px default
           width, `gitpulse-prompting-extraction-c8e628` rendered in 79px of
           the 210px it needs; at the 264px minimum it rendered in 0px, and
           the branch beside it painted straight through the change count.
           Branch rows hit this first and answered it the same way; the
           reasoning is written up in `sidebar/metrics.ts`.
         -->
         <div class="relative flex items-center gap-1.5 min-w-0">
          <button
            class="flex items-center gap-1.5 min-w-0 flex-1 text-left"
            onclick={() => open(wt)}
            title="{wt.path}\n{wt.branch ?? 'detached'} · {wt.dirty_files === null ? (wt.scan_note ?? 'not scanned') : wt.dirty_files + ' change(s)'}{wt.dirty_files !== null && wt.scan_note ? '\n' + wt.scan_note : ''}"
          >
            <span class="truncate text-meta font-medium text-textPrimary">{wt.name}</span>
            <!-- Glyphs only, never text: the two marks small enough to share
                 line one without taking back what the name just gained. -->
            <span class="ml-auto flex items-center gap-1 shrink-0">
              {#if wt.is_locked}
                <Lock size={10} class="text-textMuted" />
              {/if}
              {#if wt.is_prunable}
                <AlertTriangle size={10} class="text-warn" />
              {/if}
            </span>
          </button>
          <!-- Overlaid, not in flow. In flow, this invisible rail of six buttons
               kept ~98px of line one reserved at all times, and the name gave
               it up: the same truncation the two-line row was built to end.
               It covers the name's tail only while it is shown, and takes no
               clicks while hidden; Tab still reaches it, which is what shows
               it to a keyboard. -->
          <div class="absolute right-0 top-1/2 -translate-y-1/2 flex items-center gap-0.5 rounded-full bg-surfaceHover pl-1 opacity-0 pointer-events-none group-hover:opacity-100 group-hover:pointer-events-auto group-focus-within:opacity-100 group-focus-within:pointer-events-auto">
            <button
              onclick={() => open(wt)}
              title={hasOpenTab(wt) ? "Show tab" : "Open"}
              aria-label={hasOpenTab(wt) ? `Show the open tab for ${wt.name}` : `Open ${wt.name}`}
              class="p-0.5 rounded-full hover:bg-surfaceHover hover:text-accent"
            >
              <ExternalLink size={11} />
            </button>
            {#if !wt.is_main}
              <!-- The armed confirm is said once, inline under the row; the
                   button keeps one stable name so it is not described twice. -->
              <button
                onclick={() => void mergeTeardown(wt)}
                disabled={mergingPath === wt.path}
                title={mergingPath === wt.path ? "Merging…" : "Merge into the default branch and remove this worktree"}
                aria-label="Merge {wt.name} into the default branch and remove it"
                class="p-0.5 rounded-full {mergingPath === wt.path ? 'opacity-50 cursor-not-allowed' : mergeConfirmPath === wt.path ? 'text-warn' : 'hover:bg-surfaceHover hover:text-accent'}"
              >
                <GitMerge size={11} class={mergingPath === wt.path ? "animate-spin" : ""} />
              </button>
              <button
                onclick={() => void syncCowCaches(wt)}
                title="Sync build cache via CoW reflink"
                aria-label="Sync build cache"
                class="p-0.5 rounded-full hover:bg-surfaceHover hover:text-accent"
              >
                <RefreshCw size={10} />
              </button>
              <button
                onclick={() => void toggleLock(wt)}
                title={wt.is_locked ? "Unlock this worktree" : "Lock this worktree"}
                aria-label={wt.is_locked ? `Unlock ${wt.name}` : `Lock ${wt.name}`}
                class="p-0.5 rounded-full hover:bg-surfaceHover hover:text-accent"
              >
                {#if wt.is_locked}
                  <Unlock size={11} />
                {:else}
                  <Lock size={11} />
                {/if}
              </button>
              <button
                onclick={() => void remove(wt)}
                title={removeArmTitle(wt)}
                aria-label={removeArmTitle(wt)}
                class="p-0.5 rounded-full {removingPath === wt.path ? 'text-danger' : 'hover:text-danger'}"
              >
                <Trash2 size={11} />
              </button>
            {/if}
            <button
              onclick={() => void fetchAiSummary(wt)}
              title="Show high-density status & AI summary"
              aria-label="AI summary for {wt.name}"
              class="p-0.5 rounded-full hover:bg-surfaceHover hover:text-accent"
            >
              <Sparkles size={10} />
            </button>
          </div>
         </div>

         <!--
           Line two: the branch, the session chips, and the change count.

           `overflow-hidden` plus the right-edge fade is load-bearing rather
           than tidiness. The chips are shrink-0, so on a sidebar dragged to
           its 264px minimum they would otherwise paint through the row's edge
           and under the count — which is precisely how `claude/gitpul` came
           to sit on top of `12 changed`. The mask only bites when content
           actually reaches the last 14px, so a clipped row looks clipped
           instead of looking like a shorter branch name.
         -->
         <div class="flex items-center gap-1 min-w-0 leading-none">
          <span
            class="flex-1 min-w-0 flex items-center gap-1 overflow-hidden"
            style="mask-image: linear-gradient(to right, rgb(var(--c-text)) calc(100% - 14px), transparent); -webkit-mask-image: linear-gradient(to right, rgb(var(--c-text)) calc(100% - 14px), transparent);"
          >
            {#if isAgentWorktree(wt.path)}
              {@const kind = agentKind(wt.path)}
              {@const slug = agentSessionSlug(wt.path)}
              {@const lane = isGitPulseLane(kind) ? liveRunIn(wt.path, liveRuns) : undefined}
              {#if lane}
                <!-- A GitPulse task worktree with a live attempt: which task,
                     what its agent is doing, and the way back to it. -->
                <button
                  type="button"
                  class="gp-chip shrink-0 min-w-0 max-w-[60%] cursor-pointer hover:bg-accent/20"
                  data-tone="accent"
                  data-kind="count"
                  data-testid="worktree-task"
                  title="Open the task this worktree's agent is working on: {lane.task_title} ({runStateLabel(lane.state)})"
                  onclick={() => void showLaneTask(lane)}
                >{lane.task_title || "Task"} · {runStateLabel(lane.state)}</button>
              {:else}
                <span
                  class="gp-chip shrink-0"
                  data-tone="accent"
                  data-kind="count"
                  title="{isGitPulseLane(kind) ? 'GitPulse task worktree' : `Agent worktree (${agentKindLabel(kind)})`}{isGitPulseLane(kind) ? ' — no agent is running in it' : ''}{slug ? `: ${slug}` : ''}"
                >{agentKindLabel(kind)}</span>
              {/if}
            {/if}
            {#if wt.is_bare}
              <span class="gp-chip shrink-0" data-tone="neutral" data-kind="count">bare</span>
            {/if}
            <span
              class="truncate text-micro font-mono text-textMuted"
            >{wt.branch ?? (wt.is_detached ? wt.head.slice(0, 7) : "")}</span>
          </span>
          {#if removingPath === wt.path}
            <!--
              The armed confirm reads here, under the trash button that armed
              it, rather than inside the action cluster on line one. There it
              was a 119px sentence spliced into the same flex line as the
              name, taking that width from the only item on the line that
              could give — at the exact moment the reader needs to know which
              worktree is about to lose work.

              It does not move the button it asks to click again, and did not
              before either: the cluster is right-anchored and the trash
              button is its last child, so the button stays pinned to the
              row's edge. `harness/branches.html` measures that, because line
              two must not be what finally breaks it.
            -->
            <span class="shrink-0 text-micro font-semibold text-danger whitespace-nowrap">
              {typeof wt.dirty_files === "number" && wt.dirty_files > 0
              ? `Discard ${wt.dirty_files} changed files?`
              : wt.dirty_files === null
                ? "Not scanned — clean remove?"
                : "Remove?"}
            </span>
          {:else if (wt.dirty_files ?? 0) > 0}
            <button type="button" class="shrink-0 rounded-full px-1.5 py-0.5 text-micro text-warn hover:bg-warn/15"
              title="Preview {wt.dirty_files} uncommitted changes in {wt.path}"
              onclick={() => void repoStore.previewUncommitted(wt.path)}>{wt.dirty_files} changed</button>
          {/if}
         </div>

          <!-- Portless / Localhost routes badge -->
          {#if wt.active_routes && wt.active_routes.length > 0}
            <div class="flex items-center gap-1 flex-wrap pt-0.5 min-w-0">
              {#each wt.active_routes as route}
                <button
                  type="button"
                  onclick={() => void openRoute(wt, route.url)}
                  title="Open {route.url} in browser ({route.is_portless ? 'Portless: ' + route.hostname : 'Localhost'})"
                  class="gp-chip shrink-0 cursor-pointer font-mono hover:bg-ok/25"
                  data-tone="ok"
                  data-kind="count"
                >
                  <Globe size={9} />
                  <span>:{route.port}</span>
                  {#if route.is_portless}
                    <span class="text-micro opacity-75">portless</span>
                  {/if}
                </button>
              {/each}
            </div>
          {/if}

          <!-- High-density status board metrics: HEAD± uncommitted diff & base↕ divergence -->
          {#if wt.diff_stat || wt.main_divergence}
            <div class="flex items-center gap-2 text-micro font-mono text-textMuted pt-0.5 min-w-0">
              {#if wt.diff_stat && (wt.diff_stat.insertions > 0 || wt.diff_stat.deletions > 0)}
                <span title="HEAD± uncommitted changes: +{wt.diff_stat.insertions} -{wt.diff_stat.deletions} ({wt.diff_stat.files_changed} files)">
                  HEAD± <span class="text-ok font-semibold">+{wt.diff_stat.insertions}</span> <span class="text-danger font-semibold">-{wt.diff_stat.deletions}</span>
                </span>
              {/if}
              {#if wt.main_divergence && (wt.main_divergence.ahead > 0 || wt.main_divergence.behind > 0)}
                <span title="divergence from the default branch: {wt.main_divergence.ahead} ahead, {wt.main_divergence.behind} behind">
                  base↕ <span class="text-accent font-semibold">↑{wt.main_divergence.ahead}</span> <span class="text-warn font-semibold">↓{wt.main_divergence.behind}</span>
                </span>
              {/if}
            </div>
          {/if}

          {#if mergeConfirmPath === wt.path}
            <div class="text-micro font-semibold text-warn pt-0.5" role="status">
              Merge {wt.branch ?? wt.name} into the default branch and remove this worktree? Press merge again to confirm.
            </div>
            <Checkbox
              label="Squash into one commit"
              description={MERGE_MODES.find((mode) => mode.mode === "squash")?.detail ?? ""}
              checked={mergeSquash}
              onchange={(next) => {
                mergeSquash = next;
                holdMergeConfirm(wt.path);
              }}
            />
          {/if}

          {#if aiSummaries[wt.path]}
            <div class="text-micro font-mono text-textMuted bg-surface/80 rounded px-1.5 py-0.5 mt-0.5 truncate" title={aiSummaries[wt.path]}>
              ⚡ {aiSummaries[wt.path]}
            </div>
          {:else if loadingSummaries[wt.path]}
            <div class="text-micro text-faint italic mt-0.5">Summarizing diff…</div>
          {/if}

          <!--
            The task line. A worktree bound to a DevCouncil task has every
            mutation inside it judged against that task's planned files; an
            unbound one is judged against no plan at all, which the harness
            reports as task.absent. Showing which is which is the point: the
            two are different safety postures and used to look identical.
          -->
          {#if taskView?.available}
            {@const bound = bindings[wt.path] ?? null}
            {@const lease = leaseFor(bound)}
            <div class="flex items-center gap-1 pl-1 pb-0.5 min-w-0">
              {#if bindingErrors[wt.path]}
                <!-- Not bound and not unbound: the binding could not be read. -->
                <span class="text-micro text-warn truncate" title={bindingErrors[wt.path]}>task state unavailable</span>
              {:else if bound}
                <span
                  class="gp-chip shrink-0 font-mono" data-tone="accent" data-kind="count"
                  title="Mutations in this worktree are judged against {bound}'s planned files"
                >{bound}</span>
                {#if scopes[bound]}
                  {@const sc = scopes[bound]}
                  <span
                    class="shrink-0 text-micro text-textMuted"
                    title="Planned: {sc.planned_files.join(', ') || 'nothing'}{sc.agent_appended_files
                      .length
                      ? `\nAgent-appended: ${sc.agent_appended_files.join(', ')}`
                      : ''}{sc.forbidden_changes.length
                      ? `\nForbidden: ${sc.forbidden_changes.join(', ')}`
                      : ''}"
                  >{sc.planned_files.length} planned{sc.agent_appended_files.length
                    ? ` +${sc.agent_appended_files.length} widened`
                    : ""}</span>
                {/if}
                {#if lease}
                  <span
                    class="shrink-0 text-micro text-textMuted truncate"
                    title="Leased by {lease.owner}{lease.agent ? ` (${lease.agent})` : ''}{lease.expires_at
                      ? `, expires ${lease.expires_at}`
                      : ', no expiry'}"
                  >{lease.agent ?? lease.owner}{lease.expires_at ? "" : " · no expiry"}</span>
                {:else}
                  <!--
                    Bound but unleased is a real state, not a missing one: the
                    binding says what this checkout is for, the lease says who
                    currently holds it, and a task can be bound here while
                    nobody holds it.
                  -->
                  <span class="shrink-0 text-micro text-textMuted">not leased</span>
                {/if}
                <button
                  onclick={() => void unbindTask(wt)}
                  title="Clear this worktree's task binding"
                  class="gp-btn ml-auto shrink-0 opacity-0 group-hover:opacity-100"
                  data-variant="ghost"
                  data-size="sm"
                >unbind</button>
              {:else if taskView.leases.length > 0}
                <select
                  class="gp-select"
                  aria-label="Bind {wt.name} to a task"
                  onchange={(e) => {
                    const id = (e.currentTarget as HTMLSelectElement).value;
                    if (id) void bindTask(wt, id);
                  }}
                >
                  <option value="">no task</option>
                  {#each taskView.leases as l (l.task_id)}
                    <option value={l.task_id}>{l.task_id}</option>
                  {/each}
                </select>
              {/if}
            </div>
          {:else if taskView?.error || taskStateError}
            <!--
              A store we could not read is not a repository without one. Saying
              so is the difference between "no tasks here" and "we do not know".
              The store reports its own error; a read that threw has no store
              answer at all, which is the same "we do not know".
            -->
            <div class="pl-1 pb-0.5 text-micro text-warn truncate" title={taskView?.error ?? taskStateError ?? ""}>
              task state unavailable
            </div>
          {/if}
        </div>
      {/each}
    </div>
  {/if}
</div>
