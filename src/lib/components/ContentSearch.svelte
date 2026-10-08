<script lang="ts">
  /**
   * Repository-wide content search (Code → Search).
   *
   * One request at a time: a new search cancels the one in flight through
   * the shared query-cancel registry, and Cancel does the same. Whatever
   * comes back states what it covers — a capped, cut, timed-out or cancelled
   * answer says "partial" and why, never just a count.
   */
  import { Search, X } from "@lucide/svelte";
  import { repoStore } from "../stores/repoStore";
  import { invoke } from "../ipc/invoke";
  import { formatError } from "../ui/formatError";
  import { createAsyncGuard, type AsyncGuard } from "../async/guard";
  import { cancelCodeintelQuery, newCodeintelCancelToken } from "../codeintel/client";
  import { describeSearchReport, groupByFile } from "../search/contentSearch";
  import type { ContentSearchOptions, ContentSearchReport } from "../search/types";
  import EmptyState from "./EmptyState.svelte";

  let pattern = $state("");
  let fixedStrings = $state(true);
  let ignoreCase = $state(true);
  let revision = $state("");
  let report = $state<ContentSearchReport | null>(null);
  let error = $state<string | null>(null);
  let running = $state(false);
  let inflight: AsyncGuard | null = null;
  let token: string | null = null;

  const groups = $derived(report ? groupByFile(report.matches) : []);

  function stop() {
    inflight?.cancel();
    if (token) void cancelCodeintelQuery(token).catch(() => false);
    token = null;
  }

  async function run() {
    const repo = $repoStore.currentPath;
    if (!repo || !pattern) return;
    stop();
    const guard = createAsyncGuard();
    inflight = guard;
    const mine = newCodeintelCancelToken();
    token = mine;
    running = true;
    error = null;
    const options: ContentSearchOptions = {
      fixed_strings: fixedStrings,
      ignore_case: ignoreCase,
      revision: revision.trim() || null,
    };
    try {
      const next = await invoke<ContentSearchReport>("cmd_search_content", {
        repoPath: repo,
        pattern,
        options,
        cancelToken: mine,
      });
      if (guard.isLive()) report = next;
    } catch (err) {
      if (guard.isLive()) {
        error = formatError(err);
        report = null;
      }
    } finally {
      if (token === mine) {
        token = null;
        running = false;
      }
    }
  }

  /**
   * Cancel keeps the request live so its partial answer — marked cancelled
   * by the backend — still lands; only a newer search discards it.
   */
  function cancel() {
    if (token) void cancelCodeintelQuery(token).catch(() => false);
  }

  function open(path: string) {
    repoStore.selectFilePath(path);
    repoStore.setActiveTab("code", "explorer");
  }

  // A different repository is a different subject.
  let lastRepo: string | null = null;
  $effect(() => {
    const repo = $repoStore.currentPath;
    if (repo === lastRepo) return;
    lastRepo = repo;
    stop();
    running = false;
    report = null;
    error = null;
  });

  $effect(() => () => stop());
</script>

<div class="flex-1 flex flex-col min-h-0 bg-background text-xs">
  <form
    class="px-4 py-2 border-b border-border/60 gp-section-edge bg-surface/60 flex flex-wrap items-center gap-2 shrink-0"
    onsubmit={(event) => { event.preventDefault(); void run(); }}
  >
    <label class="relative flex-1 min-w-56">
      <Search size={12} class="absolute left-2.5 top-1/2 -translate-y-1/2 text-textMuted pointer-events-none" />
      <input class="gp-field w-full pl-7! font-mono" type="search" placeholder="Search file contents" aria-label="Search file contents" bind:value={pattern} />
    </label>
    <input class="gp-field w-36 font-mono" placeholder="Revision (optional)" aria-label="Revision to search" bind:value={revision} />
    <label class="flex items-center gap-1 text-[11px] text-textMuted"><input type="checkbox" checked={!fixedStrings} onchange={(e) => (fixedStrings = !e.currentTarget.checked)} /> regex</label>
    <label class="flex items-center gap-1 text-[11px] text-textMuted"><input type="checkbox" checked={!ignoreCase} onchange={(e) => (ignoreCase = !e.currentTarget.checked)} /> match case</label>
    {#if running}
      <button type="button" class="gp-btn py-1! px-2.5! text-[11px]! flex items-center gap-1" onclick={cancel}><X size={12} /> Cancel</button>
    {:else}
      <button type="submit" class="gp-btn py-1! px-2.5! text-[11px]!" disabled={!pattern}>Search</button>
    {/if}
  </form>

  {#if report}
    <div
      data-search-summary
      role="status"
      class="px-4 py-1.5 border-b border-border/40 text-[11px] shrink-0 {report.truncated ? 'bg-amber-500/10 text-amber-600 dark:text-amber-300' : 'text-textMuted'}"
    >
      {describeSearchReport(report)}
    </div>
  {/if}

  <div class="flex-1 min-h-0 overflow-auto px-2 py-2">
    {#if error}
      <div class="p-3 text-rose-400" role="alert">{error}</div>
    {:else if running && !report}
      <div class="p-3 text-textMuted">Searching…</div>
    {:else if !report}
      <EmptyState icon={Search} title="Search this repository" hint="Searches tracked and untracked files (never ignored ones), or a revision's tree." />
    {:else if report.matches.length === 0}
      <EmptyState icon={Search} title={report.truncated ? "No matches before the search stopped" : "No matches"} hint={describeSearchReport(report)} />
    {:else}
      {#each groups as group (group.path)}
        <section class="mb-2">
          <button type="button" class="w-full text-left px-2 py-1 font-mono text-[11px] text-accent hover:underline truncate" title="Open {group.path}" onclick={() => open(group.path)}>
            {group.path} <span class="text-textMuted">({group.matches.length})</span>
          </button>
          {#each group.matches as match (`${match.line}:${match.column}`)}
            <button type="button" data-search-match class="w-full text-left flex gap-2 px-2 py-0.5 rounded hover:bg-surfaceHover font-mono text-[11px]" onclick={() => open(match.path)}>
              <span class="w-12 shrink-0 text-right text-textMuted/60 tabular-nums">{match.line}</span>
              <span class="whitespace-pre truncate text-textPrimary">{match.text}{match.text_clipped ? " …" : ""}</span>
            </button>
          {/each}
        </section>
      {/each}
    {/if}
  </div>
</div>
