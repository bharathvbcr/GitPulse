<script lang="ts">
  /**
   * What the thread changed, beside the conversation. A turn at a time by
   * default (the newest that changed anything, or the one picked from a turn
   * footer), or the whole thread.
   *
   * Each file says where its change came from: an edit the agent is asking to
   * make (proposed, shown before it lands), one it made with its own tools
   * (applied, with the exact hunks it reported, per call), or a path that
   * moved during the turn without any edit tool naming it ("Other changes"),
   * whose patch against HEAD is read only when opened. Nothing here claims a
   * shell or editor change was the agent's.
   */
  import { untrack } from "svelte";
  import { FileCode2, FilePlus2, FileMinus2, ShieldQuestion, Ban, TriangleAlert, LoaderCircle, ExternalLink, PanelRightClose } from "@lucide/svelte";
  import { changedTurns, collectChanges, describeCode, parsePatch, type ChangeScope, type FileChange } from "../../mavi/changes";
  import { maviClient } from "../../mavi/client";
  import type { MaviHunk, ThreadState } from "../../mavi/types";
  import MaviHunks from "./MaviHunks.svelte";
  import { currentRepoPath, repoStore } from "../../stores/repoStore";
  import { requestReveal } from "../../files/revealRequests";

  let {
    thread,
    root,
    sessionId,
    focus = $bindable(null),
    turn = $bindable(null),
    onHide,
  }: {
    thread: ThreadState;
    root: string | null;
    sessionId: string | null;
    /** The file expanded; set from a chip in the timeline. */
    focus?: string | null;
    /** The turn shown; null follows the newest turn that changed anything. */
    turn?: number | null;
    onHide: () => void;
  } = $props();

  let wholeThread = $state(false);
  let turns = $derived(changedTurns(thread));
  let shownTurn = $derived(turn ?? turns[0] ?? thread.turn);
  let scope = $derived<ChangeScope>(wholeThread ? { kind: "thread" } : { kind: "turn", turn: shownTurn });
  let changes = $derived(collectChanges(thread, root, scope));
  let totals = $derived({
    added: changes.reduce((n, c) => n + c.added, 0),
    removed: changes.reduce((n, c) => n + c.removed, 0),
  });
  let turnFiles = $derived(wholeThread ? null : thread.turnFiles[shownTurn] ?? null);
  let liveTurn = $derived(!wholeThread && shownTurn === thread.turn && !thread.items.some((i) => i.kind === "turn_end" && i.turn === thread.turn));

  /** Patches of unattributed files, read on demand. */
  let fetched = $state<Record<string, { hunks: MaviHunk[]; binary: boolean; truncated: boolean } | { error: string } | "loading">>({});

  $effect(() => {
    // A new focus from the timeline opens that file and the turn it is in.
    const target = focus;
    if (!target) return;
    untrack(() => {
      const change = collectChanges(thread, root, { kind: "thread" }).find((c) => c.path === target);
      if (change && !wholeThread && !change.turns.includes(shownTurn)) turn = change.turns[change.turns.length - 1] ?? null;
    });
  });

  async function loadPatch(path: string) {
    if (!sessionId || fetched[path] === "loading") return;
    fetched[path] = "loading";
    try {
      const diff = await maviClient.fileDiff(sessionId, path);
      const parsed = parsePatch(diff.patch);
      fetched[path] = { ...parsed, truncated: diff.truncated };
    } catch (e) {
      fetched[path] = { error: e instanceof Error ? e.message : String(e) };
    }
  }

  function toggle(change: FileChange) {
    focus = focus === change.path ? null : change.path;
    if (focus && !change.attributed && !fetched[change.path]) void loadPatch(change.path);
  }

  function split(path: string): { dir: string; name: string } {
    const at = path.lastIndexOf("/");
    return at < 0 ? { dir: "", name: path } : { dir: path.slice(0, at + 1), name: path.slice(at + 1) };
  }

  let canOpen = $derived(!!root && !!$currentRepoPath && root.replace(/\/+$/, "") === $currentRepoPath.replace(/\/+$/, ""));

  function openInExplorer(change: FileChange) {
    const first = change.edits.flatMap((e) => (e.fromInput ? [] : e.hunks))[0];
    if (first && first.newStart > 0) requestReveal(change.path, first.newStart, null);
    repoStore.selectFilePath(change.path);
    repoStore.setActiveTab("code", "explorer");
  }

  const STATE_LABEL: Record<FileChange["latestState"], string> = {
    proposed: "Waiting for you",
    applying: "Applying",
    applied: "",
    denied: "Denied",
    failed: "Failed",
    not_applied: "Not applied",
    moved: "",
  };
