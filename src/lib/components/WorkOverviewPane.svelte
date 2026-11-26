<script lang="ts">
  import { tick, untrack } from "svelte";
  import { currentRepoPath, repoStore } from "../stores/repoStore";
  import { conflictSessions } from "../diff/conflictSession";
  import {
    WORK_CONFLICTS_ID,
    bumpConflictDraftProbe,
    conflictDraftProbe,
    conflictsFocusRequest,
    setConflictsPanelOpen,
    takeConflictsFocus,
    takeConflictsReveal,
  } from "../ui/workConflicts";
  import { promptState } from "../stores/modalStore";
  import { harnessStore } from "../stores/harnessStore";
  import { manviFocusHint } from "../ui/manviFocus";
  import { openManviFocus } from "../ui/openManviFocus";
  import LazyView, { type ViewLoader } from "./LazyView.svelte";
  import WorkView from "./WorkView.svelte";

  /**
   * Work → Overview: the workspace, and finishing the merges that stopped.
   *
   * Resolve used to be its own section, then half of Policy. Agents finish most
   * parked merges, so nearly every visit found an empty state, and it cost a
   * tab for the rare case that needed it. Here the conflict editor leads the
   * pane only while it has something to show, and Overview follows it.
   *
   * The editor is not gated on the conflict count alone. A parked operation
   * with every file resolved still needs the editor's staged review and its
   * continue/abort banner. An operation probe that failed must be shown, not
   * hidden. Those three take the editor at full height.
   *
   * Drafts retained from an earlier operation are different: they never
   * expire, so one stale draft must not pin a full-height editor over Overview
   * for good. They get a one-line row that opens the editor on request,
   * which keeps their only door without taking the pane.
   *
   * Overview is one instance outside every branch. A merge that parks or
   * finishes while this pane is open, often an agent's doing, must not
   * rebuild it under the reader and lose its scroll, filter and inputs.
   *
   * Every read of the repository here goes through a field store
   * (`conflictedCount`, `statuses`, `currentPath`), which notifies only when
   * its own value changes. Reading `$repoStore` would re-run all of this on
   * every status publish.
   */
  let { loadConflict }: { loadConflict: ViewLoader } = $props();

  const { conflictedCount, statuses } = repoStore;
  const isBare = $derived($repoStore.isBare);
  const op = $derived($repoStore.operation);
  const live = $derived(!isBare && ($conflictedCount > 0 || op.operation !== null || op.probeFailed));
  // Retained drafts are not reactive on their own; reading the statuses,
  // operation state and `$conflictDraftProbe` re-runs this whenever the
  // repository's status moves or an action inside the editor may have changed
  // or discarded a retained draft.
  const retainedDrafts = $derived.by(() => {
    void $statuses;
    void op;
    void $conflictDraftProbe;
    const path = $currentRepoPath;
    return path && !isBare ? conflictSessions.summaries(path).length : 0;
  });
  let manualOpen = $state(false);
  const showEditor = $derived(!isBare && (live || manualOpen));
  // Conflicts outrank a blocked verdict (viewAttention.ts): while the editor
  // leads the page, the notice would only push it down.
  const blockedVerdict = $derived(!showEditor && $harnessStore.lastVerdict?.status === "blocked");

  let scroller: HTMLDivElement | undefined;
  let overviewSection: HTMLElement | undefined;

  $effect(() => {
    setConflictsPanelOpen(showEditor);
    return () => setConflictsPanelOpen(false);
  });

  // Switching repositories or entering a live merge (or bare repo) clears
  // manual reveal state so a clean/bare repository opens on Overview and a
  // finished merge returns there.
  let prevRepo: string | null | undefined = undefined;
  $effect(() => {
    const repo = $currentRepoPath;
    if (prevRepo !== undefined && repo !== prevRepo) {
      untrack(() => {
        manualOpen = false;
        bumpConflictDraftProbe();
        if (scroller) scroller.scrollTop = 0;
      });
    }
    prevRepo = repo;
    if (live || isBare) {
      untrack(() => {
        manualOpen = false;
      });
    }
  });

  // Discarding a retained draft in ConflictEditor waits on a confirmation
  // modal (`askConfirm`); when that prompt closes while the editor is open,
  // bump the draft probe after the confirmation continuation settles.
  const scheduleDraftProbe = () => {
    void tick().then(() => bumpConflictDraftProbe());
  };

  let prevPromptOpen = false;
  $effect(() => {
    const open = $promptState !== null;
    if (prevPromptOpen && !open && showEditor) untrack(scheduleDraftProbe);
    prevPromptOpen = open;
  });

  function probeDrafts(node: HTMLElement) {
    const ac = new AbortController();
    for (const type of ["click", "keyup"]) {
      node.addEventListener(type, scheduleDraftProbe, { passive: true, signal: ac.signal });
    }
    return { destroy: () => ac.abort() };
  }

  function jumpTo(el?: HTMLElement | null): void {
    el?.scrollIntoView({ block: "start" });
    el?.focus({ preventScroll: true });
  }

  function focusConflictsSection(): void {
    void tick().then(() => jumpTo(document.getElementById(WORK_CONFLICTS_ID)));
  }

  function focusOverviewSection(): void {
    jumpTo(overviewSection);
  }

  function hideConflicts(): void {
    manualOpen = false;
    bumpConflictDraftProbe();
    void tick().then(() => {
      const button = overviewSection?.querySelector<HTMLElement>('[data-testid="overview-conflicts-btn"]');
      if (button && !button.hasAttribute("disabled")) {
        button.focus({ preventScroll: true });
      } else {
        overviewSection?.focus({ preventScroll: true });
      }
    });
  }

  // A door to the conflicts (status bar, tray, merge dialog, palette, a row)
  // asks for the editor; it is taken here, on mount or while mounted, and
  // cleared even when nothing is parked any more, so it cannot fire on a
  // later visit. Explicit reveal requests (`openConflicts()`) also open the
  // editor when clean or draft-only on non-bare repositories. Scroll and focus
  // are instant, like every other deep link.
  $effect(() => {
    if (!$conflictsFocusRequest) return;
    untrack(() => {
      const reveal = takeConflictsReveal();
      takeConflictsFocus();
      if (reveal && !live && !isBare) manualOpen = true;
      if (!showEditor) return;
      focusConflictsSection();
    });
  });
