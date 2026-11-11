<script module lang="ts">
  import { createRepoPanelCache } from "../panels/repoPanelCache";
  import type { ContentSearchReport as RememberedReport } from "../search/types";

  /**
   * The last search per repository, kept past the panel's unmount: Explorer
   * remounts when Code switches to Map and back, and returning would
   * otherwise find the box empty. Only the query, its options and the answer
   * are kept — never the cancel token, and nothing re-runs on return.
   */
  interface SearchMemo {
    pattern: string;
    fixedStrings: boolean;
    ignoreCase: boolean;
    revision: string;
    report: RememberedReport | null;
  }
  const searchMemo = createRepoPanelCache<SearchMemo>();
</script>

<script lang="ts">
  /**
   * Repository-wide content search, in Explorer's side column (Code →
   * Explorer → Search). A hit opens in the editor beside these results.
   *
   * One request at a time: a new search cancels the one in flight through
   * the shared query-cancel registry, and Cancel does the same. Whatever
   * comes back states what it covers — a capped, cut, timed-out or cancelled
   * answer says "partial" and why, never just a count.
   */
  import { Search, X } from "@lucide/svelte";
  import { tick, untrack } from "svelte";
  import { currentRepoPath, repoStore } from "../stores/repoStore";
  import { invoke } from "../ipc/invoke";
  import { formatError } from "../ui/formatError";
  import { createAsyncGuard, type AsyncGuard } from "../async/guard";
  import { cancelCodeintelQuery, newCodeintelCancelToken } from "../codeintel/client";
  import { describeSearchReport, groupByFile } from "../search/contentSearch";
  import { requestReveal } from "../files/revealRequests";
  import { consumeSearchFocus, explorerLens, setEditorLens } from "../files/explorerLens";
  import type { ContentMatch, ContentSearchOptions, ContentSearchReport } from "../search/types";
  import EmptyState from "./EmptyState.svelte";

  let {
    /**
     * How the host opens a file. Explorer passes its tree's own handler, so a
     * hit previews like a click in the tree and brings the editor forward on
     * a compact layout — even for the file already selected, which setting
     * the store's selection again would not announce. Without one, the
     * selection is set directly.
     */
    onOpen,
  }: { onOpen?: (path: string) => void } = $props();

  let pattern = $state("");
  let fixedStrings = $state(true);
  let ignoreCase = $state(true);
  let revision = $state("");
  // Replaced wholesale by each answer, never edited in place.
  let report = $state.raw<ContentSearchReport | null>(null);
  let error = $state<string | null>(null);
  let patternInput: HTMLInputElement | undefined = $state();
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

  /**
   * Opens a file, and a match's own line when one is named. The request is
   * recorded before the selection so it is already waiting when the viewer
   * mounts (the order the terminal's file links use). A search run at a
   * revision opens the working-tree file, whose lines need not match that
   * revision's, so it never carries a line. A line is only reachable in the
   * editor, so a hit that names one also turns Blame off.
   */
  function open(path: string, match: ContentMatch | null = null) {
    if (match && !report?.revision && requestReveal(match.path, match.line, match.column)) {
      setEditorLens("edit");
    }
    if (onOpen) onOpen(path);
    else repoStore.selectFilePath(path);
    repoStore.setActiveTab("code", "explorer");
  }

  // A different repository is a different subject: it shows what was last
  // searched there, or nothing.
  $effect(() => {
    const repo = $currentRepoPath;
    untrack(() => {
      stop();
      running = false;
      error = null;
      const memo = repo ? searchMemo.get(repo) : undefined;
      pattern = memo?.pattern ?? "";
      fixedStrings = memo?.fixedStrings ?? true;
      ignoreCase = memo?.ignoreCase ?? true;
      revision = memo?.revision ?? "";
      report = memo?.report ?? null;
    });
  });

  $effect(() => {
    const repo = $currentRepoPath;
    const memo: SearchMemo = { pattern, fixedStrings, ignoreCase, revision, report };
    if (repo) untrack(() => searchMemo.set(repo, memo));
  });

  // The box takes focus when someone asked to search — the toggle, the
  // palette — and never merely because Explorer mounted with Search up,
  // which would pull focus out of the editor. `tick` first: the request
  // usually arrives while the rail is still hidden.
  $effect(() => {
    if (!$explorerLens.searchFocusPending || !patternInput) return;
    untrack(() => {
      if (!consumeSearchFocus()) return;
      void tick().then(() => patternInput?.focus());
    });
  });

  $effect(() => () => stop());