</script>

<aside class="mavi-changes flex flex-col min-h-0 h-full" aria-label="Changes">
  <header class="mavi-changes-head flex items-center gap-2">
    <span class="text-xs font-medium text-textPrimary">Changes</span>
    {#if changes.length}
      <span class="text-meta"><span class="text-ok">+{totals.added}</span> <span class="text-danger">−{totals.removed}</span></span>
    {/if}
    <span class="ml-auto"></span>
    <div class="gp-segmented" role="group" aria-label="Scope">
      <button class="gp-seg-btn" data-active={!wholeThread} onclick={() => (wholeThread = false)}>Turn</button>
      <button class="gp-seg-btn" data-active={wholeThread} onclick={() => (wholeThread = true)}>Thread</button>
    </div>
    <button class="gp-icon-btn" aria-label="Hide changes" title="Hide changes" onclick={onHide}>
      <PanelRightClose size={14} aria-hidden="true" />
    </button>
  </header>

  {#if !wholeThread && turns.length > 1}
    <div class="mavi-turns flex gap-1 overflow-x-auto" role="group" aria-label="Turn">
      {#each [...turns].reverse() as t (t)}
        <button class="gp-chip" data-tone={t === shownTurn ? "accent" : "neutral"} onclick={() => (turn = t)}>Turn {t}</button>
      {/each}
    </div>
  {/if}

  <div class="flex-1 min-h-0 overflow-y-auto mavi-changes-body">
    {#if liveTurn && changes.length}
      <p class="text-micro text-textMuted px-3 pt-2" aria-live="polite">Live: edits appear as the agent makes them; other changes are listed when the turn ends.</p>
    {/if}
    {#if turnFiles && !turnFiles.available}
      <p class="gp-notice text-meta" data-tone="unknown">
        {thread.turnFiles[shownTurn]?.reason ?? "Changes made outside the agent's edit tools were not tracked for this turn."}
      </p>
    {:else if turnFiles?.partial}
      <p class="gp-notice text-meta" data-tone="warn">The worktree had too many changed paths to compare in full; this list may be incomplete.</p>
    {/if}

    {#if changes.length === 0}
      <p class="text-xs text-textMuted px-3 py-6 text-center">
        {wholeThread ? "Nothing changed in this thread yet." : liveTurn ? "Nothing changed yet in this turn." : "This turn changed no files."}
      </p>
    {/if}

    <ul>
      {#each changes as change (change.path)}
        {@const parts = split(change.path)}
        {@const open = focus === change.path}
        <li class="mavi-file" class:mavi-file-open={open}>
          <button class="mavi-file-row flex items-center gap-2 w-full text-left" data-path={change.path} aria-expanded={open} onclick={() => toggle(change)}>
            {#if change.latestState === "proposed"}
              <ShieldQuestion size={13} class="text-warn shrink-0" aria-hidden="true" />
            {:else if change.latestState === "applying"}
              <LoaderCircle size={13} class="text-accent shrink-0 animate-spin" aria-hidden="true" />
            {:else if change.latestState === "denied" || change.latestState === "not_applied"}
              <Ban size={13} class="text-textMuted shrink-0" aria-hidden="true" />
            {:else if change.latestState === "failed"}
              <TriangleAlert size={13} class="text-danger shrink-0" aria-hidden="true" />
            {:else if change.code?.includes("?") || change.edits.some((e) => e.toolName === "Write" && e.hunks[0]?.oldLines === 0)}
              <FilePlus2 size={13} class="text-ok shrink-0" aria-hidden="true" />
            {:else if change.code?.includes("D")}
              <FileMinus2 size={13} class="text-danger shrink-0" aria-hidden="true" />
            {:else}
              <FileCode2 size={13} class="text-textMuted shrink-0" aria-hidden="true" />
            {/if}
            <span class="min-w-0 flex-1 truncate text-xs">
              <span class="text-textMuted">{parts.dir}</span><span class="text-textPrimary font-medium">{parts.name}</span>
            </span>
            {#if STATE_LABEL[change.latestState]}
              <span class="text-micro shrink-0" class:text-warn={change.latestState === "proposed"} class:text-textMuted={change.latestState !== "proposed"}>{STATE_LABEL[change.latestState]}</span>
            {/if}
            {#if change.attributed}
              <span class="text-meta shrink-0"><span class="text-ok">+{change.added}</span> <span class="text-danger">−{change.removed}</span></span>
            {:else}
              <span class="gp-chip shrink-0" data-tone="unknown" title="Changed during this turn, but no agent edit names it">Other · {describeCode(change.code)}</span>
            {/if}
          </button>
          {#if open}
            <div class="mavi-file-body">
              {#if change.attributed}
                {#each change.edits as edit (edit.toolId)}
                  <div class="text-micro text-textMuted mb-1 mt-2 flex items-center gap-1.5">
                    <span>{edit.toolName}</span>
                    {#if wholeThread}<span>· turn {edit.turn}</span>{/if}
                    <span>· {edit.state === "proposed" ? "proposed, waiting for you" : edit.state === "applying" ? "being applied" : edit.state === "not_applied" ? "not applied" : edit.state}</span>
                  </div>
                  {#if edit.hunks.length}
                    <MaviHunks hunks={edit.hunks} numbered={!edit.fromInput} maxLines={400} />
                  {:else}
                    <p class="text-meta text-textMuted">No line changes were reported for this call.</p>
                  {/if}
                {/each}
              {:else}
                {@const patch = fetched[change.path]}
                <p class="text-micro text-textMuted mt-2 mb-1">Changed during this turn outside the agent's edit tools: a command it ran, a formatter, or another program. Shown against HEAD.</p>
                {#if patch === "loading" || patch === undefined}
                  <p class="text-meta text-textMuted">Reading the change…</p>
                {:else if "error" in patch}
                  <p class="text-meta text-danger" role="alert">{patch.error}</p>
                {:else if patch.binary}
                  <p class="text-meta text-textMuted">A binary file changed.</p>
                {:else if patch.hunks.length === 0}
                  <p class="text-meta text-textMuted">{change.code?.trim() ? "No textual difference against HEAD." : "The file matches HEAD again."}</p>
                {:else}
                  <MaviHunks hunks={patch.hunks} maxLines={400} />
                  {#if patch.truncated}<p class="text-micro text-textMuted mt-1">The patch was cut at 256 KiB.</p>{/if}
                {/if}
              {/if}
              {#if canOpen}
                <button class="gp-btn mt-2" data-variant="ghost" onclick={() => openInExplorer(change)}>
                  <ExternalLink size={11} aria-hidden="true" /> Open in Explorer
                </button>
              {/if}
            </div>
          {/if}
        </li>
      {/each}
    </ul>
  </div>
</aside>

<style>
  .mavi-changes {
    background: rgb(var(--c-surface) / 0.5);
  }
  .mavi-changes-head {
    padding: 8px 10px 8px 12px;
    border-bottom: 1px solid var(--gp-edge-soft);
  }
  .mavi-turns {
    padding: 6px 12px;
    border-bottom: 1px solid var(--gp-edge-soft);
  }
  .mavi-changes-body {
    padding-bottom: 12px;
  }
  .mavi-file-row {
    padding: 5px 12px;
  }
  .mavi-file-row:hover {
    background: rgb(var(--c-surface-hover) / 0.65);
  }
  .mavi-file-open > .mavi-file-row {
    background: rgb(var(--c-surface-hover) / 0.65);
  }
  .mavi-file-body {
    padding: 0 12px 10px 33px;
  }
</style>
