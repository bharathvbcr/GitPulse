<script lang="ts">
  import { invoke } from "../ipc/invoke";
  import { untrack } from "svelte";
  import { currentRepoPath, repoStore } from "../stores/repoStore";
  import { graphStore, normalizeDiffPayload } from "../stores/graphStore";
  import { resolveCurrentCommitId } from "../graph/currentCommit";
  import { harnessStore, type AiGeneration } from "../stores/harnessStore";
  import { createAsyncGuard, type AsyncGuard } from "../async/guard";
  import {
    GitCommit,
    User,
    Calendar,
    Plus,
    Minus,
    ShieldCheck,
    ShieldAlert,
    Sparkles,
    Loader,
    AlertTriangle,
    Copy,
    Check,
    Bug,
    GitBranchPlus,
    RotateCcw,
  } from "@lucide/svelte";
  import { createBranchAt, resetKeepTo } from "../branches/commitActions";
  import { historySelectionLabel, labelFor } from "../views/historyList";
  import {
    suspectsQuery,
    suspectMarks,
    evidenceTone,
    evidenceLabel,
    evidenceTitle,
    hopsLabel,
  } from "../codeintel/suspectsQuery";
  import EmptyState from "./EmptyState.svelte";
  import Skeleton from "./Skeleton.svelte";
  import LanguageLogo from "./LanguageLogo.svelte";
  import MarkdownBody from "./MarkdownBody.svelte";
  import { formatPathParts } from "../files/formatPath";
  import { formatDate, shortHash, signatureLabel } from "../format";
  import { summarizeCommitFiles } from "../diff/commitFilesNotice";
  import { describeFailure, type FailureCopy } from "../ui/copy/describeFailure";
  import { copyText } from "../desktop/clipboard";
  import { toastStore } from "../stores/toastStore";
  import { filterStore } from "../stores/filterStore";
  // The canonical wire types, not local copies: both were declared here as
  // well, and the CommitDetails one had silently fallen four fields behind.
  // `import type` is erased, so this pulls in no store at runtime.
  import type { SelectedCommitDetails as CommitDetailsPayload } from "../stores/graphStore";
  import FreshnessBadge from "./FreshnessBadge.svelte";
  import type { ProvenanceFreshness } from "../provenance/types";


  // One id for the header, the details and the preview: the same resolution
  // the commit list uses for its highlight (graph/currentCommit.ts), so the
  // panes cannot describe different commits.
  let currentCommitId = $derived(
    resolveCurrentCommitId(
      $repoStore.selectedCommitId,
      $graphStore.selectedCommit?.id ?? null,
      $graphStore.rows,
    ),
  );
  let selectedCommit = $derived(
    currentCommitId === null
      ? null
      : ($graphStore.rows.find((r) => r.id === currentCommitId) ??
          ($graphStore.selectedCommit?.id === currentCommitId ? $graphStore.selectedCommit : null)),
  );

  let details = $state<CommitDetailsPayload | null>(null);

  // A commit selected from the reflog that the graph walk does not draw (a
  // reset or amend orphaned it). Its row is a stand-in carrying only the id
  // and the reflog's message, so author, date and subject come from the
  // details read, never from the stand-in's empty fields.
  let offGraph = $derived(
    selectedCommit !== null && !$graphStore.rows.some((r) => r.id === selectedCommit.id),
  );
  // Where Graph's Suspects list ranked this commit, whichever list it was
  // selected from: the verdict travels with the commit, not with the list.
  let suspect = $derived(
    selectedCommit ? (suspectMarks($suspectsQuery, $currentRepoPath).get(selectedCommit.id) ?? null) : null,
  );
  let filterActive = $derived(
    $filterStore.searchQuery.trim() !== "" || Boolean($filterStore.selectedBranch),
  );
  // What the list that selected this commit calls it (a reflog selector), for
  // the branch-here and reset-here prompts.
  let actionLabel = $derived(labelFor($historySelectionLabel, selectedCommit?.id));
  let readDetails = $derived(details !== null && details.id === selectedCommit?.id ? details : null);
  let headerSummary = $derived(
    offGraph && readDetails ? readDetails.summary : (selectedCommit?.summary ?? ""),
  );
  let headerAuthor = $derived(
    offGraph ? (readDetails?.author_name ?? "") : (selectedCommit?.author_name ?? ""),
  );

  // The store's read status describes ITS selected commit; it only speaks for
  // the commit on screen when they are the same one. Anything else is "not
  // read yet", which renders as loading, never as an empty commit.
  let detailsStatus = $derived(
    $graphStore.selectedCommit?.id === currentCommitId
      ? $graphStore.selectedCommitDetailsStatus
      : "idle",
  );
  let detailsError = $derived(
    detailsStatus === "error" ? $graphStore.selectedCommitDetailsError : null,
  );
  // The preview pane's own states, in the order they win: a read in flight (or
  // not started: `selectedDiff` is null until the first read lands), a failed
  // read, then the text. An empty string is a diff that was read and is empty.
  let diffLoading = $derived(
    $repoStore.selectedDiffPending ||
      ($repoStore.selectedDiff === null && !$repoStore.selectedDiffError),
  );
  let diffError = $derived($repoStore.selectedDiffError);
  /** Headline, then the reason, as one line. A space written between block tags is trimmed, so it is joined here. */
  function failureLine(copy: FailureCopy): string {
    return copy.detail ? `${copy.headline} ${copy.detail}` : copy.headline;
  }
  let detailsFailure = $derived(
    detailsStatus === "error" ? describeFailure({ verb: "load", object: "commit details" }, detailsError) : null,
  );
  let diffFailure = $derived(
    diffError ? describeFailure({ verb: "read", object: "this diff" }, diffError) : null,
  );

  /**
   * "Find suspects since this commit": the commit on screen becomes the last
   * known good and Graph switches to the Suspects list. Runs at once when a
   * symptom is already named, otherwise puts the cursor in the symptom field.
   */
  function findSuspectsSince(commitId: string) {
    repoStore.setHistoryList("suspects");
    void suspectsQuery.seedSince($currentRepoPath, commitId);
  }

  function retryDetails() {
    if (selectedCommit) void graphStore.retryDetails();
  }

  function retryDiff() {
    const id = currentCommitId;
    if (!id) return;
    const filePath = $repoStore.selectionKind === "commit" ? $repoStore.selectedFilePath : null;
    if (filePath) void repoStore.selectCommitFileDiff(id, filePath);
    else void repoStore.selectCommitDiff(id);
  }

  // Agent refactors produce commits with tens of thousands of files; the
  // list renders a window of them and reveals more on demand instead of
  // mounting the whole set at once.
  const FILE_LIST_STEP = 200;
  let fileListLimit = $state(FILE_LIST_STEP);
  let fileList = $derived(details?.changed_files.slice(0, fileListLimit) ?? []);
  let hiddenFileCount = $derived((details?.changed_files.length ?? 0) - fileList.length);
  $effect(() => {
    void currentCommitId;
    fileListLimit = FILE_LIST_STEP;
  });
  // When the backend capped the list, the header reports the true file count
  // instead of the size of the capped slice.
  let filesSummary = $derived(summarizeCommitFiles(details));

  // Same story for the inline preview: a capped slice keeps the pane
  // responsive, and the full diff opens in the virtualized Diff view. The
  // normalizer keeps a legacy string payload and a new payload object alike
  // from reaching `.split` unvalidated.
  const PREVIEW_LINE_CAP = 2_000;
  let selectedDiffText = $derived(normalizeDiffPayload($repoStore.selectedDiff).content);
  let previewLines = $derived.by(() => {
    if (!selectedDiffText) return [];
    return selectedDiffText.split("\n").slice(0, PREVIEW_LINE_CAP);
  });
  let previewTruncated = $derived(selectedDiffText.split("\n").length > PREVIEW_LINE_CAP);

  // Explain-this-commit, answered by a model on this machine.
  let explanation = $state<AiGeneration | null>(null);
  let explanationFor = $state<string | null>(null);
  let isExplaining = $state(false);
  let explainFailure = $state<FailureCopy | null>(null);

  let aiReady = $derived($harnessStore.ai?.ready ?? false);
  let explainGuard: AsyncGuard | null = null;

  let copiedSha = $state(false);
  let copiedMessage = $state(false);
  // Held so a pane that unmounts mid-flash does not write to dead state.
  let copiedShaTimer: ReturnType<typeof setTimeout> | undefined;
  let copiedMessageTimer: ReturnType<typeof setTimeout> | undefined;
  $effect(() => {
    return () => {
      clearTimeout(copiedShaTimer);
      clearTimeout(copiedMessageTimer);
    };
  });

  async function copySha(id: string) {
    if (!(await copyText(id))) {
      toastStore.error("Couldn't copy the commit SHA.");
      return;
    }
    copiedSha = true;
    toastStore.info(`Copied full SHA: ${id.slice(0, 8)}`, undefined, 2000);
    clearTimeout(copiedShaTimer);
    copiedShaTimer = setTimeout(() => (copiedSha = false), 1500);
  }

  async function copyMessage(msg: string) {
    if (!(await copyText(msg))) {
      toastStore.error("Couldn't copy the commit message.");
      return;
    }
    copiedMessage = true;
    toastStore.info("Copied commit message", undefined, 2000);
    clearTimeout(copiedMessageTimer);
    copiedMessageTimer = setTimeout(() => (copiedMessage = false), 1500);
  }

  async function explainCommit() {
    const path = $repoStore.currentPath;
    const id = currentCommitId;
    if (!path || !id) return;
    explainGuard?.cancel();
    const guard = createAsyncGuard();
    explainGuard = guard;
    isExplaining = true;
    explainFailure = null;
    explanation = null;
    try {
      const result = await harnessStore.explainCommit(path, id);
      if (!guard.isLive()) return;
      if (currentCommitId !== id || $repoStore.currentPath !== path) return;
      explanation = result;
      explanationFor = id;
    } catch (err: unknown) {
      if (!guard.isLive()) return;
      if (currentCommitId !== id || $repoStore.currentPath !== path) return;
      explainFailure = describeFailure({ verb: "explain", object: "this commit" }, err);
    } finally {
      if (guard.isLive()) isExplaining = false;
    }
  }

  $effect(() => {
    return () => explainGuard?.cancel();
  });

  // A new commit or repository outdates an explanation still being written.
  $effect(() => {
    void currentCommitId;
    void $currentRepoPath;
    untrack(() => {
      explainGuard?.cancel();
      isExplaining = false;
      // The error belongs to the commit it failed on; left set it reads as
      // "this commit failed" under every commit clicked afterwards.
      explainFailure = null;
    });
  });

  $effect(() => {
    const id = currentCommitId;
    if (!id) {
      details = null;
      return;
    }
    // The graph store owns detail fetching (selectCommit and the auto-select
    // on load both populate it). Fetching here as well doubled every backend
    // traversal per click; this pane just mirrors the store, showing blank
    // until the one in-flight fetch lands.
    const cached = $graphStore.selectedCommitDetails;
    details = cached?.id === id ? cached : null;
  });

  /**
   * Git-native provenance for the selected commit.
   *
   * The single-commit measurement rather than the batch the branch and PR
   * lists use: those badge many rows and skip measuring commits that carry no
   * note, because a commit nobody verified has no verification whose age could
   * decay. Here one commit is in front of the reader and "how far has the base
   * moved since this" is a fair question whether or not anything was recorded.
   */
  let provenance = $state<ProvenanceFreshness | null>(null);
  /** Monotonic: clicking through history faster than the fetch returns must
   *  never leave an earlier commit's provenance on a later one. */
  let provenanceToken = 0;

  // Keyed by value on the repository and the commit, so a store publish that
  // changed neither keeps the badge and any fetch in flight. Only a new
  // target blanks it and outdates the previous request.
  $effect(() => {
    const repo = $currentRepoPath;
    const id = currentCommitId;
    const mine = ++provenanceToken;
    provenance = null;
    if (!repo || !id) return;
    invoke<ProvenanceFreshness>("cmd_provenance_freshness", {
      repoPath: repo,
      commitSha: id,
      baseBranch: null,
    })
      .then((f) => {
        if (mine === provenanceToken) provenance = f;
      })
      .catch(() => {
        // Left null, which draws nothing. The badge is an addition to this
        // pane, not a claim about it: a failed lookup must not put an error
        // where a verification would go.
      });
  });

  // The label comes from the separate one-commit verification the store runs
  // after the details land: the details themselves carry no verdict, so
  // `signature_check` is `undefined` while that runs and `null` if it failed.