</script>

<!-- Laid out for Explorer's rail (about 288px, resizable): the query gets
     its own full-width row, and the options wrap under it rather than
     squeezing the box. -->
<div class="flex-1 flex flex-col min-h-0 min-w-0 bg-surface/50 text-xs" data-content-search>
  <form
    class="px-2 pt-2 pb-1.5 gp-section-edge bg-surface/30 flex flex-col gap-1.5 shrink-0"
    onsubmit={(event) => { event.preventDefault(); void run(); }}
  >
    <label class="relative w-full">
      <Search size={12} class="absolute left-2.5 top-1/2 -translate-y-1/2 text-textMuted pointer-events-none" />
      <input class="gp-field w-full pl-7! font-mono" type="search" placeholder="Search file contents" aria-label="Search file contents" bind:value={pattern} bind:this={patternInput} />
    </label>
    <div class="flex items-center gap-1.5 min-w-0">
      <input class="gp-field flex-1 min-w-0 font-mono" placeholder="Revision (optional)" aria-label="Revision to search" bind:value={revision} />
      {#if running}
        <button type="button" class="gp-btn shrink-0 py-1! px-2.5! text-meta! flex items-center gap-1" onclick={cancel}><X size={12} /> Cancel</button>
      {:else}
        <button type="submit" class="gp-btn shrink-0 py-1! px-2.5! text-meta!" disabled={!pattern}>Search</button>
      {/if}
    </div>
    <div class="flex flex-wrap items-center gap-x-3 gap-y-1 px-0.5">
      <label class="flex items-center gap-1 text-meta text-textMuted"><input type="checkbox" checked={!fixedStrings} onchange={(e) => (fixedStrings = !e.currentTarget.checked)} /> regex</label>
      <label class="flex items-center gap-1 text-meta text-textMuted"><input type="checkbox" checked={!ignoreCase} onchange={(e) => (ignoreCase = !e.currentTarget.checked)} /> match case</label>
    </div>
  </form>

  {#if report}
    <div
      data-search-summary
      role="status"
      class="px-2.5 py-1.5 text-meta shrink-0 {running ? 'opacity-60' : ''} {report.truncated ? 'bg-warn/10 text-warn' : 'text-textMuted'}"
    >
      {describeSearchReport(report)}
    </div>
  {/if}

  <!-- While a newer search runs the previous answer stays up, dimmed and
       labelled, rather than reading as the current one. -->
  <div class="flex-1 min-h-0 overflow-auto px-1 py-1.5 {running && report ? 'opacity-60' : ''}" aria-busy={running}>
    {#if running && report}
      <p class="px-2 pb-1 text-meta text-textMuted" data-search-running>Searching… showing the previous results.</p>
    {/if}
    {#if report?.revision && report.matches.length > 0 && !error}
      <p class="px-2 pb-1 text-meta text-textMuted" data-search-revision-note>
        Searched at {report.revision.slice(0, 7)}. Opening a hit shows the working-tree file.
      </p>
    {/if}
    {#if error}
      <div class="p-3 text-danger" role="alert">{error}</div>
    {:else if running && !report}
      <div class="p-3 text-textMuted">Searching…</div>
    {:else if !report}
      <EmptyState icon={Search} title="Search this repository" hint="Searches tracked and untracked files (never ignored ones), or a revision's tree." />
    {:else if report.matches.length === 0}
      <EmptyState icon={Search} title={report.truncated ? "No matches before the search stopped" : "No matches"} hint={describeSearchReport(report)} />
    {:else}
      {#each groups as group (group.path)}
        <section class="mb-2">
          <button type="button" class="w-full text-left px-2 py-1 font-mono text-meta text-accent hover:underline truncate" title="Open {group.path}" onclick={() => open(group.path)}>
            {group.path} <span class="text-textMuted">({group.matches.length})</span>
          </button>
          {#each group.matches as match (`${match.line}:${match.column}`)}
            <button type="button" data-search-match class="w-full text-left flex gap-2 px-2 py-0.5 rounded hover:bg-surfaceHover font-mono text-meta" onclick={() => open(match.path, match)}>
              <span class="w-9 shrink-0 text-right text-faint tabular-nums">{match.line}</span>
              <span class="whitespace-pre truncate text-textPrimary">{match.text}{match.text_clipped ? " …" : ""}</span>
            </button>
          {/each}
        </section>
      {/each}
    {/if}
  </div>
</div>
