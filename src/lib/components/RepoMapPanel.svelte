<script lang="ts">
  import { repoStore } from "../stores/repoStore";
  import {
    buildDevmap,
    getCodeGraphViz,
    getDevmapCliStatus,
    getDevmapRepoMap,
    getMapPreviewViz,
    getWorkspaceLinkCandidates,
    refreshDevmap,
  } from "../codeintel/client";
  import { linkCandidatesHonesty } from "../codeintel/linkCandidates";
  import { formatCap, preferredDeadLists, roleSample } from "../codeintel/repoMap";
  import type {
    DevmapCliStatus,
    DevmapStatusPayload,
    GraphVizLoad,
    RepoMapDocument,
    RepoMapLoad,
    RepoMapSubsystem,
    WorkspaceLinksResult,
  } from "../codeintel/types";
  import {
    docsBrokenLinks,
    docsGraph,
    docsRefresh,
    docsSearch,
    type BrokenLink,
    type DocsSearchHit,
    type DocsStatus,
  } from "../docs/client";
  import {
    DOCS_SEARCH_DEFAULT_LIMIT,
    docGraphToLoad,
  } from "../docs/docGraphPayload";
  import {
    brokenLinksHonesty,
    docsSearchHonesty,
    docsStatusHonesty,
  } from "../docs/docsHonesty";
  import { formatError } from "../ui/formatError";
  import CodeGraphCanvas from "./CodeGraphCanvas.svelte";
  import ExternalToolsPanel from "./ExternalToolsPanel.svelte";
  import Skeleton from "./Skeleton.svelte";
  import { openSetupWizard } from "../tools/onboardingStore";
  import {
    classifyMapFailure,
    mapFailureMessage,
  } from "../tools/externalTools";

  /**
   * Code → Map: navigate the consumer `repo_map.json`, draw the code /
   * subsystem / doc graph canvas, search repo docs, and list cross-repo
   * link candidates from the workspace registry.
   *
   * Subsystems, entry points, critical files, and role_files.tests — with
   * `role_file_counts` and `liveness_meta` honesty so a capped sample never
   * reads as the whole inventory. Graph legends state
   * `counts.nodes_truncated` the same way. Freshness comes from `cmd_devmap_status`.
   */

  type MapView =
    | "navigator"
    | "files"
    | "symbols"
    | "subsystems"
    | "docs"
    | "docgraph"
    | "links";

  const CODE_GRAPH_VIEWS = new Set<MapView>(["files", "symbols", "subsystems", "docgraph"]);
  const BROKEN_LINKS_DISPLAY_CAP = 200;

  let load = $state<RepoMapLoad | null>(null);
  let cliStatus = $state<DevmapCliStatus | null>(null);
  let selectedArea = $state<string | null>(null);
  let loading = $state(false);
  let building = $state(false);
  let errorMsg = $state<string | null>(null);
  let actionNote = $state<string | null>(null);
  let mapView = $state<MapView>("navigator");
  let graphLoad = $state<GraphVizLoad | null>(null);
  let graphLoading = $state(false);

  let docsStatus = $state<DocsStatus | null>(null);
  let docsQuery = $state("");
  let docsHits = $state<DocsSearchHit[]>([]);
  let docsSearching = $state(false);
  let docsError = $state<string | null>(null);
  let brokenLinks = $state<BrokenLink[]>([]);
  let brokenLoading = $state(false);

  let linkResult = $state<WorkspaceLinksResult | null>(null);
  let linksLoading = $state(false);
  let linksError = $state<string | null>(null);

  const map = $derived(load?.available ? load.map ?? null : null);
  const statusPayload = $derived(
    (cliStatus?.available ? cliStatus.status : null) as DevmapStatusPayload | null,
  );
  const mapFailureMode = $derived(
    classifyMapFailure({
      cliAvailable: cliStatus?.available ?? null,
      cliReason: cliStatus?.reason,
      mapAvailable: load?.available ?? null,
      mapReason: load?.reason,
      mapPath: load?.path,
    }),
  );
  const mapFailureText = $derived(
    !load?.available || (cliStatus && !cliStatus.available)
      ? mapFailureMessage(mapFailureMode, errorMsg ?? load?.reason ?? cliStatus?.reason)
      : null,
  );
  const cliKnownAbsent = $derived(cliStatus != null && !cliStatus.available);
  const selected = $derived.by((): RepoMapSubsystem | null => {
    if (!map) return null;
    if (selectedArea) {
      return map.subsystems.find((s) => s.area === selectedArea) ?? map.subsystems[0] ?? null;
    }
    return map.subsystems[0] ?? null;
  });
  const deadLists = $derived(map ? preferredDeadLists(map) : null);
  const tests = $derived(selected ? roleSample(selected, "tests") : null);
  const docsHonesty = $derived(docsStatusHonesty(docsStatus));
  const searchHonesty = $derived(docsSearchHonesty(docsHits, DOCS_SEARCH_DEFAULT_LIMIT));
  const brokenDisplay = $derived(brokenLinksHonesty(brokenLinks, BROKEN_LINKS_DISPLAY_CAP));
  const linksHonesty = $derived(linkCandidatesHonesty(linkResult));

  let inflightPath = "";
  let graphInflight = "";
  let docsInflight = "";
  let linksInflight = "";

  async function reload(path?: string) {
    const repoPath = path ?? $repoStore.currentPath;
    if (!repoPath) return;
    loading = true;
    errorMsg = null;
    actionNote = null;
    inflightPath = repoPath;
    const [mapResult, statusResult] = await Promise.allSettled([
      getDevmapRepoMap(repoPath),
      getDevmapCliStatus(repoPath),
    ]);
    if (inflightPath !== repoPath || $repoStore.currentPath !== repoPath) return;
    loading = false;
    if (mapResult.status === "fulfilled") {
      load = mapResult.value;
      if (!mapResult.value.available) {
        errorMsg = mapResult.value.reason ?? "repo map unavailable";
      } else if (selectedArea == null && mapResult.value.map?.subsystems[0]) {
        selectedArea = mapResult.value.map.subsystems[0].area;
      }
    } else {
      load = null;
      errorMsg = formatError(mapResult.reason);
    }
    if (statusResult.status === "fulfilled") {
      cliStatus = statusResult.value;
    } else {
      cliStatus = null;
    }
    if (CODE_GRAPH_VIEWS.has(mapView)) {
      void reloadGraph(repoPath);
    } else if (mapView === "docs") {
      void reloadDocs(repoPath);
    } else if (mapView === "links") {
      void reloadLinks(repoPath);
    }
  }

  async function reloadGraph(path?: string) {
    const repoPath = path ?? $repoStore.currentPath;
    if (!repoPath || !CODE_GRAPH_VIEWS.has(mapView)) return;
    graphLoading = true;
    graphInflight = `${repoPath}:${mapView}`;
    try {
      let result: GraphVizLoad;
      if (mapView === "files") {
        result = await getCodeGraphViz(repoPath, false);
      } else if (mapView === "symbols") {
        result = await getCodeGraphViz(repoPath, true);
      } else if (mapView === "docgraph") {
        const graph = await docsGraph(repoPath);
        result = docGraphToLoad(graph, null);
      } else {
        result = await getMapPreviewViz(repoPath);
      }
      if (graphInflight !== `${repoPath}:${mapView}`) return;
      graphLoad = result;
    } catch (err) {
      if (graphInflight !== `${repoPath}:${mapView}`) return;
      graphLoad = {
        available: false,
        reason: formatError(err),
        kind:
          mapView === "subsystems"
            ? "map_preview"
            : mapView === "docgraph"
              ? "doc_graph"
              : "code_graph",
        payload: null,
      };
    } finally {
      if (graphInflight === `${repoPath}:${mapView}`) graphLoading = false;
    }
  }

  async function reloadDocs(path?: string) {
    const repoPath = path ?? $repoStore.currentPath;
    if (!repoPath) return;
    docsInflight = repoPath;
    docsError = null;
    brokenLoading = true;
    try {
      const status = await docsRefresh(repoPath);
      if (docsInflight !== repoPath) return;
      docsStatus = status;
      brokenLinks = await docsBrokenLinks(repoPath);
      if (docsInflight !== repoPath) return;
    } catch (err) {
      if (docsInflight !== repoPath) return;
      docsError = formatError(err);
      docsStatus = null;
      brokenLinks = [];
    } finally {
      if (docsInflight === repoPath) brokenLoading = false;
    }
  }

  async function runDocsSearch() {
    const repoPath = $repoStore.currentPath;
    const q = docsQuery.trim();
    if (!repoPath || !q) {
      docsHits = [];
      return;
    }
    docsSearching = true;
    docsError = null;
    try {
      docsHits = await docsSearch(repoPath, q, DOCS_SEARCH_DEFAULT_LIMIT);
    } catch (err) {
      docsHits = [];
      docsError = formatError(err);
    } finally {
      docsSearching = false;
    }
  }

  async function reloadLinks(path?: string) {
    const registryRoot = path ?? $repoStore.currentPath;
    if (!registryRoot) return;
    linksLoading = true;
    linksError = null;
    linksInflight = registryRoot;
    try {
      const result = await getWorkspaceLinkCandidates(registryRoot);
      if (linksInflight !== registryRoot) return;
      linkResult = result;
    } catch (err) {
      if (linksInflight !== registryRoot) return;
      linkResult = null;
      linksError = formatError(err);
    } finally {
      if (linksInflight === registryRoot) linksLoading = false;
    }
  }

  async function runBuild(kind: "build" | "refresh") {
    const repoPath = $repoStore.currentPath;
    if (!repoPath) return;
    building = true;
    actionNote = null;
    errorMsg = null;
    try {
      const outcome = kind === "build" ? await buildDevmap(repoPath) : await refreshDevmap(repoPath);
      if (!outcome.ok) {
        actionNote = outcome.stderr.trim() || outcome.stdout.trim() || `devmap ${kind} failed`;
      } else {
        actionNote = `Map ${kind === "build" ? "built" : "refreshed"}.`;
      }
      await reload(repoPath);
    } catch (err) {
      actionNote = formatError(err);
    } finally {
      building = false;
    }
  }

  $effect(() => {
    const path = $repoStore.currentPath;
    if (!path) {
      load = null;
      cliStatus = null;
      selectedArea = null;
      graphLoad = null;
      docsStatus = null;
      docsHits = [];
      brokenLinks = [];
      linkResult = null;
      return;
    }
    void reload(path);
  });

  $effect(() => {
    const view = mapView;
    const path = $repoStore.currentPath;
    if (!path) return;
    if (CODE_GRAPH_VIEWS.has(view)) {
      void reloadGraph(path);
    } else if (view === "docs") {
      void reloadDocs(path);
    } else if (view === "links") {
      void reloadLinks(path);
    }
  });

  function coverageGapSummary(payload: DevmapStatusPayload | null): string | null {
    const gaps = payload?.coverage_gaps;
    if (!gaps || typeof gaps !== "object") return null;
    const parts: string[] = [];
    for (const [key, value] of Object.entries(gaps)) {
      const n = Array.isArray(value)
        ? value.length
        : typeof value === "number"
          ? value
          : value && typeof value === "object" && "length" in value
            ? Number((value as { length: unknown }).length)
            : null;
      if (n != null && n > 0) parts.push(`${key}: ${n}`);
      else if (value && !Array.isArray(value) && typeof value === "object") {
        const count = Object.keys(value as object).length;
        if (count > 0) parts.push(`${key}: ${count}`);
      }
    }
    return parts.length > 0 ? parts.join(" · ") : null;
  }

  function selectSubsystem(area: string) {
    selectedArea = area;
  }

  function openFile(path: string) {
    repoStore.selectFilePath(path);
    repoStore.setActiveTab("code", "explorer");
  }

  function openGraphNode(path: string, _nodeId: string) {
    // File paths open in the explorer; bare symbol ids are not files.
    if (
      path.includes("/") ||
      /\.(rs|ts|tsx|js|svelte|py|go|md|mdx)$/.test(path)
    ) {
      openFile(path.split("::")[0] ?? path);
    }
  }

  function setView(view: MapView) {
    mapView = view;
  }
