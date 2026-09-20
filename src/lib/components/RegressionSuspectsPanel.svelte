<script lang="ts">
  /**
   * Regression suspects: which commit since a known-good ref could have caused
   * a symptom.
   *
   * The fourth lens on "what happened to this repository". Graph, Diff and
   * Reflog all answer it forwards — here is the history, now read it. This one
   * answers it backwards: name the symbol that is misbehaving and the last ref
   * it was well at, and the ranking walks the call graph out from the symptom
   * and blames the lines it reaches.
   *
   * Two things this pane must never let the reader assume:
   *
   *  - The window ends at the *indexed* head, not at HEAD. The blamed lines and
   *    the graph's byte offsets have to describe the same content, so commits
   *    made since the last index are outside the question — stated in the scope
   *    line rather than left to be discovered.
   *  - An empty list after a real walk ("nothing in the cone changed") and a
   *    refusal ("the walk never ran") are different answers. They are rendered
   *    differently here, and `cone_size` / `blamed_symbols` say how much was
   *    actually examined.
   *
   * `score` is comparable only within one answer. It is rendered as a rank and
   * a bar relative to the top hit, never as a percentage or a probability.
   */
  import { Loader2, Search } from "@lucide/svelte";
  import { repoStore } from "../stores/repoStore";
  import { createAsyncGuard, type AsyncGuard } from "../async/guard";
  import { getSuspects, MAX_SUSPECT_CONE_DEPTH } from "../codeintel/client";
  import type { CodeintelSuspect, CodeintelSuspectScope } from "../codeintel/types";
  import { boundedJoin, tooltipWalkIncomplete } from "../codeintel/walkIncomplete";
  import { formatRelativeTime, formatDate, shortHash } from "../format";
  import { formatError } from "../ui/formatError";
  import { reportPanelError } from "../diagnostics/report";

  /**
   * Default window. `HEAD~20` rather than the root commit because every commit
   * in the window costs a `git blame` per file the cone reaches, and a default
   * that walks the whole history would make the first run look broken.
   */
  const DEFAULT_SINCE = "HEAD~20";

  /** Symptom symbols are qualified names, not prose; this is a generous cap. */
  const MAX_SYMPTOM_LENGTH = 512;

  let symptom = $state("");
  let since = $state(DEFAULT_SINCE);
  let depth = $state(3);

  let suspects = $state<CodeintelSuspect[]>([]);
  let scope = $state<CodeintelSuspectScope | null>(null);
  let available = $state(false);
  let reason = $state<string | null>(null);
  let walkIncomplete = $state<string | null>(null);
  let truncated = $state(false);
  let total = $state(0);
  let loading = $state(false);
  let errorMsg = $state<string | null>(null);
  /** False until the first completed run, so "no suspects" is not shown cold. */
  let ran = $state(false);

  let inflight: AsyncGuard | null = null;

  const repoPath = $derived($repoStore.currentPath);
  const canRun = $derived(
    Boolean(repoPath) && symptom.trim().length > 0 && since.trim().length > 0 && !loading,
  );
  /** Top score, for the relative bar. Never used as a denominator when zero. */
  const topScore = $derived(suspects.reduce((max, s) => Math.max(max, s.score), 0));

  /**
   * A new repository invalidates every field of the last answer. Clearing on
   * the path rather than on mount because the pane is cached by LazyView: it
   * survives a repository switch, and stale suspects under a new repo's name
   * would be read as that repo's.
   */
  $effect(() => {
    void repoPath;
    inflight?.cancel();
    inflight = null;
    suspects = [];
    scope = null;
    available = false;
    reason = null;
    walkIncomplete = null;
    truncated = false;
    total = 0;
    loading = false;
    errorMsg = null;
    ran = false;
  });

  async function run() {
    const path = repoPath;
    const symbol = symptom.trim();
    const base = since.trim();
    if (!path || !symbol || !base) return;
    inflight?.cancel();
    const guard = createAsyncGuard();
    inflight = guard;
    loading = true;
    errorMsg = null;
    try {
      const payload = await getSuspects(path, symbol.slice(0, MAX_SYMPTOM_LENGTH), base, depth);
      if (!guard.isLive()) return;
      available = payload.response.available;
      reason = payload.response.available
        ? null
        : (payload.response.reason ?? "the suspects walk did not run");
      suspects = payload.response.available ? payload.response.items : [];
      // `total` counts what the walk saw; `items` is what fitted in the budget.
      // Never below the row count, so a heading cannot claim fewer than it lists.
      total = payload.response.available
        ? Math.max(payload.response.total ?? 0, payload.response.items.length)
        : 0;
      truncated = payload.response.available && payload.response.truncated === true;
      walkIncomplete = payload.response.walk_incomplete?.trim() || null;
      scope = payload.scope;
      ran = true;
    } catch (err: unknown) {
      if (!guard.isLive()) return;
      errorMsg = formatError(err);
      reportPanelError("suspects", err);
      suspects = [];
      available = false;
      total = 0;
      scope = null;
      ran = false;
    } finally {
      if (guard.isLive()) loading = false;
    }
  }

  /**
   * Open a suspect in the Diff lens. The sections of History share one
   * selection by design, so a ranked commit is a commit the reader can read —
   * not a hash they have to go and find in the graph by hand.
   */
  function openSuspect(commit: string) {
    void repoStore.selectCommitDiff(commit);
    repoStore.setViewSection("history", "diff");
  }

  /** Evidence is a three-state enum from the backend; unknown values pass through. */
  function evidenceLabel(evidence: string): string {
    if (evidence === "body_changed") return "body changed";
    if (evidence === "moved_only") return "moved only";
    if (evidence === "unknown") return "unknown";
    return evidence;
  }

  function evidenceClass(evidence: string): string {
    if (evidence === "body_changed") return "text-red-400 border-red-400/40";
    if (evidence === "moved_only") return "text-amber-500 border-amber-500/40";
    return "text-textMuted border-border/60";
  }

  function evidenceTitle(evidence: string): string {
    if (evidence === "body_changed")
      return "The commit changed the body of a symbol the symptom reaches.";
    if (evidence === "moved_only")
      return "The commit moved these lines without changing the body — a reformat or a move, not a behaviour change on its own.";
    return "Whether the body changed could not be established. Not the same as unchanged.";
  }

  function bodyChangedLabel(changed: boolean | null): string {
    if (changed === true) return "changed";
    if (changed === false) return "moved";
    return "could not tell";
  }
