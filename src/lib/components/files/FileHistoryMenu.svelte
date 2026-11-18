<script lang="ts">
  /**
   * The open file's previous changes, one click from the file: the commits
   * that changed it, newest first, each opening that commit's diff of this
   * file on History → Diff.
   *
   * The path bar's Diff button shows the file's uncommitted change; this is
   * the way back through the ones before it. Reading the history used to mean
   * Live pulse's short list (which opened the whole commit on the graph) or
   * filtering History by path by hand.
   *
   * Placed and dismissed by the shared popover owner. The read is
   * `loadFileHistory`, which keeps a failed or unsupported read distinct from
   * a file with no commits, and says when the list is only the newest N.
   */
  import { History } from "@lucide/svelte";
  import { tick, untrack } from "svelte";
  import { invoke } from "../../ipc/invoke";
  import { portal } from "../../dom/portal";
  import { popover, restoreFocusTo, popoverExit } from "../../ui/popover";
  import { LAYERS } from "../../ui/layers";
  import { createAsyncGuard, type AsyncGuard } from "../../async/guard";
  import { loadFileHistory, type FileHistory } from "../../files/fileHistory";
  import { formatAge } from "../../storage/format";
  import { repoStore } from "../../stores/repoStore";

  let {
    repoPath,
    path,
  }: {
    repoPath: string | null;
    /** Repository-relative path of the open file. */
    path: string;
  } = $props();

  /** How many of the newest changes the menu lists. */
  const LIMIT = 20;

  let open = $state(false);
  let history = $state<FileHistory | null>(null);
  let loading = $state(false);
  let toggleEl: HTMLButtonElement | undefined = $state();
  let listEl: HTMLDivElement | undefined = $state();
  let guard: AsyncGuard | null = null;
  /** Whether the menu was opened from the keyboard, so its first entry takes focus. */
  let focusFirst = false;

  const name = $derived(path.split("/").pop() ?? path);
  const now = $derived(open ? Date.now() : 0);

  // A different file or repository closes the menu: its rows describe the old one.
  // Untracked: `close` reads `open`, and tracking it would close the menu the
  // moment it opened.
  $effect(() => {
    void path;
    void repoPath;
    untrack(() => close(false));
  });

  const dismissal = $derived({
    anchor: { kind: "element" as const, element: toggleEl, gap: 4, align: "end" as const },
    inset: 8,
    revision: `${loading}|${history?.status ?? ""}|${history?.status === "loaded" ? history.commits.length : 0}`,
    dismiss: { inside: "[data-file-history]", escape: "bubble" as const, resize: true },
    onDismiss: (reason: string) => close(reason === "escape"),
  });

  function close(restoreFocus: boolean) {
    guard?.cancel();
    guard = null;
    loading = false;
    if (!open) return;
    open = false;
    if (restoreFocus) restoreFocusTo(toggleEl);
  }

  async function load() {
    const repo = repoPath;
    if (!repo || !path) return;
    guard?.cancel();
    const mine = createAsyncGuard();
    guard = mine;
    loading = true;
    history = null;
    const answer = await loadFileHistory(invoke, repo, path, LIMIT);
    if (!mine.isLive()) return;
    history = answer;
    loading = false;
    if (focusFirst) {
      focusFirst = false;
      await tick();
      listEl?.querySelector<HTMLButtonElement>("button")?.focus();
    }
  }

  function toggle(event: MouseEvent) {
    if (open) {
      close(false);
      return;
    }
    // `detail` is 0 for a click made with the keyboard (Enter or Space).
    focusFirst = event.detail === 0;
    open = true;
    void load();
  }

  function openChange(commitId: string) {
    close(false);
    void repoStore.selectCommitFileDiff(commitId, path);
  }

  function handleListKeydown(event: KeyboardEvent) {
    if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
    const items = [...(listEl?.querySelectorAll<HTMLButtonElement>("button[data-file-history-commit]") ?? [])];
    if (items.length === 0) return;
    event.preventDefault();
    const at = items.indexOf(document.activeElement as HTMLButtonElement);
    const next = event.key === "ArrowDown" ? Math.min(items.length - 1, at + 1) : Math.max(0, at - 1);
    items[next]?.focus();
  }
