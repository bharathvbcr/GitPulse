<script lang="ts">
  import { onMount, tick, untrack } from "svelte";
  import { createAdaptiveTimer } from "../runtime/adaptiveTimer";
  import { bindForegroundChanges, readBackgroundDocument } from "../runtime/foreground";
  import { invoke } from "../ipc/invoke";
  import { isTauri } from "../platform";
  import { createListenerTracker } from "../dom/listenerTracker";
  import { openExternal as openExternalUrl } from "../desktop/openExternal";
  import { describeFailure } from "../ui/copy/describeFailure";
  import Checkbox from "./Checkbox.svelte";
  import Skeleton from "./Skeleton.svelte";
  import { reportPanelError } from "../diagnostics/report";
  import {
    AlertTriangle,
    ArrowDownToLine,
    ArrowUpFromLine,
    Bug,
    CheckCircle2,
    ExternalLink,
    GitBranch,
    ListPlus,
    LoaderCircle,
    RefreshCw,
    Rocket,
    ScanSearch,
    ShieldCheck,
    Tag,
    Trash2,
  } from "@lucide/svelte";
  import { currentRepoPath, repoStore } from "../stores/repoStore";
  import { askConfirm } from "../stores/modalStore";
  import { toastStore } from "../stores/toastStore";
  import { offerBranchUndo } from "../repos/branchUndo";
  import { harnessStore, verdictLabel } from "../stores/harnessStore";
  import { harnessPermissionMode } from "../harness/availability";
  import {
    formatReleaseDate,
    releaseTagSuggestion,
    summarizeCommitReview,
    splitBranchCleanup,
    type BranchCleanupPlan,
    type CommitReviewReport,
    type TagCleanupPlan,
    type IssueInfo,
  } from "../ops/model";
  import { registerRepository, subscribeWorkbenchChanges, type TaskCard } from "../workbench/client";
  import { createLiveLoader } from "../workbench/liveLoader";
  import {
    createTaskFromIssue,
    batchCreateTasksFromIssues,
    TASK_CATALOG_UNREAD_MESSAGE,
    findTaskForIssue,
    listAllRepositoryTasks,
  } from "../workbench/issueTask";
  import ManviHarnessPane from "./ManviHarnessPane.svelte";
  import {
    MANVI_FOCUS_TARGETS,
    MANVI_PANE_LIST,
    MANVI_PANES,
    manviFocusRequest,
    manviSectionId,
    takeManviFocus,
    type ManviFocusId,
    type ManviPane,
  } from "../ui/manviFocus";
  import type { WorkflowRunInfo, GitHubContext } from "../github/types";


  const ISSUE_REFRESH_MS = 60_000;

  /** "Couldn't <verb> <object>. <Reason>." for a notice. */
  function failure(verb: string, object: string, cause: unknown): string {
    const copy = describeFailure({ verb, object }, cause);
    return copy.detail ? `${copy.headline} ${copy.detail}` : copy.headline;
  }

  /** One Manvi surface, two panes: guarded operations, and harness/AI controls. */
  let pane = $state<ManviPane>("ops");
  let permissionMode = $derived(harnessPermissionMode($harnessStore.harness));
  let permissionLabel = $derived(
    permissionMode === "connected"
      ? "policy connected"
      : permissionMode === "unguarded"
        ? "Manvi not installed"
        : permissionMode === "blocked"
          ? "mutations blocked"
          : "checking policy",
  );
  /** Not probed yet is unknown, never a neutral that reads as "fine". */
  let permissionTone = $derived(
    permissionMode === "connected" ? "ok" : permissionMode === "unguarded" ? "warn" : permissionMode === "blocked" ? "danger" : "unknown",
  );

  let subtitle = $derived(MANVI_PANES[pane].summary);

  let cleanup = $state<BranchCleanupPlan | null>(null);
  let selectedBranches = $state<string[]>([]);
  let review = $state<CommitReviewReport | null>(null);
  let github = $state<GitHubContext | null>(null);
  let busy = $state<string | null>(null);
  /**
   * Set only while the once-a-minute poll refreshes issues in the
   * background. Unlike `busy`, it disables nothing and spins nothing: an
   * automatic refresh must not freeze every quick-action card or spin the
   * header icon once a minute.
   */
  let backgroundRefreshing = $state(false);
  /**
   * Repo whose issue load was requested while an operation held `busy`
   * (e.g. switching repos mid-op). The repo-change effect below fires once
   * per real change; without this queue the new repo's issues would not
   * load until the next interval tick.
   */
  let pendingIssueRepo: string | null = null;
  let notice = $state<string | null>(null);
  /** Background-poll failures, kept apart from action feedback. */
  let pollError = $state<string | null>(null);
  let issueTitle = $state("");
  let issueBody = $state("");
  let issueLabels = $state("bug");
  let releaseTag = $state("");
  let releaseMessage = $state("");
  let releaseConfirmed = $state(false);

  /** The task catalog as read; `null` while loading or after a failed read — never "empty". */
  let repoTasks = $state<TaskCard[] | null>(null);
  let repoTasksError = $state<string | null>(null);
  let taskAddingNumber = $state<number | null>(null);
  let batchAdding = $state(false);

  let unimportedIssues = $derived(
    github?.issues
      ? github.issues.filter((issue) => !findTaskForIssue(repoTasks, issue.number))
      : [],
  );
  // Unknown, not zero: with no catalog every issue would look new.
  let unimportedCount = $derived(repoTasks === null ? 0 : unimportedIssues.length);
  let importPaused = $derived(repoTasks === null && !!github?.issues?.length);

  let tagPlan = $state<TagCleanupPlan | null>(null);
  let selectedTags = $state<string[]>([]);

  // Same shape as cleanupInvariant: every tag must land in exactly one bucket,
  // or the split is not describing the repository and deletion is unsafe.
  let tagInvariant = $derived(
    tagPlan
      ? tagPlan.uncompared_tags + tagPlan.retained_count + tagPlan.deletable_count ===
          tagPlan.total_tags
      : true,
  );

  let cleanupInvariant = $derived(
    cleanup
      ? cleanup.protected_branches + cleanup.unmerged_branches + cleanup.candidates.length ===
          cleanup.total_local_branches
      : true,
  );

  function isSelected(name: string): boolean {
    return selectedBranches.includes(name);
  }

  function toggleSelected(name: string) {
    selectedBranches = isSelected(name)
      ? selectedBranches.filter((item) => item !== name)
      : [...selectedBranches, name];
  }

  /**
   * Opens issue/release/run URLs through the canonical OS opener. The shared
   * opener throws on failure; there is deliberately no in-webview navigation
   * fallback — it could replace the whole app shell with the target URL.
   * Failures surface on the panel notice and in the diagnostics ring.
   */
  async function openExternal(url: string) {
    try {
      await openExternalUrl(url);
    } catch (err) {
      notice = reportPanelError("ops", err);
    }
  }

  /** Releases `busy` and drains any issue load an op had deferred. */
  function settleBusy() {
    busy = null;
    const wanted = pendingIssueRepo;
    if (wanted !== null) {
      pendingIssueRepo = null;
      void loadIssues(wanted);
    }
  }

  async function loadIssues(
    repo: string = $repoStore.currentPath ?? "",
    opts: { background?: boolean } = {},
  ) {
    if (!repo) return;
    if (busy !== null) {
      // An operation owns `busy`: a user-initiated request is remembered and
      // run on settle; the background poll simply waits for its next tick.
      if (!opts.background) {
        pendingIssueRepo = repo;
      }
      return;
    }
    if (opts.background) {
      if (backgroundRefreshing) return;
      backgroundRefreshing = true;
    } else {
      busy = "issues";
    }
    try {
      const next = await invoke<GitHubContext>("cmd_github_context", { repoPath: repo });
      if ($repoStore.currentPath === repo) {
        github = next;
        if (opts.background) pollError = null;
        // The issue poll does not re-read tasks while `workbench-changed`
        // delivers them: that listener already reloads on every change, so a
        // timed re-read only repeated it once a minute for nothing.
        if (!opts.background || !tasksFollowChanges) void loadRepoTasks(repo);
      }
    } catch (error) {
      if ($repoStore.currentPath !== repo) return;
      if (opts.background) {
        // A background poll must not clobber action feedback ("Issue
        // reported…"); its failures get their own quieter slot.
        pollError = failure("refresh", "GitHub issues", error);
      } else {
        notice = failure("load", "GitHub issues", error);
      }
    } finally {
      if (opts.background) backgroundRefreshing = false;
      else settleBusy();
    }
  }

  async function scanBranches() {
    const repo = $repoStore.currentPath;
    if (!repo) return;
    busy = "branches";
    notice = null;
    try {
      const next = await invoke<BranchCleanupPlan>("cmd_branch_cleanup_plan", { repoPath: repo });
      if ($repoStore.currentPath !== repo) return;
      cleanup = next;
      selectedBranches = next.candidates.map((candidate) => candidate.name);
    } catch (error) {
      if ($repoStore.currentPath === repo) notice = failure("scan", "branches", error);
    } finally {
      settleBusy();
    }
  }

  async function scanTags() {
    const repo = $repoStore.currentPath;
    if (!repo) return;
    busy = "tags";
    notice = null;
    try {
      const next = await invoke<TagCleanupPlan>("cmd_tag_cleanup_plan", { repoPath: repo });
      if ($repoStore.currentPath !== repo) return;
      tagPlan = next;
      // Nothing pre-selected: unlike a merged branch, a tag is often kept on
      // purpose, so deleting one is a choice the user makes per row.
      selectedTags = [];
    } catch (error) {
      if ($repoStore.currentPath === repo) notice = failure("scan", "tags", error);
    } finally {
      settleBusy();
    }
  }

  function toggleTag(name: string) {
    selectedTags = selectedTags.includes(name)
      ? selectedTags.filter((item) => item !== name)
      : [...selectedTags, name];
  }

  async function cleanTags() {
    if (selectedTags.length === 0) return;
    const repo = $repoStore.currentPath;
    // The repository these tags belong to, held across the confirm and the loop.
    const target = repoStore.captureTarget();
    if (!repo || !target) return;
    const names = [...selectedTags];
    const confirmed = await askConfirm({
      title: `Delete ${names.length} tag${names.length === 1 ? "" : "s"}?`,
      message: `Delete ${names.length} tag${names.length === 1 ? "" : "s"} whose commits are already in ${tagPlan?.compared_to ?? "the base"}?\n\n${names.join("\n")}`,
      confirmLabel: "Delete",
    });
    if (!confirmed) return;
    busy = "tag-clean";
    notice = null;
    let deleted = 0;
    const failures: string[] = [];
    try {
      for (const name of names) {
        const outcome = await repoStore.deleteTag(name, target);
        if (outcome.ok) deleted += 1;
        else failures.push(`${name}: ${outcome.error ?? "no reason reported"}`);
        // The repository changed: every remaining tag would be refused too.
        if (outcome.refused) break;
      }
      notice = failures.length
        ? `Deleted ${deleted} of ${names.length}. ${failures.join("; ")}`
        : `Deleted ${deleted} tag${deleted === 1 ? "" : "s"}.`;
      selectedTags = [];
      if ($repoStore.currentPath === repo) await scanTags();
    } finally {
      settleBusy();
    }
  }

  async function cleanBranches() {
    if (selectedBranches.length === 0) return;
    const repo = $repoStore.currentPath;
    // The repository these branches belong to, held across the confirm, the
    // loop and the Undo toast.
    const target = repoStore.captureTarget();
    if (!repo || !target || !cleanup) return;
    const selected = cleanup.candidates.filter((c) => selectedBranches.includes(c.name));
    if (selected.length === 0) return;
    const names = selected.map((c) => c.name);
    const confirmed = await askConfirm({
      title: `Delete ${names.length} merged branch${names.length === 1 ? "" : "es"}?`,
      message: `Delete ${names.length} merged local branch${names.length === 1 ? "" : "es"}?\n\n${names.join("\n")}`,
      confirmLabel: "Delete",
    });
    if (!confirmed) {
      return;
    }
    busy = "clean";
    notice = null;
    let deleted = 0;
    const failures: string[] = [];
    // Tips captured from the plan before delete — same restore input the
    // sidebar toast uses. The durable ledger before_ref survives toast death.
    const restoredTips: { name: string; tip: string }[] = [];
    try {
      const { ancestry, byTree } = splitBranchCleanup(selected, selectedBranches);
      let repositoryRefused = false;
      for (const candidate of ancestry) {
        const tip = candidate.tip_commit_id;
        const outcome = await repoStore.deleteBranch(candidate.name, false, target);
        if (outcome.ok) {
          deleted += 1;
          if (tip) restoredTips.push({ name: candidate.name, tip });
        } else {
          failures.push(`${candidate.name}: ${outcome.error ?? "failed"}`);
        }
        // The repository changed: every remaining branch would be refused too.
        if (outcome.refused) {
          repositoryRefused = true;
          break;
        }
      }
      // Squash- and rebase-merged branches: `git branch -d` refuses them, so
      // the gated dead-branch clean deletes them. It re-scans, backs up, and
      // forces only a branch it re-proves merged by tree.
      if (byTree.length > 0 && !repositoryRefused) {
        const outcome = await repoStore.deadbranchClean(
          byTree.map((c) => c.name),
          false,
          true,
          target,
        );
        if (outcome.ok && outcome.output) {
          const gone = new Set(outcome.output.deleted);
          for (const candidate of byTree) {
            if (gone.has(candidate.name)) {
              deleted += 1;
              if (candidate.tip_commit_id) {
                restoredTips.push({ name: candidate.name, tip: candidate.tip_commit_id });
              }
            }
          }
          for (const [name, reason] of outcome.output.failed) failures.push(`${name}: ${reason}`);
          // A branch the clean neither deleted nor reported was not re-proven merged.
          const failedNames = new Set(outcome.output.failed.map(([name]) => name));
          for (const candidate of byTree) {
            if (!gone.has(candidate.name) && !failedNames.has(candidate.name)) {
              failures.push(`${candidate.name}: not deleted (no longer provably merged)`);
            }
          }
        } else {
          failures.push(`${byTree.map((c) => c.name).join(", ")}: ${outcome.error ?? "failed"}`);
        }
      }
      // The Undo is bound to the repository the delete ran in, so it is offered
      // even when another tab is active now. The panel's own notice and rescan
      // describe the active repository, and only apply while it is still this one.
      if (restoredTips.length > 0) {
        offerBranchUndo(
          restoredTips.length === 1
            ? `Deleted branch "${restoredTips[0].name}"`
            : `Deleted ${restoredTips.length} merged branches`,
          restoredTips,
          target,
        );
      }
      if ($repoStore.currentPath === repo) {
        notice = failures.length
          ? `Deleted ${deleted} of ${names.length}. ${failures.join(" ")}`
          : `Deleted ${deleted} merged branch${deleted === 1 ? "" : "es"}.`;
        await scanBranches();
      }
    } finally {
      settleBusy();
    }
  }

  async function reviewCommits() {
    const repo = $repoStore.currentPath;
    if (!repo) return;
    busy = "review";
    notice = null;
    try {
      const next = await invoke<CommitReviewReport>("cmd_review_outgoing_commits", { repoPath: repo });
      if ($repoStore.currentPath === repo) review = next;
    } catch (error) {
      if ($repoStore.currentPath === repo) notice = failure("review", "outgoing commits", error);
    } finally {
      settleBusy();
    }
  }

  async function sync(kind: "pull" | "push") {
    busy = kind;
    notice = null;
    try {
      const outcome = kind === "pull" ? await repoStore.pull() : await repoStore.push();
      notice = outcome.ok
        ? `${kind === "pull" ? "Pull" : "Push"} completed${outcome.policy ? ` — ${verdictLabel(outcome.policy)}` : ""}.`
        : outcome.error ?? `Couldn't ${kind} the branch.`;
    } catch (error) {
      // runMutating resolves invoke failures itself; this keeps an unexpected
      // throw (verdict rendering, store internals) visible in the panel
      // instead of console-only, matching scanBranches/reportIssue.
      notice = failure(kind, "the branch", error);
    } finally {
      settleBusy();
    }
  }

  async function reportIssue() {
    const title = issueTitle.trim();
    if (!title) return;
    busy = "report";
    notice = null;
    try {
      const labels = issueLabels.split(",").map((label) => label.trim()).filter(Boolean);
      const outcome = await repoStore.reportIssue(title, issueBody, labels);
      if (!outcome.ok) {
        notice = outcome.error ?? "Couldn't report the issue.";
        return;
      }
      notice = `Issue reported${outcome.policy ? ` — ${verdictLabel(outcome.policy)}` : ""}.`;
      issueTitle = "";
      issueBody = "";
      try {
        if (outcome.output) await openExternal(outcome.output);
      } catch {
        // The issue exists even if opening it fails; the URL is in the journal.
      }
      await loadIssues();
    } catch (error) {
      notice = failure("report", "the issue", error);
    } finally {
      settleBusy();
    }
  }

  async function publishRelease() {
    if (!releaseConfirmed || !releaseTag.trim()) return;
    busy = "release";
    notice = null;
    try {
      const outcome = await repoStore.publishRelease(
        releaseTag.trim(),
        releaseMessage.trim() || `Release ${releaseTag.trim()}`,
      );
      if (!outcome.ok) {
        notice = outcome.error ?? "Couldn't publish the release.";
        return;
      }
      notice = `Pushed ${outcome.output?.tag ?? releaseTag} to ${outcome.output?.remote ?? "the remote"}; the release workflow can now build the app.`;
      releaseConfirmed = false;
      await loadIssues();
    } catch (error) {
      notice = failure("publish", "the release", error);
    } finally {
      settleBusy();
    }
  }

  function runState(run: WorkflowRunInfo): string {
    return run.conclusion || run.status || "unknown";
  }

  function openTasksView() {
    repoStore.setViewSection("work", "tasks");
  }

  let repoTasksGeneration = 0;
  /** True once the `workbench-changed` listener is attached; tasks then reload on change, not on a timer. */
  let tasksFollowChanges = false;
  /**
   * Change-driven task reloads through the shared live loader: one at a time,
   * spaced (a store written every second no longer means a reload every
   * second), never while the window is behind. The timer this replaces was
   * restarted by each change, so changes closer together than its delay put
   * the reload off for as long as they kept coming.
   */
  const repoTaskLoader = createLiveLoader({
    canRun: () => !readBackgroundDocument(),
    read: async () => {
      const repo = $repoStore.currentPath;
      if (repo) await loadRepoTasks(repo);
    },
    onError: (cause) => { repoTasksError = failure("read", "the task list", cause); },
  });
  function scheduleRefreshRepoTasks() { repoTaskLoader.schedule(); }

  async function loadRepoTasks(repoPath: string = $repoStore.currentPath ?? "") {
    if (!repoPath) {
      repoTasks = null;
      repoTasksError = null;
      return;
    }
    const currentGen = ++repoTasksGeneration;
    try {
      const registered = await registerRepository(repoPath);
      if ($repoStore.currentPath !== repoPath || !registered?.id || currentGen !== repoTasksGeneration) return;
      const tasks = await listAllRepositoryTasks(registered.id);
      if ($repoStore.currentPath === repoPath && currentGen === repoTasksGeneration) {
        repoTasks = tasks;
        repoTasksError = null;
      }
    } catch (err) {
      // Unknown, not empty: importing against a catalog that was not read
      // would re-create every task that already exists.
      if ($repoStore.currentPath === repoPath && currentGen === repoTasksGeneration) {
        repoTasks = null;
        repoTasksError = failure("read", "the task list", err);
      }
    }
  }

  async function addIssueToTask(issue: IssueInfo) {
    const repoPath = $repoStore.currentPath;
    if (!repoPath) {
      notice = "No active repository found.";
      return;
    }
    if (taskAddingNumber !== null || batchAdding || busy !== null) return;
    if (repoTasks === null) {
      notice = TASK_CATALOG_UNREAD_MESSAGE;
      return;
    }
    taskAddingNumber = issue.number;
    notice = null;
    try {
      const saved = await createTaskFromIssue(issue, repoPath);
      if ($repoStore.currentPath === repoPath) {
        repoTasks = [saved, ...(repoTasks ?? []).filter((t) => t.id !== saved.id)];
        toastStore.success(`Created task for issue #${issue.number}`);
        notice = `Task [#${issue.number}] created. Available on the Tasks board for agent handoff.`;
      }
    } catch (err) {
      if ($repoStore.currentPath === repoPath) {
        notice = failure("create", "the task", err);
      }
    } finally {
      taskAddingNumber = null;
    }
  }

  async function addAllIssuesToTasks() {
    const repoPath = $repoStore.currentPath;
    if (!repoPath || !github?.issues?.length) return;
    if (batchAdding || taskAddingNumber !== null || busy !== null) return;
    if (repoTasks === null) {
      notice = TASK_CATALOG_UNREAD_MESSAGE;
      return;
    }

    if (unimportedIssues.length === 0) {
      notice = "All open issues are already in tasks.";
      return;
    }

    const count = unimportedIssues.length;
    const confirmed = await askConfirm({
      title: "Add all issues to tasks?",
      message: `Create ${count} task${count === 1 ? "" : "s"} from open GitHub issues for coding agents to work on?`,
      confirmLabel: "Add tasks",
    });
    if (!confirmed || $repoStore.currentPath !== repoPath) return;

    batchAdding = true;
    notice = null;
    try {
      const result = await batchCreateTasksFromIssues(unimportedIssues, repoPath, repoTasks);
      if ($repoStore.currentPath === repoPath) {
        repoTasks = [...result.created, ...(repoTasks ?? [])];
        if (result.created.length > 0) {
          toastStore.success(
            `Created ${result.created.length} task${result.created.length === 1 ? "" : "s"} from GitHub issues`,
          );
          notice = `Added ${result.created.length} task${result.created.length === 1 ? "" : "s"} to Tasks board for agent handoff.`;
        }
        if (result.errors.length > 0) {
          notice = `Added ${result.created.length} tasks. Couldn't add ${result.errors.length}. ${result.errors[0].error}`;
        }
      }
    } catch (err) {
      if ($repoStore.currentPath === repoPath) {
        notice = failure("add", "issues to tasks", err);
      }
    } finally {
      batchAdding = false;
    }
  }

  /**
   * A deep link from elsewhere in the app (the header chips, the storage
   * panel) names a section, not just this view. Switching the pane is only
   * half the answer: the section still has to be brought on screen and marked,
   * or the reader lands mid-page with no idea which card answered their click.
   */
  let flashTimer: number | null = null;
  let flashed: HTMLElement | null = null;
  let realignTimers: number[] = [];

  function clearFlash() {
    if (flashTimer !== null) {
      window.clearTimeout(flashTimer);
      flashTimer = null;
    }
    // The marked section lives in this panel or in ManviHarnessPane, so the
    // class is applied to the node rather than bound in one component's markup.
    flashed?.classList.remove("gp-focus-flash");
    flashed = null;
  }

  function clearRealign() {
    for (const timer of realignTimers) window.clearTimeout(timer);
    realignTimers = [];
  }

  /**
   * Cards above the target keep settling for a moment after the view mounts —
   * a status probe resolves, the grant list arrives — and every height change
   * slides the section the reader was sent to, typically far enough to push its
   * heading off the top. Two bounded re-alignments cover the settle; a scroll
   * gesture cancels them, because yanking a reader back is worse than drift.
   */
  const REALIGN_DELAYS_MS = [0, 250];

  function keepAligned(section: HTMLElement) {
    clearRealign();
    for (const delay of REALIGN_DELAYS_MS) {
      realignTimers.push(
        window.setTimeout(() => section.scrollIntoView({ block: "start" }), delay),
      );
    }
    for (const event of ["wheel", "touchstart", "keydown"] as const) {
      window.addEventListener(event, clearRealign, { once: true, passive: true });
    }
  }

  async function revealFocus(id: ManviFocusId) {
    const target = MANVI_FOCUS_TARGETS[id];
    pane = target.pane;
    await tick();
    const section = document.getElementById(manviSectionId(id));
    if (!section) return;
    // Instant, like every other scroll-into-view in the app: a deep link should
    // land deterministically, and a smooth 2000px glide is both a lot of motion
    // and, in a webview that is not compositing, no scroll at all.
    section.scrollIntoView({ block: "start" });
    keepAligned(section);
    // Sections carry tabindex="-1": keyboard and screen-reader users land on
    // the same card the scroll brought into view, not back at the page top.
    section.focus({ preventScroll: true });
    clearFlash();
    section.classList.add("gp-focus-flash");
    flashed = section;
    flashTimer = window.setTimeout(() => clearFlash(), 1600);
  }

  // Runs on mount (the view mounts lazily, after the click) and again whenever
  // a request arrives while the view is already open.
  $effect(() => {
    const requested = $manviFocusRequest;
    if (!requested) return;
    takeManviFocus();
    void revealFocus(requested);
  });

  $effect(() => {
    return () => {
      clearFlash();
      clearRealign();
      for (const event of ["wheel", "touchstart", "keydown"] as const) {
        window.removeEventListener(event, clearRealign);
      }
    };
  });

  onMount(() => {
    // Each tick costs four parallel gh round trips, so it runs only while the
    // window is on screen AND this pane is the one showing. The visibility
    // half belongs to the timer now — it used to keep firing behind other
    // windows and merely decline the work, which still woke the renderer.
    const stopInterval = createAdaptiveTimer(() => {
      if (pane === "ops" && $repoStore.currentPath) {
        void loadIssues(undefined, { background: true });
      }
    }, ISSUE_REFRESH_MS);

    const listeners = createListenerTracker();
    if (isTauri()) {
      // Following changes is a promise to stop polling tasks, so it is made
      // only while the subscription stands; a failed one says so and the
      // issue poll keeps the tasks fresh.
      tasksFollowChanges = true;
      listeners.track(subscribeWorkbenchChanges({
        changed: scheduleRefreshRepoTasks,
        unavailable: (message) => { tasksFollowChanges = false; notice = `Live updates unavailable: ${message}. Tasks refresh with the issue poll.`; },
      }));
    }
    const onForeground = () => {
      if (readBackgroundDocument()) {
        repoTaskLoader.reset();
        return;
      }
      scheduleRefreshRepoTasks();
    };
    listeners.track(bindForegroundChanges(document, typeof window === "undefined" ? null : window, onForeground));

    return () => {
      stopInterval();
      tasksFollowChanges = false;
      listeners.dispose();
      repoTaskLoader.dispose();
    };
  });

  // Keyed on the repository alone: repoStore republishes fresh objects on
  // every status poll (~6s), and re-running the resets per emission would
  // thrash panel state. A load skipped because an op holds `busy` is queued
  // in pendingIssueRepo and drained by settleBusy().
  const repoTags = $derived($repoStore.tags);
  $effect(() => {
    const repo = $currentRepoPath;
    untrack(() => resetForRepository(repo));
  });
  function resetForRepository(repo: string | null) {
    cleanup = null;
    selectedBranches = [];
    review = null;
    github = null;
    repoTasks = null;
    repoTasksError = null;
    notice = null;
    pollError = null;
    const tag = releaseTagSuggestion(repoTags.map((tag) => tag.name));
    releaseTag = tag;
    releaseMessage = `Release ${tag}`;
    releaseConfirmed = false;
    pendingIssueRepo = null; // a queued request for an older repo is obsolete
    if (repo) {
      void loadIssues(repo);
      void loadRepoTasks(repo);
    }
  }