</script>

{#if selectedCommit}
  {@const sig = signatureLabel(details?.signature_check, details != null && details.signature_check === undefined)}
  <div class="h-64 bg-background flex flex-col font-sans select-none overflow-hidden">
    <div class="gp-header justify-between">
      <div class="flex items-center gap-3 min-w-0">
        <GitCommit size={16} class="text-accent shrink-0" />
        <button
          type="button"
          onclick={() => copySha(selectedCommit.id)}
          class="font-mono text-xs font-semibold text-accent hover:underline flex items-center gap-1 group/sha"
          title="Click to copy full commit SHA ({selectedCommit.id})"
          aria-label="Copy commit SHA"
        >
          <span>{shortHash(selectedCommit.id, 8)}</span>
          {#if copiedSha}
            <Check size={11} class="text-ok" />
          {:else}
            <Copy size={10} class="text-textMuted opacity-0 group-hover/sha:opacity-100 group-focus-within/sha:opacity-100 transition-opacity" />
          {/if}
        </button>

        <div class="flex items-center gap-1.5 min-w-0 group/msg">
          <span class="text-xs font-medium text-textPrimary truncate select-text">{headerSummary}</span>
          {#if offGraph}
            <!-- HEAD moves and Suspects keep their selection when a reload
                 stops drawing it, so "not drawn" has two causes. A suspect is
                 an ancestor of the indexed commit, and with a filter active
                 the filter is the likelier one: those say "loaded". -->
            {@const filtered = Boolean(suspect) || filterActive}
            <span
              class="gp-chip shrink-0"
              data-tone="warn"
              title={filtered
                ? "The loaded graph does not draw this commit: the commit filter or the page size leaves it out. It stays selected because it was chosen from HEAD's moves or Suspects."
                : "The loaded graph does not draw this commit. It was selected from HEAD's moves (the reflog); a reset or amend may have left it reachable only from there, or the page size leaves it out."}
            >{filtered ? "Not in the loaded graph" : "Not in the graph"}</span>
          {/if}
          {#if suspect}
            <span class="gp-chip shrink-0" data-tone="danger" title={suspect.title}>suspect #{suspect.rank}</span>
          {/if}
          <button
            type="button"
            onclick={() => copyMessage(headerSummary)}
            class="gp-icon-btn opacity-0 group-hover/msg:opacity-100 group-focus-within/msg:opacity-100 focus-visible:opacity-100 transition-opacity"
            data-size="sm"
            title="Copy commit message"
            aria-label="Copy commit message"
          >
            {#if copiedMessage}
              <Check size={11} class="text-ok" />
            {:else}
              <Copy size={11} />
            {/if}
          </button>
        </div>
      </div>
      <div class="flex items-center gap-4 text-meta text-textMuted shrink-0">
        <span class="flex items-center gap-1.5 select-text"><User size={13} /> {headerAuthor || "—"}</span>
        <span class="flex items-center gap-1.5 select-text"><Calendar size={13} /> {details?.author_date || (offGraph ? "—" : formatDate(selectedCommit.timestamp))}</span>
        <span class="flex items-center gap-1 {sig.ok ? 'text-ok' : 'text-textMuted'}">
          {#if sig.ok}
            <ShieldCheck size={13} />
          {:else}
            <ShieldAlert size={13} />
          {/if}
          {sig.text}
        </span>
        <FreshnessBadge freshness={provenance} />
        <!-- What can be done to this commit, whichever list selected it. HEAD
             moves used to carry these on every row; they are the same two
             actions as the graph's row menu, from one owner. -->
        <button
          type="button"
          class="flex items-center gap-1 rounded px-1 py-0.5 text-textMuted hover:bg-surfaceHover hover:text-textPrimary"
          onclick={() => void createBranchAt(selectedCommit.id, actionLabel)}
          title="Create a branch at this commit — the non-destructive way back to it"
          aria-label="Create a branch at this commit"
        >
          <GitBranchPlus size={13} /> Branch
        </button>
        <button
          type="button"
          class="flex items-center gap-1 rounded px-1 py-0.5 text-textMuted hover:bg-surfaceHover hover:text-danger"
          onclick={() => void resetKeepTo(selectedCommit.id, actionLabel)}
          title="Reset the current branch here (keeps local changes)… — previews which commits would leave the branch first"
          aria-label="Reset the current branch to this commit"
        >
          <RotateCcw size={13} /> Reset
        </button>
        <button
          type="button"
          class="flex items-center gap-1 rounded px-1 py-0.5 text-textMuted hover:bg-surfaceHover hover:text-textPrimary"
          onclick={() => findSuspectsSince(selectedCommit.id)}
          title="Find suspects since this commit: treat it as the last known good and rank the commits after it by what the call graph reaches from a symptom"
        >
          <Bug size={13} /> Suspects since
        </button>
      </div>
    </div>

    {#if suspect}
      <!-- The Suspects verdict on this commit, under whichever list selected it.
           Told apart from the header by its tint, not by a rule. -->
      <div class="flex items-center gap-2 bg-danger/5 px-4 py-1 text-micro text-textSecondary min-w-0" aria-label="Suspect verdict">
        <span
          class="gp-chip shrink-0"
          data-tone={evidenceTone(suspect.suspect.evidence)}
          title={evidenceTitle(suspect.suspect.evidence)}
        >{evidenceLabel(suspect.suspect.evidence)}</span>
        <span class="shrink-0 font-mono tabular-nums text-textMuted" title="Call edges from the symptom to the nearest symbol this commit touched.">
          {hopsLabel(suspect.suspect.nearest_distance)}
        </span>
        {#if suspect.suspect.touched.length > 0}
          <span class="min-w-0 truncate font-mono" title={suspect.suspect.touched.map((t) => t.qualified_name).join("\n")}>
            touched {suspect.suspect.touched.slice(0, 3).map((t) => t.qualified_name).join(", ")}{suspect.suspect.touched.length > 3 ? ` +${suspect.suspect.touched.length - 3} more` : ""}
          </span>
        {/if}
      </div>
    {/if}

    <div class="flex-1 grid grid-cols-3 min-h-0">
      <div class="p-2 overflow-y-auto space-y-1">
        <div class="gp-label px-2 py-1">
          Changed files ({details ? filesSummary.count : "--"})
          {#if details}
            <span class="font-mono text-ok ml-1">+{details.total_additions}{filesSummary.partial ? "+" : ""}</span>
            <span class="font-mono text-danger">-{details.total_deletions}{filesSummary.partial ? "+" : ""}</span>
          {/if}
        </div>
        {#if detailsFailure}
          <div class="px-2 py-1">
            <div class="gp-notice" data-tone="danger" role="alert">
              <span class="min-w-0 flex-1 break-words text-meta">{failureLine(detailsFailure)}</span>
              <button type="button" class="gp-btn shrink-0" data-size="sm" onclick={retryDetails}>Try again</button>
            </div>
          </div>
        {/if}
        {#if details?.body}
          <div class="px-2 py-1">
            <MarkdownBody source={details.body} />
          </div>
        {/if}
        {#if details && details.co_authors.length > 0}
          <div class="px-2 py-1 text-meta text-textMuted select-text">Co-authors: {details.co_authors.join(", ")}</div>
        {/if}

        <!-- Local explanation of the commit. Nothing leaves the machine: the
             diff goes to a model server on loopback and the answer comes back. -->
        <div class="px-2 py-1.5">
          <button
            onclick={explainCommit}
            disabled={isExplaining || !aiReady || !currentCommitId}
            title={aiReady
              ? "Explain this commit with the local model"
              : ($harnessStore.ai?.detail ?? "No local model server is running")}
            class="gp-btn"
            data-size="sm"
          >
            {#if isExplaining}
              <Loader size={12} class="animate-spin" />
              <span>Reading the diff…</span>
            {:else}
              <Sparkles size={12} class="text-accent" />
              <span>Explain this commit</span>
            {/if}
          </button>

          {#if explainFailure}
            <div class="mt-1.5 text-meta text-danger">{failureLine(explainFailure)}</div>
          {/if}

          {#if explanation && explanationFor === currentCommitId}
            <div class="mt-2 rounded-xl bg-surface p-3 space-y-1.5">
              <div class="text-meta text-textPrimary whitespace-pre-wrap">
                {explanation.text}
              </div>
              <div class="text-micro text-textMuted">
                <span class="font-mono">{explanation.model}</span> · {explanation.context_source}
              </div>
              {#each explanation.warnings as warning}
                <div class="text-micro text-warn flex items-start gap-1">
                  <AlertTriangle size={11} class="mt-px shrink-0" />
                  <span>{warning}</span>
                </div>
              {/each}
            </div>
          {/if}
        </div>
        {#if filesSummary.note}
          <div
            class="px-2 pb-1 text-micro {filesSummary.partial ? 'text-warn' : 'text-textMuted'}"
            data-testid="commit-files-note"
          >
            {filesSummary.note}
          </div>
        {/if}
        {#each fileList as f (f.path)}
          {@const parts = formatPathParts(f.path)}
          <button
            class="gp-row w-full justify-between text-left text-xs"
            onclick={() => {
              if (currentCommitId) repoStore.selectCommitFileDiff(currentCommitId, f.path);
            }}
          >
            <div class="flex items-center gap-2 truncate">
              <LanguageLogo filePath={f.path} size={13} class="shrink-0" />
              <span class="truncate font-mono text-meta">
                {#if parts.dir}
                  <span class="text-textMuted text-micro">{parts.dir}</span>
                {/if}
                <span class="text-textPrimary font-medium">{parts.name}</span>
              </span>
            </div>
            <div class="flex items-center gap-1.5 text-micro font-mono shrink-0">
              {#if f.additions > 0}
                <span class="text-ok flex items-center"><Plus size={10} />{f.additions}</span>
              {/if}
              {#if f.deletions > 0}
                <span class="text-danger flex items-center"><Minus size={10} />{f.deletions}</span>
              {/if}
            </div>
          </button>
        {/each}
        {#if hiddenFileCount > 0}
          <button
            class="gp-btn w-full justify-center"
            data-size="sm"
            onclick={() => (fileListLimit = details!.changed_files.length)}
          >
            Show {hiddenFileCount.toLocaleString()} more changed file{hiddenFileCount === 1 ? "" : "s"}
          </button>
        {/if}
      </div>

      <div class="col-span-2 p-3 font-mono text-xs overflow-auto bg-surface flex flex-col">
        {#if diffLoading}
          <div class="flex-1" aria-label="Reading the diff…">
            <Skeleton variant="code-lines" count={8} />
          </div>
        {:else if diffFailure}
          <div class="flex flex-1 items-center justify-center px-4 font-sans">
            <div class="gp-notice" data-tone="danger" role="alert">
              <span class="min-w-0 flex-1 break-words text-meta">{failureLine(diffFailure)}</span>
              <button type="button" class="gp-btn shrink-0" data-size="sm" onclick={retryDiff}>Try again</button>
            </div>
          </div>
        {:else if previewLines.length > 0}
          <pre class="whitespace-pre text-textPrimary text-meta select-text">{previewLines.join("\n")}</pre>
          {#if previewTruncated}
            <button
              class="gp-btn mt-2 self-start font-sans"
              data-size="sm"
              onclick={() => repoStore.setActiveTab("history", "diff")}
            >
              Preview capped at {PREVIEW_LINE_CAP.toLocaleString()} lines. Open the full diff
            </button>
          {/if}
        {:else if currentCommitId !== null}
          <EmptyState
            icon={GitCommit}
            title={$repoStore.selectionKind === "commit" && $repoStore.selectedFilePath
              ? "No changes in this file"
              : "No changes in this commit"}
            compact
          />
        {:else}
          <EmptyState icon={GitCommit} title="No commit selected" hint="Select a commit to load its diff." compact />
        {/if}
      </div>
    </div>
  </div>
{/if}