</script>

<div class="flex-1 min-h-0 flex flex-col">
  {#if showEditor}
    <div
      class="shrink-0 flex items-center gap-2 px-4 py-1.5 text-meta text-textMuted"
      data-testid="work-conflicts-bar"
    >
      <span>{live ? "Resolve conflicts" : retainedDrafts > 0 ? "Retained conflict drafts" : "Conflicts and resolve"}</span>
      <div class="ml-auto flex items-center gap-1.5">
        <button type="button" class="gp-btn" data-size="sm" onclick={focusConflictsSection}>
          Conflicts
        </button>
        <button type="button" class="gp-btn" data-size="sm" onclick={focusOverviewSection}>
          Overview
        </button>
        {#if !live}
          <button type="button" class="gp-btn" data-size="sm" onclick={hideConflicts}>
            Hide
          </button>
        {/if}
      </div>
    </div>
  {/if}
  <div
    bind:this={scroller}
    class="flex-1 min-h-0 flex flex-col {showEditor ? 'overflow-y-auto' : ''}"
    data-overview-layout={showEditor ? "with-conflicts" : "overview"}
  >
    {#if showEditor}
      <!-- One scroller for the pane. The editor keeps the full pane height it
           had as its own section, so resolving is unchanged; Overview follows. -->
      <section id={WORK_CONFLICTS_ID} tabindex="-1" use:probeDrafts aria-label="Conflicts" class="work-conflicts shrink-0 flex flex-col">
        <LazyView load={loadConflict} name="the conflict editor" />
      </section>
    {:else if blockedVerdict}
      <!-- The attention dot on Overview says "Manvi blocked the last action";
           the why is in Policy activity, a collapsed disclosure below the rows.
           One line and one door, only while that verdict is the latest. -->
      <div
        class="shrink-0 flex items-center gap-2 px-4 py-1.5 text-meta text-textMuted"
        data-testid="work-blocked-verdict"
        role="status"
      >
        <span>Manvi blocked the last action.</span>
        <button type="button" class="gp-btn ml-auto" title={manviFocusHint("activity")} onclick={() => openManviFocus("activity")}>
          Show activity
        </button>
      </div>
    {/if}
    {#if !showEditor && retainedDrafts > 0}
      <div
        class="shrink-0 flex items-center gap-2 px-4 py-1.5 text-meta text-textMuted"
        data-testid="work-retained-drafts"
      >
        <span>
          {retainedDrafts} conflict draft{retainedDrafts === 1 ? "" : "s"} kept from an earlier merge or rebase.
        </span>
        <button
          type="button"
          class="gp-btn ml-auto"
          onclick={() => {
            manualOpen = true;
            focusConflictsSection();
          }}
        >
          Review drafts
        </button>
      </div>
    {/if}
    <section
      bind:this={overviewSection}
      tabindex="-1"
      aria-label="Overview"
      class="flex flex-col {showEditor ? 'shrink-0' : 'flex-1 min-h-0'}"
    >
      <WorkView />
    </section>
  </div>
</div>

<style>
  /* A definite height: the editor's root is height:100% with overflow
     hidden, so an auto-height parent would collapse it to its heading. */
  .work-conflicts {
    height: 100%;
    min-height: 28rem;
  }
</style>