</script>

<div class="flex-1 overflow-auto bg-background p-4 text-xs text-textPrimary">
  <div class="flex w-full flex-col gap-4">
    <div class="flex items-start justify-between gap-4">
      <div>
        <div class="flex items-center gap-2">
          <ShieldCheck size={19} class="text-accent" />
          <h2 class="text-base font-semibold">Manvi</h2>
          <span class="gp-chip" data-tone={permissionTone} data-kind="label">{permissionLabel}</span>
        </div>
        <p class="mt-1 text-textMuted">{subtitle}</p>
      </div>
      <div class="flex items-center gap-2 shrink-0">
        {#if pane === "ops"}
          <button class="gp-btn" data-size="sm" onclick={() => { void scanBranches(); void reviewCommits(); void loadIssues(); }} disabled={busy !== null}>
            <RefreshCw size={13} class={busy ? "animate-spin" : ""} /> Refresh all
          </button>
        {/if}
        <div class="gp-segmented" role="group" aria-label="Manvi view">
          {#each MANVI_PANE_LIST as entry (entry.id)}
            <button type="button" aria-pressed={pane === entry.id} data-active={pane === entry.id ? "true" : "false"} class="gp-seg-btn text-meta!" onclick={() => (pane = entry.id)}>{entry.label}</button>
          {/each}
        </div>
      </div>
    </div>

    {#if notice}
      <div class="gp-notice" data-tone="info" role="status">
        <span class="min-w-0 flex-1">{notice}</span>
        {#if notice.includes("Tasks board")}
          <button type="button" class="gp-btn shrink-0" data-size="sm" onclick={openTasksView}>
            Open Tasks
          </button>
        {/if}
      </div>
    {/if}

    {#if pollError}
      <div class="gp-notice" data-tone="warn" role="status">{pollError}</div>
    {/if}

    {#if pane === "harness"}
      <ManviHarnessPane />
    {:else}
    <section class="grid gap-3 md:grid-cols-2 lg:grid-cols-4">
      <button class="gp-card flex items-center gap-3 p-4 text-left hover:bg-surfaceHover disabled:opacity-40" onclick={() => sync("pull")} disabled={busy !== null}>
        <ArrowDownToLine size={20} class="text-accent" />
        <span><strong class="block">Quick pull</strong><span class="text-textMuted">Update the working branch through Manvi.</span></span>
      </button>
      <button class="gp-card flex items-center gap-3 p-4 text-left hover:bg-surfaceHover disabled:opacity-40" onclick={() => sync("push")} disabled={busy !== null}>
        <ArrowUpFromLine size={20} class="text-accent" />
        <span><strong class="block">Quick push</strong><span class="text-textMuted">Publish the current branch safely.</span></span>
      </button>
      <button class="gp-card flex items-center gap-3 p-4 text-left hover:bg-surfaceHover disabled:opacity-40" onclick={scanBranches} disabled={busy !== null}>
        <GitBranch size={20} class="text-accent" />
        <span><strong class="block">Scan branches</strong><span class="text-textMuted">Find merged local branches only.</span></span>
      </button>
      <button class="gp-card flex items-center gap-3 p-4 text-left hover:bg-surfaceHover disabled:opacity-40" onclick={reviewCommits} disabled={busy !== null}>
        <ScanSearch size={20} class="text-accent" />
        <span><strong class="block">Review messages</strong><span class="text-textMuted">Audit commits that are about to ship.</span></span>
      </button>
    </section>

    <div class="grid gap-4 xl:grid-cols-2">
      <section id={manviSectionId("cleanup")} tabindex="-1" class="gp-card p-4">
        <div class="mb-3 flex items-center justify-between">
          <div>
            <h3 class="font-semibold">Branch cleanup</h3>
            <p class="text-textMuted">Dry-run first; current, default, worktree, and unmerged branches stay protected.</p>
          </div>
          {#if cleanup}<span class="gp-chip" data-tone="neutral" data-kind="label">{cleanup.candidates.length} eligible / {cleanup.total_local_branches} local</span>{/if}
        </div>
        {#if busy === "branches" || busy === "clean"}
          <div class="py-3" aria-label="Inspecting Git refs"><Skeleton variant="text" count={3} /></div>
        {:else if cleanup}
          {#if !cleanupInvariant}
            <div class="gp-notice mb-2" data-tone="danger" role="alert"><AlertTriangle size={14} class="shrink-0" style="color: var(--gp-notice-ink)" /> Cleanup coverage is inconsistent; deletion is disabled.</div>
          {/if}
          <div class="max-h-52 space-y-1 overflow-auto">
            {#each cleanup.candidates as branch, i (`${branch.name}#${i}`)}
              <div class="gp-row">
                <Checkbox checked={isSelected(branch.name)} ariaLabel="Select {branch.name}" onchange={() => toggleSelected(branch.name)} />
                <span class="min-w-0 flex-1">
                  <span class="font-mono">{branch.name}</span>
                  <span class="ml-2 font-mono text-textMuted" title={branch.tip_commit_id}>{branch.tip_commit_id.slice(0, 7)}</span>
                  <span class="ml-2 truncate text-textMuted">{branch.last_summary}</span>
                </span>
                {#if branch.upstream_gone}<span class="gp-chip" data-tone="warn" data-kind="label">upstream gone</span>{/if}
              </div>
            {:else}
              <div class="flex items-center gap-2 py-5 text-textMuted"><CheckCircle2 size={15} class="text-ok" /> No merged local branches need cleanup.</div>
            {/each}
          </div>
          <div class="gp-separator" aria-hidden="true"></div>
          <div class="mt-3 flex items-center justify-between pt-3 text-textMuted">
            <span>{cleanup.protected_branches} protected · {cleanup.unmerged_branches} unmerged</span>
            {#if cleanup.unchecked_branches > 0}
              <span class="text-warn" title={cleanup.unchecked_reason ?? ""}>
                {cleanup.unchecked_branches} not checked for squash merges{cleanup.unchecked_reason ? `: ${cleanup.unchecked_reason}` : ""}
              </span>
            {/if}
            <button class="gp-btn" data-variant="danger" data-size="sm" onclick={cleanBranches} disabled={!cleanupInvariant || selectedBranches.length === 0 || busy !== null}>
              <Trash2 size={13} /> Delete {selectedBranches.length} selected
            </button>
          </div>
        {:else}
          <button class="gp-btn" onclick={scanBranches}><GitBranch size={13} /> Build cleanup plan</button>
        {/if}
      </section>

      <section class="gp-card p-4">
        <div class="mb-3 flex items-center justify-between">
          <div>
            <h3 class="font-semibold">Tag retention</h3>
            <p class="text-textMuted">A commit held by a tag alone is on no branch, so branch cleanup counts it nowhere.</p>
          </div>
          {#if tagPlan}<span class="gp-chip" data-tone="neutral" data-kind="label">{tagPlan.retained_count} holding work / {tagPlan.total_tags} tags</span>{/if}
        </div>
        {#if busy === "tags" || busy === "tag-clean"}
          <div class="py-3" aria-label="Measuring tags against the base"><Skeleton variant="text" count={3} /></div>
        {:else if tagPlan}
          {#if !tagInvariant}
            <div class="gp-notice mb-2" data-tone="danger" role="alert"><AlertTriangle size={14} class="shrink-0" style="color: var(--gp-notice-ink)" /> Tag coverage is inconsistent; deletion is disabled.</div>
          {/if}
          {#if tagPlan.truncated}
            <div class="gp-notice mb-2" data-tone="warn" role="status">The tag listing was capped, so these counts describe a sample of this repository's tags, not all of them.</div>
          {/if}
          {#if tagPlan.total_tags === 0}
            <div class="flex items-center gap-2 py-5 text-textMuted"><CheckCircle2 size={15} class="text-ok" /> This repository has no tags.</div>
          {:else}
            {#if tagPlan.retained_count > 0}
              <p class="mb-1 text-textSecondary">Holding work {tagPlan.compared_to ?? "the base"} does not have — kept, not deletable.</p>
              <div class="max-h-40 space-y-1 overflow-auto">
                {#each tagPlan.retained as tag, i (`${tag.name}#${i}`)}
                  <div class="flex items-center gap-2 rounded-lg px-2 py-1.5">
                    <Tag size={13} class="shrink-0 text-warn" />
                    <span class="min-w-0 flex-1 truncate font-mono">{tag.name}</span>
                    <span class="gp-chip" data-tone="warn" data-kind="count" title="{tag.commits_ahead_of_base} commits not in {tagPlan.compared_to ?? 'the base'}">+{tag.commits_ahead_of_base}</span>
                  </div>
                {/each}
              </div>
              {#if tagPlan.retained.length < tagPlan.retained_count}
                <div class="mt-1 text-warn">Showing {tagPlan.retained.length} of {tagPlan.retained_count} tags holding work.</div>
              {/if}
            {/if}
            <p class="mt-3 mb-1 text-textSecondary">Every commit already in {tagPlan.compared_to ?? "the base"} — safe to delete.</p>
            <div class="max-h-40 space-y-1 overflow-auto">
              {#each tagPlan.candidates as tag, i (`${tag.name}#${i}`)}
                <div class="gp-row">
                  <Checkbox checked={selectedTags.includes(tag.name)} ariaLabel="Select {tag.name}" onchange={() => toggleTag(tag.name)} />
                  <span class="min-w-0 flex-1 truncate"><span class="font-mono">{tag.name}</span><span class="ml-2 text-textMuted">{tag.summary}</span></span>
                </div>
              {:else}
                <div class="flex items-center gap-2 py-4 text-textMuted"><CheckCircle2 size={15} class="text-ok" /> No tags are redundant with {tagPlan.compared_to ?? "the base"}.</div>
              {/each}
            </div>
            {#if tagPlan.candidates.length < tagPlan.deletable_count}
              <div class="mt-1 text-warn">Showing {tagPlan.candidates.length} of {tagPlan.deletable_count} deletable tags.</div>
            {/if}
          {/if}
          <div class="gp-separator" aria-hidden="true"></div>
          <div class="mt-3 flex items-center justify-between pt-3 text-textMuted">
            <span>
              {#if tagPlan.uncompared_tags > 0}
                <!-- Not folded into either bucket: these were never measured. -->
                {tagPlan.uncompared_tags} couldn't be compared
              {:else if tagPlan.compared_to}
                measured against {tagPlan.compared_to}
              {:else}
                no base to compare against
              {/if}
            </span>
            <button class="gp-btn" data-variant="danger" data-size="sm" onclick={cleanTags} disabled={!tagInvariant || selectedTags.length === 0 || busy !== null}>
              <Trash2 size={13} /> Delete {selectedTags.length} selected
            </button>
          </div>
        {:else}
          <button class="gp-btn" onclick={scanTags}><Tag size={13} /> Build tag plan</button>
        {/if}
      </section>

      <section class="gp-card p-4">
        <div class="mb-3 flex items-center justify-between">
          <div><h3 class="font-semibold">Outgoing commit review</h3><p class="text-textMuted">Conventional format, duplicate subjects, issue links, and length.</p></div>
          {#if review}<span class="gp-chip" data-tone={review.findings.length === 0 ? "ok" : "warn"} data-kind="label">{review.findings.length} findings</span>{/if}
        </div>
        {#if busy === "review"}
          <div class="py-3" aria-label="Reviewing commit headers"><Skeleton variant="text" count={3} /></div>
        {:else if review}
          <p class="mb-2 text-textSecondary">{summarizeCommitReview(review)} <span class="font-mono text-textMuted">{review.range}</span></p>
          <div class="max-h-56 space-y-1 overflow-auto">
            {#each review.findings as finding, i (`${finding.commit_id}:${finding.code}#${i}`)}
              <div class="rounded-lg bg-surface/50 px-2.5 py-2">
                <div class="flex items-center gap-2"><span class="font-mono text-accent">{finding.short_id}</span><span class={finding.severity === "error" ? "text-danger" : finding.severity === "warning" ? "text-warn" : "text-textMuted"}>{finding.code}</span></div>
                <div class="mt-0.5 truncate">{finding.subject}</div><div class="text-textMuted">{finding.detail}</div>
              </div>
            {:else}
              <div class="flex items-center gap-2 py-5 text-textMuted"><CheckCircle2 size={15} class="text-ok" /> No commit-message findings.</div>
            {/each}
          </div>
        {:else}
          <button class="gp-btn" onclick={reviewCommits}><ScanSearch size={13} /> Review outgoing commits</button>
        {/if}
      </section>

      <section class="gp-card p-4">
        <div class="mb-3 flex items-center justify-between">
          <div><h3 class="font-semibold">Issue monitor</h3><p class="text-textMuted">Refreshes open GitHub issues every minute while this view is visible.</p></div>
          <div class="flex items-center gap-2">
            {#if importPaused}
              <button
                type="button"
                class="gp-btn" data-size="sm"
                onclick={() => loadRepoTasks()}
                title={repoTasksError ?? "Reading the existing task list again"}
              >
                <RefreshCw size={12} />
                <span>Try again</span>
              </button>
            {/if}
            {#if unimportedCount > 0}
              <button
                type="button"
                class="gp-btn" data-size="sm"
                onclick={addAllIssuesToTasks}
                disabled={batchAdding || busy !== null}
                title="Add all unimported open issues into tasks for agent handoff"
              >
                {#if batchAdding}
                  <LoaderCircle size={12} class="animate-spin text-accent" />
                  <span>Importing…</span>
                {:else}
                  <ListPlus size={12} class="text-accent" />
                  <span>Add all ({unimportedCount}) to tasks</span>
                {/if}
              </button>
            {/if}
            <button class="gp-icon-btn" data-size="sm" aria-label="Refresh issues" title="Refresh issues" onclick={() => loadIssues()} disabled={busy !== null}><RefreshCw size={13} class={busy === "issues" ? "animate-spin" : ""} /></button>
          </div>
        </div>
        {#if importPaused && repoTasksError}
          <div class="gp-notice mb-2" data-tone="unknown" role="status"><AlertTriangle size={14} class="shrink-0" style="color: var(--gp-notice-ink)" /> Task import paused. Couldn't read the existing task list ({repoTasksError}), so issues are not checked against it.</div>
        {/if}
        {#if github?.error}
          <div class="gp-notice" data-tone="unknown" role="status">{github.error}</div>
        {:else if github?.issues_error}
          <div class="gp-notice" data-tone="unknown" role="status"><AlertTriangle size={14} class="shrink-0" style="color: var(--gp-notice-ink)" /> Couldn't check the issue monitor. {github.issues_error}</div>
        {:else}
          <div class="max-h-56 space-y-1.5 overflow-auto">
            {#each github?.issues ?? [] as issue (issue.number)}
              {@const inTask = findTaskForIssue(repoTasks, issue.number)}
              <div class="gp-row justify-between bg-surface/50">
                <div class="flex items-center gap-2 min-w-0 flex-1">
                  <Bug size={13} class="text-accent shrink-0" />
                  <button
                    type="button"
                    class="gp-btn shrink-0 font-mono" data-variant="ghost" data-size="sm"
                    onclick={() => openExternal(issue.url)}
                    title={`Open issue #${issue.number} on GitHub`}
                  >
                    #{issue.number}
                  </button>
                  <span class="min-w-0 flex-1 truncate text-xs font-medium" title={issue.title}>{issue.title}</span>
                  {#if issue.labels[0]}<span class="gp-chip shrink-0" data-tone="neutral" data-kind="label">{issue.labels[0]}</span>{/if}
                </div>

                <div class="flex items-center gap-1.5 shrink-0">
                  {#if inTask}
                    <button
                      type="button"
                      class="gp-chip cursor-pointer gap-1"
                      data-tone="accent"
                      onclick={openTasksView}
                      title={`Issue #${issue.number} is in tasks. Click to view on Tasks board.`}
                    >
                      <CheckCircle2 size={11} class="text-ok" />
                      <span>In tasks</span>
                    </button>
                  {:else}
                    <button
                      type="button"
                      class="gp-btn" data-size="sm"
                      onclick={() => addIssueToTask(issue)}
                      disabled={taskAddingNumber === issue.number || batchAdding || busy !== null || repoTasks === null}
                      title={`Create task from issue #${issue.number} for agents to work on`}
                    >
                      {#if taskAddingNumber === issue.number}
                        <LoaderCircle size={11} class="animate-spin text-accent" />
                        <span>Adding…</span>
                      {:else}
                        <ListPlus size={11} class="text-accent" />
                        <span>+ Task</span>
                      {/if}
                    </button>
                  {/if}
                  <button
                    type="button"
                    class="gp-icon-btn"
                    data-size="sm"
                    aria-label={`Open issue #${issue.number} on GitHub`}
                    title={`Open issue #${issue.number} on GitHub`}
                    onclick={() => openExternal(issue.url)}
                  >
                    <ExternalLink size={12} />
                  </button>
                </div>
              </div>
            {:else}
              <div class="py-3 text-textMuted">No open issues found.</div>
            {/each}
          </div>
          {#if github?.issues_truncated}<div class="mt-1 text-warn">Showing 50 issues. More open issues exist, so this is not complete coverage.</div>{/if}
        {/if}
        <div class="gp-separator" aria-hidden="true"></div>
        <div class="mt-3 space-y-2 pt-3">
          <input class="gp-field w-full" maxlength="256" placeholder="Issue title" bind:value={issueTitle} />
          <textarea class="gp-field gp-field-multi min-h-20 w-full" maxlength="65536" placeholder="What happened, what you expected, and how to reproduce it" bind:value={issueBody}></textarea>
          <div class="flex gap-2"><input class="gp-field min-w-0 flex-1" placeholder="labels, comma-separated" bind:value={issueLabels} /><button class="gp-btn" onclick={reportIssue} disabled={!issueTitle.trim() || busy !== null}><Bug size={13} /> Report issue</button></div>
        </div>
      </section>

      <section class="gp-card p-4">
        <div class="mb-3 flex items-center justify-between">
          <div>
            <h3 class="font-semibold">Release monitor and publication</h3>
            <p class="text-textMuted">Monitors published releases and pushes annotated SemVer tags.</p>
          </div>
          {#if (github?.releases?.length ?? 0) > 0}
            <span class="gp-chip" data-tone="neutral" data-kind="label">{github?.releases?.length ?? 0} releases</span>
          {/if}
        </div>

        {#if github?.error}
          <div class="gp-notice mb-3" data-tone="unknown" role="status">{github.error}</div>
        {:else if github?.releases_error}
          <div class="gp-notice mb-3" data-tone="unknown" role="status">
            <AlertTriangle size={14} class="shrink-0" style="color: var(--gp-notice-ink)" /> Couldn't check the release monitor. {github.releases_error}
          </div>
        {:else if (github?.releases?.length ?? 0) > 0}
          <div class="mb-3 max-h-44 space-y-1 overflow-auto">
            {#each github?.releases ?? [] as release, i (`${release.tag_name || release.name || "release"}#${i}`)}
              <button
                class="flex w-full items-center justify-between rounded-lg px-2 py-1.5 text-left hover:bg-surfaceHover"
                onclick={() => release.url && openExternal(release.url)}
              >
                <div class="flex items-center gap-2 min-w-0">
                  <Tag size={13} class="text-accent shrink-0" />
                  <span class="font-mono text-textPrimary font-medium truncate">{release.tag_name}</span>
                  {#if release.name && release.name !== release.tag_name}
                    <span class="text-textMuted truncate max-w-xs">{release.name}</span>
                  {/if}
                  {#if release.is_latest}
                    <span class="gp-chip" data-tone="ok" data-kind="label">latest</span>
                  {/if}
                  {#if release.is_prerelease}
                    <span class="gp-chip" data-tone="warn" data-kind="label">pre-release</span>
                  {/if}
                  {#if release.is_draft}
                    <span class="gp-chip" data-tone="neutral" data-kind="label">draft</span>
                  {/if}
                </div>
                {#if release.published_at || release.created_at}
                  <span class="text-textMuted text-meta shrink-0 font-mono">
                    {formatReleaseDate(release.published_at || release.created_at)}
                  </span>
                {/if}
              </button>
            {/each}
          </div>
          {#if github?.releases_truncated}
            <div class="mb-3 text-warn text-meta">Showing 50 releases; more releases exist. This is not complete coverage.</div>
          {/if}
        {:else if github}
          <div class="mb-3 py-2 text-textMuted">No releases found on GitHub.</div>
        {/if}

        <div class="gp-separator" aria-hidden="true"></div>
        <div class="space-y-2 pt-3">
          <div class="text-xs font-semibold text-textPrimary">Publish a new release tag</div>
          <input class="gp-field w-full font-mono" placeholder="v1.2.3" bind:value={releaseTag} />
          <input class="gp-field w-full" maxlength="4096" placeholder="Release message" bind:value={releaseMessage} />
          <div class="rounded-lg bg-surfaceHover p-2">
            <Checkbox checked={releaseConfirmed} onchange={(next) => (releaseConfirmed = next)} label="I confirm the working tree is ready." description="GitPulse will still require a clean, fully synchronized default branch and will refuse duplicate remote tags." />
          </div>
          <button class="gp-btn w-full justify-center" data-variant="primary" onclick={publishRelease} disabled={!releaseConfirmed || !releaseTag.trim() || busy !== null}>
            {#if busy === "release"}<LoaderCircle size={14} class="animate-spin" />{:else}<Rocket size={14} />{/if} Push release tag
          </button>
        </div>
        {#if (github?.workflow_runs?.length ?? 0) > 0}
          <div class="gp-separator" aria-hidden="true"></div>
          <div class="mt-3 pt-3"><div class="mb-1 text-textMuted">Recent release/workflow activity</div>{#each github?.workflow_runs.slice(0, 3) ?? [] as run (run.id)}<button class="flex w-full items-center justify-between rounded px-1 py-1 text-left hover:bg-surfaceHover" onclick={() => openExternal(run.url)}><span class="truncate">{run.title || run.name}</span><span class="gp-chip" data-tone="neutral" data-kind="label">{runState(run)}</span></button>{/each}</div>
        {/if}
      </section>
    </div>
    {/if}
  </div>
</div>