</script>

<div class="flex-1 min-h-0 overflow-auto p-4 font-sans">
  <div class="mx-auto flex max-w-3xl flex-col gap-3">
    <header class="flex flex-col gap-1">
      <h2 class="text-sm font-semibold text-textPrimary">Regression suspects</h2>
      <p class="text-[11px] text-textMuted">
        Name the symbol that is misbehaving and the last ref it was well at. The
        call graph is walked out from the symptom and the lines it reaches are
        blamed, so the commits below are ranked by what they touched — not by
        how recent they are.
      </p>
    </header>

    <form
      class="flex flex-col gap-2 rounded-lg border border-border/60 bg-background/60 p-3"
      onsubmit={(event) => {
        event.preventDefault();
        void run();
      }}
    >
      <div>
        <label for="suspects-symptom" class="mb-1.5 block text-[11px] text-textMuted">
          Symptom symbol
        </label>
        <input
          id="suspects-symptom"
          type="text"
          bind:value={symptom}
          maxlength={MAX_SYMPTOM_LENGTH}
          autocomplete="off"
          spellcheck="false"
          placeholder="module::function, Class.method, or a file path"
          class="gp-field w-full font-mono"
        />
      </div>

      <div class="flex items-end gap-2">
        <div class="min-w-0 flex-1">
          <label for="suspects-since" class="mb-1.5 block text-[11px] text-textMuted">
            Last known good
          </label>
          <input
            id="suspects-since"
            type="text"
            bind:value={since}
            autocomplete="off"
            spellcheck="false"
            placeholder={DEFAULT_SINCE}
            class="gp-field w-full font-mono"
          />
        </div>
        <div class="w-24 shrink-0">
          <label for="suspects-depth" class="mb-1.5 block text-[11px] text-textMuted">
            Cone depth
          </label>
          <input
            id="suspects-depth"
            type="number"
            min="1"
            max={MAX_SUSPECT_CONE_DEPTH}
            bind:value={depth}
            class="gp-field w-full font-mono tabular-nums"
          />
        </div>
        <button type="submit" class="gp-btn shrink-0" disabled={!canRun}>
          {#if loading}
            <Loader2 size={13} class="animate-spin" />
          {:else}
            <Search size={13} />
          {/if}
          <span>Find suspects</span>
        </button>
      </div>

      {#if !repoPath}
        <p class="text-[10px] text-textMuted">Open a repository to run this query.</p>
      {/if}
    </form>

    {#if errorMsg}
      <p class="rounded-lg border border-red-400/40 bg-red-400/5 p-2 text-[11px] text-red-400" role="alert">
        {errorMsg}
      </p>
    {/if}

    {#if loading}
      <div class="flex items-center gap-1.5 text-[10px] text-textMuted">
        <Loader2 size={11} class="animate-spin" />
        <span>Walking the cone and blaming the lines it reaches…</span>
      </div>
    {/if}

    {#if ran}
      {#if scope}
        <div class="flex flex-col gap-1 rounded-lg border border-border/60 bg-background/60 p-2">
          <span class="text-[10px] font-bold uppercase tracking-wider text-textMuted">
            What was examined
          </span>
          <p class="font-mono text-[10px] text-textSecondary">
            <span title="The commit the code graph was built at. Commits made since it was built are outside this window, because the blamed lines and the graph's offsets have to describe the same content.">
              {shortHash(scope.indexed_head) || "unknown"}
            </span>
            <span class="text-textMuted">…back to…</span>
            <span>{scope.since || "unknown"}</span>
          </p>
          <p class="font-mono text-[10px] tabular-nums text-textMuted">
            {scope.cone_size.toLocaleString()} symbol{scope.cone_size === 1 ? "" : "s"} in the cone ·
            {scope.blamed_symbols.toLocaleString()} blamed
          </p>
          <p class="text-[10px] text-textMuted">
            The window ends at the indexed commit, not at HEAD.
          </p>
          {#if scope.refusals.length > 0}
            <p class="text-[10px] text-amber-500" title={boundedJoin(scope.refusals, 12)}>
              {scope.refusals.length} part{scope.refusals.length === 1 ? "" : "s"} of the walk refused
              — the ranking below is a lower bound.
            </p>
          {/if}
        </div>
      {/if}

      {#if !available}
        {@const reasonTitle = tooltipWalkIncomplete([reason])}
        <p class="min-w-0 line-clamp-3 rounded-lg border border-amber-500/40 bg-amber-500/5 p-2 text-[11px] text-amber-500" title={reasonTitle}>
          Suspects unavailable{reasonTitle ? `: ${reasonTitle}` : ""} — not the same as no suspects.
        </p>
      {:else if suspects.length === 0}
        <p class="rounded-lg border border-border/60 bg-background/60 p-2 text-[11px] text-textSecondary">
          Nothing in the cone was touched in this window. The walk ran; it found
          no commit that reaches the symptom.
        </p>
      {:else}
        <div class="flex items-baseline justify-between gap-2">
          <span class="text-[10px] font-bold uppercase tracking-wider text-textMuted">
            Ranked suspects
          </span>
          <span class="font-mono text-[10px] tabular-nums text-textMuted">
            {suspects.length} of {total.toLocaleString()}
          </span>
        </div>

        <ul class="flex flex-col gap-1.5">
          {#each suspects as suspect, index (suspect.commit)}
            <li>
              <button
                type="button"
                class="flex w-full flex-col gap-1 rounded-lg border border-border/60 bg-background/60 p-2 text-left transition-colors hover:border-accent/50 hover:bg-surface/60"
                onclick={() => openSuspect(suspect.commit)}
                title="Open this commit in the Diff lens"
              >
                <div class="flex items-center gap-2">
                  <span class="w-5 shrink-0 font-mono text-[10px] tabular-nums text-textMuted">
                    #{index + 1}
                  </span>
                  <span class="font-mono text-[11px] text-textPrimary">{shortHash(suspect.commit)}</span>
                  <span
                    class="shrink-0 rounded border px-1 text-[9px] uppercase tracking-wide {evidenceClass(suspect.evidence)}"
                    title={evidenceTitle(suspect.evidence)}
                  >
                    {evidenceLabel(suspect.evidence)}
                  </span>
                  <span class="min-w-0 flex-1 truncate text-[10px] text-textMuted">
                    {suspect.author}
                  </span>
                  <span class="shrink-0 text-[10px] text-textMuted" title={formatDate(suspect.author_time)}>
                    {formatRelativeTime(suspect.author_time)}
                  </span>
                </div>

                <div class="flex items-center gap-2">
                  <!-- Relative to the top hit, never a percentage: the score is
                       comparable only against other scores in this answer. -->
                  <div class="h-1 min-w-0 flex-1 overflow-hidden rounded-full bg-border/40">
                    <div
                      class="h-full rounded-full bg-accent/70"
                      style:width="{topScore > 0 ? Math.round((suspect.score / topScore) * 100) : 0}%"
                    ></div>
                  </div>
                  <span
                    class="shrink-0 font-mono text-[10px] tabular-nums text-textMuted"
                    title="Call edges from the symptom to the nearest symbol this commit touched. Zero means it touched the symptom itself."
                  >
                    {suspect.nearest_distance} hop{suspect.nearest_distance === 1 ? "" : "s"}
                  </span>
                </div>

                {#if suspect.touched.length > 0}
                  <ul class="flex flex-col gap-0.5 pl-7">
                    {#each suspect.touched.slice(0, 4) as touch (touch.qualified_name + touch.file_path)}
                      <li class="flex items-baseline gap-2 font-mono text-[10px] text-textSecondary">
                        <span class="min-w-0 truncate" title={touch.file_path}>
                          {touch.qualified_name}
                        </span>
                        <span class="shrink-0 tabular-nums text-textMuted">
                          {touch.distance} hop · {touch.lines} line{touch.lines === 1 ? "" : "s"}
                        </span>
                        <span
                          class="shrink-0 {touch.body_changed === null ? 'text-amber-500' : 'text-textMuted'}"
                          title={touch.body_changed === null
                            ? "Whether the body changed could not be established — not the same as unchanged."
                            : "Whether this commit changed the symbol's body or only moved it."}
                        >
                          {bodyChangedLabel(touch.body_changed)}
                        </span>
                      </li>
                    {/each}
                    {#if suspect.touched.length > 4}
                      <li
                        class="font-mono text-[10px] text-textMuted"
                        title={boundedJoin(suspect.touched.slice(4).map((t) => t.qualified_name), 12)}
                      >
                        +{suspect.touched.length - 4} more symbol{suspect.touched.length - 4 === 1 ? "" : "s"}
                      </li>
                    {/if}
                  </ul>
                {/if}
              </button>
            </li>
          {/each}
        </ul>

        {#if truncated}
          <p class="text-[10px] text-amber-500">
            Suspect list truncated by token budget — commits below the cut are
            not ruled out.
          </p>
        {/if}
      {/if}

      {#if walkIncomplete}
        {@const walkTitle = tooltipWalkIncomplete([walkIncomplete])}
        <p class="min-w-0 line-clamp-3 text-[10px] text-amber-500" title={walkTitle}>
          Walk incomplete: {walkIncomplete}
        </p>
      {/if}
    {/if}
  </div>
</div>
