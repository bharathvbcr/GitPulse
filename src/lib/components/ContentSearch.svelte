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
    executedPattern: string;
    executedFixedStrings: boolean;
    executedIgnoreCase: boolean;
  }
  const searchMemo = createRepoPanelCache<SearchMemo>();
</script>

<script lang="ts">
  import Checkbox from "./Checkbox.svelte";
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
  import { untrack } from "svelte";
  import { currentRepoPath, repoStore } from "../stores/repoStore";
  import { invoke } from "../ipc/invoke";
  import { formatError } from "../ui/formatError";
  import { createAsyncGuard, type AsyncGuard } from "../async/guard";
  import { cancelCodeintelQuery, newCodeintelCancelToken } from "../codeintel/client";
  import { describeSearchReport, groupByFile } from "../search/contentSearch";
  import { requestReveal } from "../files/revealRequests";
  import { setEditorLens } from "../files/explorerLens";
  import {
    buildMatcher,
    matchesInLine,
    sliceTokensWithMatches,
    type LineMatch,
  } from "../text/lineSearch";
  import type { ContentMatch, ContentSearchOptions, ContentSearchReport } from "../search/types";
  import EmptyState from "./EmptyState.svelte";

  let {
    repoPath = null,
    /**
     * How the host opens a file. Explorer passes its tree's own handler, so a
     * hit previews like a click in the tree and brings the editor forward on
     * a compact layout — even for the file already selected, which setting
     * the store's selection again would not announce. Without one, the
     * selection is set directly.
     */
    onOpen,
    onSelectMatch,
    pattern = $bindable(""),
  }: {
    repoPath?: string | null;
    onOpen?: (path: string) => void;
    onSelectMatch?: (path: string, line: number | null, column: number | null) => void;
    /**
     * The query. The field it is typed in belongs to the host — Explorer's one
     * search field in Contents mode — so this panel holds only its options,
     * the Search button and the results.
     */
    pattern?: string;
  } = $props();

  const effectiveRepoPath = $derived(repoPath ?? $currentRepoPath);

  let fixedStrings = $state(true);
  let ignoreCase = $state(true);
  let revision = $state("");
  // Replaced wholesale by each answer, never edited in place.
  let report = $state.raw<ContentSearchReport | null>(null);
  let executedPattern = $state("");
  let executedFixedStrings = $state(true);
  let executedIgnoreCase = $state(true);
  let error = $state<string | null>(null);
  let running = $state(false);
  let inflight: AsyncGuard | null = null;
  let token: string | null = null;

  const groups = $derived(report ? groupByFile(report.matches) : []);
  const resultMatcher = $derived(
    executedPattern
      ? buildMatcher(executedPattern, {
          regex: !executedFixedStrings,
          caseSensitive: !executedIgnoreCase,
        })
      : null,
  );

  function highlightResultLine(text: string): { text: string; match: "none" | "match" | "active" }[] | null {
    const matcher = resultMatcher;
    if (!matcher || !text) return null;
    const found: LineMatch[] = [];
    matchesInLine(text, matcher, 0, found, 32);
    if (found.length === 0) return null;
    return sliceTokensWithMatches(
      text,
      [{ text }],
      found.map((m) => ({ start: m.colStart, end: m.colStart + m.length, active: false })),
      (t) => ({ text: t }),
    );
  }

  function stop() {
    inflight?.cancel();
    if (token) void cancelCodeintelQuery(token).catch(() => false);
    token = null;
  }

  /** Runs the search for the current pattern; the host's field calls it on Enter. */
  export async function run() {
    const repo = effectiveRepoPath;
    if (!repo || !pattern) return;
    stop();
    const guard = createAsyncGuard();
    inflight = guard;
    const mine = newCodeintelCancelToken();
    token = mine;
    running = true;
    error = null;
    const submittedPattern = pattern;
    const submittedFixed = fixedStrings;
    const submittedIgnoreCase = ignoreCase;
    const options: ContentSearchOptions = {
      fixed_strings: submittedFixed,
      ignore_case: submittedIgnoreCase,
      revision: revision.trim() || null,
    };
    try {
      const next = await invoke<ContentSearchReport>("cmd_search_content", {
        repoPath: repo,
        pattern: submittedPattern,
        options,
        cancelToken: mine,
      });
      if (guard.isLive()) {
        report = next;
        executedPattern = submittedPattern;
        executedFixedStrings = submittedFixed;
        executedIgnoreCase = submittedIgnoreCase;
      }
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
    if (onSelectMatch) {
      const hasLine = Boolean(match && !report?.revision);
      onSelectMatch(path, hasLine ? (match?.line ?? null) : null, hasLine ? (match?.column ?? null) : null);
      return;
    }
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
    const repo = effectiveRepoPath;
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
      executedPattern = memo?.executedPattern ?? "";
      executedFixedStrings = memo?.executedFixedStrings ?? true;
      executedIgnoreCase = memo?.executedIgnoreCase ?? true;
    });
  });

  $effect(() => {
    const repo = effectiveRepoPath;
    const memo: SearchMemo = {
      pattern,
      fixedStrings,
      ignoreCase,
      revision,
      report,
      executedPattern,
      executedFixedStrings,
      executedIgnoreCase,
    };
    if (repo) untrack(() => searchMemo.set(repo, memo));
  });

  $effect(() => () => stop());
</script>

<!-- Laid out for Explorer's rail (about 288px, resizable): the revision and
     Search button share a row, and the options wrap under it rather than
     squeezing the box. The query itself is typed in Explorer's field above. -->
<div class="flex-1 flex flex-col min-h-0 min-w-0 text-xs" data-content-search>
  <form
    class="px-2 pt-2 pb-1.5 gp-section-edge flex flex-col gap-1.5 shrink-0"
    onsubmit={(event) => { event.preventDefault(); void run(); }}
  >
    <div class="flex items-center gap-1.5 min-w-0">
      <input class="gp-field flex-1 min-w-0 font-mono" placeholder="Revision (optional)" aria-label="Revision to search" bind:value={revision} />
      {#if running}
        <button type="button" class="gp-btn shrink-0 px-2.5! text-meta! flex items-center gap-1" data-size="sm" onclick={cancel}><X size={12} /> Cancel</button>
      {:else}
        <button type="submit" class="gp-btn shrink-0 px-2.5! text-meta!" data-size="sm" disabled={!pattern}>Search</button>
      {/if}
    </div>
    <div class="flex flex-wrap items-center gap-x-3 gap-y-1 px-0.5">
      <label class="flex items-center gap-1 text-meta text-textMuted"><Checkbox ariaLabel="Regex" checked={!fixedStrings} onchange={(next) => (fixedStrings = !next)} /> regex</label>
      <label class="flex items-center gap-1 text-meta text-textMuted"><Checkbox ariaLabel="Match case" checked={!ignoreCase} onchange={(next) => (ignoreCase = !next)} /> match case</label>
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
            {@const lineSpans = highlightResultLine(match.text)}
            <button type="button" data-search-match class="w-full text-left flex gap-2 px-2 py-0.5 rounded hover:bg-surfaceHover font-mono text-meta" onclick={() => open(match.path, match)}>
              <span class="w-9 shrink-0 text-right text-faint tabular-nums">{match.line}</span>
              <span class="whitespace-pre truncate text-textPrimary">{#if lineSpans}{#each lineSpans as span}<span class={span.match === 'match' ? 'rounded-sm bg-warn/30' : ''}>{span.text}</span>{/each}{:else}{match.text}{/if}{match.text_clipped ? " …" : ""}</span>
            </button>
          {/each}
        </section>
      {/each}
    {/if}
  </div>
</div>