</script>

<div class="relative" data-file-history>
  <button
    type="button"
    bind:this={toggleEl}
    onclick={toggle}
    aria-expanded={open}
    aria-haspopup="true"
    disabled={!repoPath}
    class="gp-btn px-1.5! text-micro flex items-center gap-1 {open ? 'text-accent bg-accent/15 border-accent!' : 'text-info'}"
    data-size="sm"
    data-file-history-toggle
    title="Previous changes to this file: open one on the Diff page"
  >
    <History size={11} />
    <span>History</span>
  </button>
  {#if open}
    <!-- Justified: the group's keydown only moves focus between the change
         buttons inside it with Up and Down; every one is also its own tab
         stop, so nothing is reachable only through the group. -->
    <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
    <div
      use:portal={"body"}
      use:popover={dismissal} out:popoverExit
      bind:this={listEl}
      data-file-history
      class="panel fixed gp-menu gp-pop p-1.5 w-80 flex flex-col gap-0.5"
      style="z-index: {LAYERS.MENU}"
      role="group"
      aria-label="Previous changes to {name}"
      data-testid="file-history-menu"
      onkeydown={handleListKeydown}
    >
      <p class="gp-label px-1.5 pt-0.5 pb-1 truncate" title={path}>Changes to {name}</p>
      {#if loading || !history}
        <p class="note" role="status">Reading this file's history…</p>
      {:else if history.status === "error"}
        <p class="note failed" role="alert">Couldn't read this file's history: {history.message}</p>
        <button type="button" class="gp-btn self-start ml-1.5 mt-1" data-size="sm" onclick={() => void load()}>Try again</button>
      {:else if history.status === "unsupported"}
        <p class="note" role="status">{history.reason}</p>
      {:else if history.commits.length === 0}
        <p class="note" role="status">No commits have changed this file yet.</p>
      {:else}
        {#each history.commits as c (c.id)}
          <button
            type="button"
            class="change"
            data-file-history-commit={c.id}
            onclick={() => openChange(c.id)}
            title="Show {c.id.slice(0, 7)}'s change to {name} on the Diff page"
          >
            <span class="meta">
              <span class="font-mono text-accent font-semibold">{c.id.slice(0, 7)}</span>
              <span class="font-mono text-textMuted">{formatAge(c.date, now)}</span>
            </span>
            <span class="subject">{c.message || "(no message)"}</span>
            <span class="author">{c.author}</span>
          </button>
        {/each}
        {#if history.hasMore}
          <p class="note pt-1">The {history.commits.length} most recent changes. Older ones are in History.</p>
        {/if}
      {/if}
    </div>
  {/if}
</div>

<style>
  .panel{max-height:min(70vh,30rem);overflow:hidden auto}
  .change{display:flex;flex-direction:column;gap:1px;text-align:left;padding:5px 6px;border-radius:var(--gp-r-sm);min-width:0}
  .change:hover{background:rgb(var(--c-surface-hover) / 0.7)}
  .change:focus-visible{outline:2px solid rgb(var(--c-accent));outline-offset:-1px}
  .meta{display:flex;justify-content:space-between;gap:8px;font-size:var(--text-micro)}
  .subject{font-size:var(--text-meta);color:rgb(var(--c-text));overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
  .author{font-size:var(--text-micro);color:rgb(var(--c-text-muted));overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
  .note{margin:0;padding:2px 6px;font-size:var(--text-micro);line-height:1.35;color:rgb(var(--c-text-muted))}
  .note.failed{color:rgb(var(--c-danger))}
</style>