</script>

<div class="flex-1 flex flex-col min-h-0 font-sans text-sm text-textPrimary">
  <!-- Freshness strip from cmd_devmap_status -->
  <div
    class="shrink-0 flex flex-wrap items-center gap-x-3 gap-y-1 border-b border-border/60 px-3 py-2 text-[11px] text-textMuted"
    role="status"
  >
    {#if loading && !map}
      <span>Loading map…</span>
    {:else if statusPayload}
      <span
        class={statusPayload.is_fresh
          ? "text-emerald-600 dark:text-emerald-400"
          : "text-amber-600 dark:text-amber-300"}
        title="devmap status is_fresh"
      >
        {statusPayload.is_fresh ? "Fresh" : "Stale"}
      </span>
      <span title="generation_id">gen {statusPayload.generation_id ?? "—"}</span>
      {#if statusPayload.schema_outdated}
        <span class="text-rose-600 dark:text-rose-400" title="schema_outdated">
          schema outdated ({statusPayload.schema_version ?? "?"} → {statusPayload.expected_schema_version ?? "?"})
        </span>
      {:else if statusPayload.schema_version != null}
        <span>schema {statusPayload.schema_version}</span>
      {/if}
      {#if statusPayload.node_count != null}
        <span>{statusPayload.node_count} nodes · {statusPayload.edge_count ?? "?"} edges</span>
      {/if}
      {#if coverageGapSummary(statusPayload)}
        <span class="text-amber-600 dark:text-amber-300" title="coverage_gaps">
          gaps: {coverageGapSummary(statusPayload)}
        </span>
      {/if}
    {:else if cliStatus && !cliStatus.available}
      <span class="text-amber-600 dark:text-amber-300">{mapFailureText ?? cliStatus.reason ?? "devmap CLI missing"}</span>
    {:else if load && !load.available}
      <span class="text-amber-600 dark:text-amber-300">{mapFailureText ?? load.reason ?? "map unavailable"}</span>
    {:else}
      <span>Status unknown</span>
    {/if}
    <span class="ml-auto flex items-center gap-1.5">
      <button
        type="button"
        class="gp-btn text-[11px] px-2 py-0.5"
        disabled={building || !$repoStore.currentPath || cliKnownAbsent}
        title={cliKnownAbsent ? "devmap CLI is not installed — run Setup first" : "Refresh the map index"}
        onclick={() => void runBuild("refresh")}
      >
        Refresh
      </button>
      <button
        type="button"
        class="gp-btn text-[11px] px-2 py-0.5"
        disabled={building || !$repoStore.currentPath || cliKnownAbsent}
        title={cliKnownAbsent ? "devmap CLI is not installed — run Setup first" : "Build the map index"}
        onclick={() => void runBuild("build")}
      >
        Build
      </button>
    </span>
  </div>

  {#if cliStatus && !cliStatus.available}
    <div class="shrink-0 border-b border-border/50 px-3 py-2 space-y-2">
      <p class="text-[11px] text-amber-600 dark:text-amber-400">{mapFailureMessage("no_cli")}</p>
      <button
        type="button"
        class="gp-btn text-[11px] px-2 py-0.5"
        onclick={() => openSetupWizard("devmap", "explain")}
      >
        Set up devmap
      </button>
      <ExternalToolsPanel
        only="devmap"
        compact
        onInstalled={() => {
          void reload();
        }}
      />
    </div>
  {:else if load && !load.available && mapFailureMode !== "other"}
    <div class="shrink-0 border-b border-border/50 px-3 py-2">
      <p class="text-[11px] text-amber-600 dark:text-amber-400">{mapFailureText}</p>
    </div>
  {/if}

  <!-- View switcher: navigator vs canvases vs docs vs links -->
  <div
    class="shrink-0 flex flex-wrap items-center gap-1 border-b border-border/50 px-2 py-1 text-[11px]"
    role="tablist"
    aria-label="Map views"
  >
    <button
      type="button"
      role="tab"
      aria-selected={mapView === "navigator"}
      class="rounded px-2 py-0.5 {mapView === 'navigator' ? 'bg-accent/15 text-textPrimary' : 'text-textMuted'}"
      onclick={() => setView("navigator")}
    >
      Navigator
    </button>
    <button
      type="button"
      role="tab"
      aria-selected={mapView === "files"}
      class="rounded px-2 py-0.5 {mapView === 'files' ? 'bg-accent/15 text-textPrimary' : 'text-textMuted'}"
      onclick={() => setView("files")}
      title="File import graph (ranked cap)"
    >
      Files
    </button>
    <button
      type="button"
      role="tab"
      aria-selected={mapView === "symbols"}
      class="rounded px-2 py-0.5 {mapView === 'symbols' ? 'bg-accent/15 text-textPrimary' : 'text-textMuted'}"
      onclick={() => setView("symbols")}
      title="Symbol call graph (ranked cap)"
    >
      Symbols
    </button>
    <button
      type="button"
      role="tab"
      aria-selected={mapView === "subsystems"}
      class="rounded px-2 py-0.5 {mapView === 'subsystems' ? 'bg-accent/15 text-textPrimary' : 'text-textMuted'}"
      onclick={() => setView("subsystems")}
      title="Subsystem handoff map"
    >
      Subsystems
    </button>
    <button
      type="button"
      role="tab"
      aria-selected={mapView === "docgraph"}
      class="rounded px-2 py-0.5 {mapView === 'docgraph' ? 'bg-accent/15 text-textPrimary' : 'text-textMuted'}"
      onclick={() => setView("docgraph")}
      title="Markdown doc link graph (same free-2D canvas)"
    >
      Doc graph
    </button>
    <button
      type="button"
      role="tab"
      aria-selected={mapView === "docs"}
      class="rounded px-2 py-0.5 {mapView === 'docs' ? 'bg-accent/15 text-textPrimary' : 'text-textMuted'}"
      onclick={() => setView("docs")}
      title="Repo markdown search and broken links"
    >
      Docs
    </button>
    <button
      type="button"
      role="tab"
      aria-selected={mapView === "links"}
      class="rounded px-2 py-0.5 {mapView === 'links' ? 'bg-accent/15 text-textPrimary' : 'text-textMuted'}"
      onclick={() => setView("links")}
      title="Cross-repo import link candidates from the workspace registry"
    >
      Links
    </button>
  </div>

  {#if actionNote}
    <div class="shrink-0 px-3 py-1.5 text-[11px] text-textMuted border-b border-border/40" role="status">
      {actionNote}
    </div>
  {/if}

  {#if CODE_GRAPH_VIEWS.has(mapView)}
    <div class="flex-1 min-h-0 flex flex-col">
      <CodeGraphCanvas load={graphLoad} loading={graphLoading} onOpenNode={openGraphNode} />
    </div>
  {:else if mapView === "docs"}
    <div class="flex-1 min-h-0 overflow-y-auto p-3 space-y-4" data-testid="docs-panel">
      {#if docsStatus}
        <p class="text-[11px] text-textMuted" role="status">
          {docsStatus.noteCount} note{docsStatus.noteCount === 1 ? "" : "s"} indexed
        </p>
      {/if}
      {#if docsHonesty}
        <p class="text-[10px] text-amber-600 dark:text-amber-300" role="note" data-testid="docs-status-honesty">
          {docsHonesty}
        </p>
      {/if}
      {#if docsError}
        <p class="text-[11px] text-rose-600 dark:text-rose-400">{docsError}</p>
      {/if}

      <section aria-labelledby="docs-search-heading">
        <h3 id="docs-search-heading" class="text-[10px] uppercase tracking-wide text-textMuted mb-1.5">
          Search docs
        </h3>
        <form
          class="flex gap-2 items-center"
          onsubmit={(e) => {
            e.preventDefault();
            void runDocsSearch();
          }}
        >
          <input
            type="search"
            class="gp-input flex-1 text-[12px] py-1 px-2"
            placeholder="Full-text markdown search…"
            bind:value={docsQuery}
            aria-label="Search repository markdown"
          />
          <button type="submit" class="gp-btn text-[11px] px-2 py-1" disabled={docsSearching || !$repoStore.currentPath}>
            {docsSearching ? "Searching…" : "Search"}
          </button>
        </form>
        {#if searchHonesty}
          <p class="mt-1 text-[10px] text-amber-600 dark:text-amber-300" data-testid="docs-search-honesty">
            {searchHonesty}
          </p>
        {/if}
        {#if docsHits.length === 0 && docsQuery.trim() && !docsSearching}
          <p class="mt-2 text-[11px] text-textMuted">No hits.</p>
        {:else if docsHits.length > 0}
          <ul class="mt-2 space-y-1.5">
            {#each docsHits as hit (hit.path + ":" + hit.line + ":" + hit.score)}
              <li>
                <button
                  type="button"
                  class="w-full text-left rounded-md px-2 py-1.5 hover:bg-surface/80"
                  onclick={() => openFile(hit.path)}
                >
                  <span class="font-mono text-[11px] text-accent block truncate">{hit.path}:{hit.line}</span>
                  <span class="text-[11px] text-textPrimary block truncate">{hit.title}</span>
                  <span class="text-[10px] text-textMuted block truncate">{hit.context}</span>
                </button>
              </li>
            {/each}
          </ul>
        {/if}
      </section>

      <section aria-labelledby="docs-broken-heading" class="border-t border-border/50 pt-3">
        <h3 id="docs-broken-heading" class="text-[10px] uppercase tracking-wide text-textMuted mb-1.5">
          Broken links
          {#if !brokenLoading}
            <span class="normal-case tracking-normal ml-1">({brokenLinks.length})</span>
          {/if}
        </h3>
        {#if brokenLoading}
          <p class="text-[11px] text-textMuted">Loading…</p>
        {:else if brokenLinks.length === 0}
          <p class="text-[11px] text-textMuted">None found in the indexed vault.</p>
        {:else}
          {#if brokenDisplay.honesty}
            <p class="mb-1 text-[10px] text-amber-600 dark:text-amber-300" data-testid="docs-broken-honesty">
              {brokenDisplay.honesty}
            </p>
          {/if}
          <ul class="space-y-0.5 max-h-64 overflow-y-auto">
            {#each brokenDisplay.shown as link (`${link.source}->${link.target}`)}
              <li class="text-[11px]">
                <button
                  type="button"
                  class="font-mono text-accent hover:underline"
                  onclick={() => openFile(link.source)}
                >
                  {link.source}
                </button>
                <span class="text-textMuted"> → </span>
                <span class="font-mono text-textMuted">{link.target}</span>
              </li>
            {/each}
          </ul>
        {/if}
      </section>
    </div>
  {:else if mapView === "links"}
    <div class="flex-1 min-h-0 overflow-y-auto p-3 space-y-3" data-testid="link-candidates-panel">
      <header>
        <h3 class="text-[10px] uppercase tracking-wide text-textMuted">Cross-repo link candidates</h3>
        <p class="text-[11px] text-textMuted mt-0.5">
          Import-graph edges between registered workspace repos — not name-matched call edges.
        </p>
      </header>
      {#if linksHonesty}
        <p class="text-[10px] text-textMuted" role="status" data-testid="link-candidates-honesty">
          {linksHonesty}
        </p>
      {/if}
      {#if linksError}
        <p class="text-[11px] text-rose-600 dark:text-rose-400">{linksError}</p>
      {/if}
      {#if linksLoading}
        <p class="text-[11px] text-textMuted">Loading…</p>
      {:else if linkResult && linkResult.links.length > 0}
        <ul class="space-y-1.5">
          {#each linkResult.links as link (`${link.from_repo}:${link.from_file}:${link.module_specifier}:${link.to_repo}`)}
            <li class="rounded-md border border-border/40 px-2 py-1.5 text-[11px]">
              <div class="font-mono text-textPrimary truncate">
                [{link.from_repo}] {link.from_file}
              </div>
              <div class="text-textMuted mt-0.5">
                <span class="font-mono">{link.module_specifier}</span>
                <span> → </span>
                <span class="font-mono text-accent">[{link.to_repo}]</span>
              </div>
              {#if link.evidence}
                <div class="text-[10px] text-textMuted mt-0.5 truncate" title={link.evidence}>
                  {link.evidence}
                </div>
              {/if}
            </li>
          {/each}
        </ul>
      {/if}
      <button
        type="button"
        class="gp-btn text-[11px] px-2 py-1"
        disabled={linksLoading || !$repoStore.currentPath}
        onclick={() => void reloadLinks()}
      >
        Refresh links
      </button>
    </div>
  {:else if loading && !map}
    <div class="flex-1 p-4 flex flex-col gap-3" aria-busy="true">
      <Skeleton variant="text" width="40%" height="1rem" />
      <Skeleton variant="card" />
      <Skeleton variant="text" count={5} />
    </div>
  {:else if !map}
    <div class="flex-1 flex flex-col items-center justify-center gap-2 p-6 text-center" role="status">
      <p class="text-sm text-textPrimary">No repository map</p>
      <p class="text-[11px] text-textMuted max-w-md">
        {errorMsg ?? "Build the code map to navigate subsystems, entry points and area tests."}
      </p>
      <button
        type="button"
        class="gp-btn mt-2"
        disabled={building || !$repoStore.currentPath}
        onclick={() => void runBuild("build")}
      >
        Build Map
      </button>
    </div>
  {:else}
    {@const doc = map as RepoMapDocument}
    <div class="flex-1 flex min-h-0">
      <!-- Subsystem list -->
      <nav
        class="w-56 shrink-0 border-r border-border/60 overflow-y-auto"
        aria-label="Subsystems"
      >
        <div class="px-2.5 py-2 text-[10px] uppercase tracking-wide text-textMuted">
          Subsystems
          <span class="normal-case tracking-normal ml-1">
            {formatCap(
              doc.liveness_meta.subsystems.shown,
              doc.liveness_meta.subsystems.total,
              doc.liveness_meta.subsystems.truncated,
            )}
          </span>
        </div>
        <ul class="px-1 pb-2">
          {#each doc.subsystems as sub (sub.area)}
            <li>
              <button
                type="button"
                class="w-full text-left rounded-md px-2 py-1.5 text-[12px] hover:bg-surface/80 {selected?.area === sub.area
                  ? 'bg-accent/15 text-textPrimary'
                  : 'text-textMuted'}"
                onclick={() => selectSubsystem(sub.area)}
              >
                <span class="font-mono block truncate" title={sub.area}>{sub.area}</span>
                <span class="block truncate text-[10px] opacity-80" title={sub.summary}>{sub.summary}</span>
              </button>
            </li>
          {/each}
        </ul>
        {#if doc.liveness_meta.subsystems.truncated}
          <p class="px-2.5 pb-2 text-[10px] text-amber-600 dark:text-amber-300">
            Showing {doc.liveness_meta.subsystems.shown} of {doc.liveness_meta.subsystems.total}
            {#if doc.liveness_meta.subsystems.dropped_no_area > 0}
              · {doc.liveness_meta.subsystems.dropped_no_area} dropped (no area)
            {/if}
          </p>
        {/if}
      </nav>

      <!-- Detail -->
      <div class="flex-1 min-w-0 overflow-y-auto p-3 space-y-4">
        {#if selected}
          <header>
            <h2 class="font-mono text-[13px] text-textPrimary">{selected.area}</h2>
            <p class="text-[11px] text-textMuted mt-0.5">{selected.summary}</p>
          </header>

          <section aria-labelledby="map-entry-points">
            <h3 id="map-entry-points" class="text-[10px] uppercase tracking-wide text-textMuted mb-1">
              Entry points
            </h3>
            {#if selected.entry_points.length === 0}
              <p class="text-[11px] text-textMuted">None listed.</p>
            {:else}
              <ul class="space-y-0.5">
                {#each selected.entry_points as path (path)}
                  <li>
                    <button
                      type="button"
                      class="font-mono text-[11px] text-accent hover:underline truncate max-w-full"
                      onclick={() => openFile(path)}
                    >
                      {path}
                    </button>
                  </li>
                {/each}
              </ul>
            {/if}
          </section>

          <section aria-labelledby="map-critical">
            <h3 id="map-critical" class="text-[10px] uppercase tracking-wide text-textMuted mb-1">
              Critical files
            </h3>
            {#if selected.critical_files.length === 0}
              <p class="text-[11px] text-textMuted">None listed.</p>
            {:else}
              <ul class="space-y-0.5">
                {#each selected.critical_files as path (path)}
                  <li>
                    <button
                      type="button"
                      class="font-mono text-[11px] text-accent hover:underline truncate max-w-full"
                      onclick={() => openFile(path)}
                    >
                      {path}
                    </button>
                  </li>
                {/each}
              </ul>
            {/if}
          </section>

          <section aria-labelledby="map-tests">
            <h3 id="map-tests" class="text-[10px] uppercase tracking-wide text-textMuted mb-1">
              Tests
              {#if tests}
                <span class="normal-case tracking-normal ml-1">
                  {formatCap(tests.paths.length, tests.total, tests.truncated)}
                </span>
              {/if}
            </h3>
            {#if !tests || tests.paths.length === 0}
              <p class="text-[11px] text-textMuted">
                {tests && tests.total > 0
                  ? `Sample empty; role_file_counts says ${tests.total} test file(s).`
                  : "No test role files in this area."}
              </p>
            {:else}
              <ul class="space-y-0.5">
                {#each tests.paths as path (path)}
                  <li>
                    <button
                      type="button"
                      class="font-mono text-[11px] text-accent hover:underline truncate max-w-full"
                      onclick={() => openFile(path)}
                    >
                      {path}
                    </button>
                  </li>
                {/each}
              </ul>
              {#if tests.truncated}
                <p class="mt-1 text-[10px] text-amber-600 dark:text-amber-300">
                  Sample of {tests.paths.length}; {tests.total} total for this area.
                </p>
              {/if}
            {/if}
          </section>

          {#if selected.neighbors.length > 0}
            <section aria-labelledby="map-neighbors">
              <h3 id="map-neighbors" class="text-[10px] uppercase tracking-wide text-textMuted mb-1">
                Neighbors
              </h3>
              <p class="font-mono text-[11px] text-textMuted">
                {selected.neighbors.join(" · ")}
              </p>
            </section>
          {/if}
        {/if}

        <!-- Dead / unwired candidates (prefer over unreachable) -->
        {#if deadLists}
          <section class="border-t border-border/50 pt-3 space-y-3" aria-label="Dead and unwired candidates">
            <div>
              <h3 class="text-[10px] uppercase tracking-wide text-textMuted mb-1">
                Unwired candidates
                <span class="normal-case tracking-normal ml-1">
                  {formatCap(
                    doc.liveness_meta.unwired.shown,
                    doc.liveness_meta.unwired.total,
                    doc.liveness_meta.unwired.truncated,
                  )}
                </span>
              </h3>
              {#if deadLists.unwired.length === 0}
                <p class="text-[11px] text-textMuted">None in this sample.</p>
              {:else}
                <ul class="space-y-0.5 max-h-32 overflow-y-auto">
                  {#each deadLists.unwired as path (path)}
                    <li>
                      <button
                        type="button"
                        class="font-mono text-[11px] text-textMuted hover:text-accent truncate max-w-full"
                        onclick={() => openFile(path)}
                      >
                        {path}
                      </button>
                    </li>
                  {/each}
                </ul>
              {/if}
              {#if doc.liveness_meta.unwired.excluded_coverage_loss > 0 || doc.liveness_meta.unwired.excluded_import_blind > 0}
                <p class="mt-1 text-[10px] text-textMuted">
                  Excluded: coverage loss {doc.liveness_meta.unwired.excluded_coverage_loss},
                  import-blind {doc.liveness_meta.unwired.excluded_import_blind}
                </p>
              {/if}
            </div>
            <div>
              <h3 class="text-[10px] uppercase tracking-wide text-textMuted mb-1">
                Dead symbol candidates
                <span class="normal-case tracking-normal ml-1">
                  {formatCap(
                    doc.liveness_meta.dead_symbol.shown,
                    doc.liveness_meta.dead_symbol.total,
                    doc.liveness_meta.dead_symbol.truncated,
                  )}
                </span>
              </h3>
              {#if deadLists.deadSymbols.length === 0}
                <p class="text-[11px] text-textMuted">None in this sample.</p>
              {:else}
                <ul class="space-y-0.5 max-h-40 overflow-y-auto">
                  {#each deadLists.deadSymbols as id (id)}
                    <li class="font-mono text-[11px] text-textMuted truncate" title={id}>{id}</li>
                  {/each}
                </ul>
              {/if}
            </div>
            {#if deadLists.unreachableSuppressed}
              <p class="text-[10px] text-amber-600 dark:text-amber-300">
                Unreachable files ignored — liveness_unreachable_unreliable is set.
                Prefer unwired and dead-symbol candidates above.
              </p>
            {:else if deadLists.unreachable.length > 0}
              <div>
                <h3 class="text-[10px] uppercase tracking-wide text-textMuted mb-1">
                  Unreachable files (secondary)
                </h3>
                <ul class="space-y-0.5 max-h-24 overflow-y-auto">
                  {#each deadLists.unreachable as path (path)}
                    <li class="font-mono text-[11px] text-textMuted truncate">{path}</li>
                  {/each}
                </ul>
              </div>
            {/if}
          </section>
        {/if}

        {#if doc.liveness_meta.entry_roots.truncated || doc.liveness_meta.subsystems.role_files_truncated}
          <p class="text-[10px] text-textMuted border-t border-border/40 pt-2">
            Caps:
            entry roots {formatCap(
              doc.liveness_meta.entry_roots.shown,
              doc.liveness_meta.entry_roots.total,
              doc.liveness_meta.entry_roots.truncated,
            )}
            · role files {formatCap(
              doc.liveness_meta.subsystems.role_files_shown,
              doc.liveness_meta.subsystems.role_files_total,
              doc.liveness_meta.subsystems.role_files_truncated,
            )}
            · neighbors {formatCap(
              doc.liveness_meta.subsystems.neighbors_shown,
              doc.liveness_meta.subsystems.neighbors_total,
              doc.liveness_meta.subsystems.neighbors_truncated,
            )}
            · handoffs {formatCap(
              doc.liveness_meta.subsystems.handoff_paths_shown,
              doc.liveness_meta.subsystems.handoff_paths_total,
              doc.liveness_meta.subsystems.handoff_paths_truncated,
            )}
          </p>
        {/if}
      </div>
    </div>
  {/if}
</div>
