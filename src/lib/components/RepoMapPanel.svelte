<script module lang="ts">
  import { createRepoPanelCache } from "../panels/repoPanelCache";

  /**
   * What Map → Docs search shows for a repository, kept past the panel's
   * unmount so opening a hit and coming back finds the query and its hits
   * where they were. Restored without re-running the search.
   */
  interface DocsSearchMemo {
    docsQuery: string;
    /** The query `docsHits` answer; null when no search has completed. */
    searchedQuery: string | null;
    docsHits: import("../docs/client").DocsSearchHit[];
  }
  const docsSearchMemo = createRepoPanelCache<DocsSearchMemo>();

  /**
   * Apply the repo map when its own read settles. `cmd_devmap_status` keeps
   * the 30s probe deadline and must not hold the map: the panel was staying
   * on "Loading map…" until both reads finished.
   *
   * A reload that is no longer current applies neither result. The newer
   * reload owns `loading` and is the one that clears it.
   */
  export function settleMapBeforeStatus<M, S>(input: {
    map: Promise<M>;
    status: Promise<S>;
    isCurrent: () => boolean;
    onMap: (result: PromiseSettledResult<M>) => void;
    onStatus: (result: PromiseSettledResult<S>) => void;
  }): Promise<void> {
    const statusSettled: Promise<PromiseSettledResult<S>> = input.status.then(
      (value) => ({ status: "fulfilled", value }),
      (reason) => ({ status: "rejected", reason }),
    );
    void statusSettled.then((result) => {
      if (!input.isCurrent()) return;
      input.onStatus(result);
    });
    return input.map.then(
      (value) => {
        if (!input.isCurrent()) return;
        input.onMap({ status: "fulfilled", value });
      },
      (reason: unknown) => {
        if (!input.isCurrent()) return;
        input.onMap({ status: "rejected", reason });
      },
    );
  }
</script>

<script lang="ts">
  import { onDestroy, untrack } from "svelte";
  import { crossfade } from "svelte/transition";
  import { beginGeneration } from "../async/guard";
  import { reportPanelError } from "../diagnostics/report";
  import { paneDetails } from "../diagnostics/paneCrash";
  import { keyedList } from "../ui/eachKeys";
  import { currentRepoPath, repoStore } from "../stores/repoStore";
  import { liveIndex } from "../codeintel/liveIndex";
  import { buildBlockReason } from "../codeintel/buildBlock";
  import { bindHorizontalWheel, scrollChildIntoHorizontalView } from "../dom/overflowHint";
  import { focusTabAt, handleTablistKeydown, panelId, tabId, tabProps } from "../dom/tablist";
  import { Loader2, Network } from "@lucide/svelte";
  import EmptyState from "./EmptyState.svelte";
  import { isMacOS } from "../platform";
  import { liquidSelection } from "../ui/transitions";
  import { requestReveal } from "../files/revealRequests";
  import { copyDevmapLogs } from "../codeintel/diagnostics";
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
  import { boundText } from "../codeintel/walkIncomplete";
  import {
    coverageGapSummary,
    formatCap,
    preferredDeadLists,
    roleSample,
    unwiredExclusionSummary,
  } from "../codeintel/repoMap";
  import {
    buildAreaTree,
    summaryWithoutArea,
    ancestorKeys,
    neighborRowKey,
    NO_KEYS,
    subsystemNodeId,
    treeKeyOutcome,
    visibleAreaRows,
  } from "../codeintel/codeMap";
  import { tick } from "svelte";
  import { ChevronDown, ChevronRight } from "@lucide/svelte";
  import type {
    DevmapCliStatus,
    DevmapStatusPayload,
    GraphVizLoad,
    RepoMapDocument,
    RepoMapLoad,
    RepoMapSubsystem,
    WorkspaceLinksResult,
  } from "../codeintel/types";
  import { buildFailureReason } from "../codeintel/types";
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
    brokenLinksSection,
    docsHitsStale,
    docsSearchHonesty,
    docsSearchPrompt,
    docsStatusHonesty,
  } from "../docs/docsHonesty";
  import { formatError } from "../ui/formatError";
  import CodeGraphCanvas from "./CodeGraphCanvas.svelte";
  import ExternalToolsPanel from "./ExternalToolsPanel.svelte";
  import ScrollCue from "./ScrollCue.svelte";
  import Skeleton from "./Skeleton.svelte";
  import { openSetupWizard } from "../tools/onboardingStore";
  import {
    classifyMapFailure,
    mapFailureMessage,
  } from "../tools/externalTools";

  const macos = isMacOS();
  // Same glass pill the header's section tabs glide. Labels stay put.
  const [sendSelection, receiveSelection] = crossfade(liquidSelection());

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

  const MAP_VIEWS: ReadonlyArray<{ id: MapView; label: string; title?: string }> = [
    { id: "navigator", label: "Navigator" },
    { id: "files", label: "Files", title: "File import graph (ranked cap)" },
    { id: "symbols", label: "Symbols", title: "Symbol call graph (ranked cap)" },
    { id: "subsystems", label: "Subsystems", title: "Subsystem handoff map" },
    { id: "docgraph", label: "Doc graph", title: "Markdown doc link graph (same free-2D canvas)" },
    { id: "docs", label: "Docs", title: "Repository markdown search and broken links" },
    { id: "links", label: "Links", title: "Cross-repository import link candidates from the workspace registry" },
  ];

  const CODE_GRAPH_VIEWS = new Set<MapView>(["files", "symbols", "subsystems", "docgraph"]);
  const BROKEN_LINKS_DISPLAY_CAP = 200;
  const SETUP_TOOL = "devmap" as const;

  /**
   * Clip a process's own output to what a status strip can hold.
   *
   * `devmap`'s stderr is the source of most reasons on this pane, and stderr
   * has no length anyone here controls — a failed build's is a log. The
   * ellipsis `boundText` appends is what tells the reader a tail was cut; the
   * full text stays in the logs the Copy-logs button collects.
   */
  const STRIP_MAX_CHARS = 240;
  const boundStrip = (text: string | null | undefined): string | null =>
    text == null ? null : boundText(text, STRIP_MAX_CHARS);

  let load = $state<RepoMapLoad | null>(null);
  let cliStatus = $state<DevmapCliStatus | null>(null);
  // Navigator selection is a tree row key, not an area string: two subsystems
  // can carry one area, and the tree merges them into one row.
  let selectedKey = $state<string | null>(null);
  let treeFocusKey = $state<string | null>(null);
  let treeFilter = $state("");
  let collapsed = $state<ReadonlySet<string>>(new Set());
  let treeEl: HTMLDivElement | undefined = $state();
  // Graph node "Show on map" hands to the subsystems canvas; cleared on leaving it.
  let focusNodeId = $state<string | null>(null);
  let loading = $state(false);
  let building = $state(false);
  let errorMsg = $state<string | null>(null);
  let actionNote = $state<string | null>(null);
  let actionNoteKind = $state<"ok" | "error" | null>(null);
  let buildKind = $state<"build" | "refresh" | null>(null);
  // Plain: only timers read it, and it must not re-run any effect.
  let noteTimer: number | null = null;
  const SUCCESS_NOTE_MS = 6000;
  let mapView = $state<MapView>("navigator");
  let graphLoad = $state<GraphVizLoad | null>(null);
  let graphLoading = $state(false);
  let copyingLogs = $state(false);
  let copyLogsNote = $state<string | null>(null);

  let docsStatus = $state<DocsStatus | null>(null);
  let docsQuery = $state("");
  let docsHits = $state<DocsSearchHit[]>([]);
  let docsSearching = $state(false);
  // The trimmed query the current `docsHits` answer; null when no search has
  // completed. Set only when a search for it succeeds, so a failed search
  // leaves the earlier hits labelled with the query they really answer.
  let searchedQuery = $state<string | null>(null);
  // The last search failed. Broken-links reads own `brokenError`.
  let docsError = $state<string | null>(null);
  let brokenLinks = $state<BrokenLink[]>([]);
  let brokenLoading = $state(false);
  let brokenError = $state<string | null>(null);

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
      mapAvailable: load?.available ?? null,
      mapFailure: load?.failure,
    }),
  );
  const mapFailureText = $derived(
    !load?.available || (cliStatus && !cliStatus.available)
      ? mapFailureMessage(mapFailureMode, errorMsg ?? load?.reason ?? cliStatus?.reason)
      : null,
  );
  /**
   * What the automatic index is doing about this absence right now.
   *
   * The static messages tell the user to press Build. That was the only
   * recourse when the live path could not produce a navigator map at all;
   * now it can, so an empty pane on a repository that is mid-build must say
   * so rather than ask for a click the app has already made. `null` means
   * there is nothing in flight and the static message stands.
   */
  const indexActivity = $derived.by((): string | null => {
    if (!currentRepo) return null;
    const snapshot = $indexSnapshots[currentRepo];
    if (!snapshot) return null;
    switch (snapshot.phase) {
      case "scheduled":
        return "Indexing this repository — queued.";
      case "running":
        // No claim about whether this is the first build: the facts that would
        // establish that only come back when the attempt finishes. The stage,
        // when the kernel has reported one, is the only real progress there is.
        return snapshot.stage
          ? `Indexing this repository — stage ${snapshot.stage.current} of ${snapshot.stage.total}.`
          : "Indexing this repository… a large repository takes a few minutes.";
      case "failed":
        // The reason is the build's own stderr; it is the only thing that
        // explains why pressing Build again would not help either. Bounded
        // for exactly that reason — a failed build's stderr can be a log.
        return snapshot.reason
          ? `The automatic index failed: ${boundStrip(snapshot.reason)}`
          : "The automatic index failed.";
      case "paused":
        return indexPausedText(snapshot.reason);
      case "skipped":
        // A skip the user needs to know about is one a rebuild cannot fix.
        return snapshot.decision === "skip_schema_outdated" && snapshot.reason
          ? boundStrip(snapshot.reason)
          : null;
      default:
        return null;
    }
  });
  /**
   * The automatic index is paused because its last build ran past the
   * deadline. Shown over a map that did load too: the map on screen is then
   * the last one that finished, and nothing will refresh it on its own until
   * the pause ends or Build is pressed.
   */
  function indexPausedText(reason: string | null): string {
    return boundStrip(reason) ?? "Index too slow: automatic indexing is paused. Press Build to index now.";
  }
  const indexPaused = $derived.by((): string | null => {
    if (!currentRepo || !load?.available) return null;
    const snapshot = $indexSnapshots[currentRepo];
    return snapshot?.phase === "paused" ? indexPausedText(snapshot.reason) : null;
  });
  const cliKnownAbsent = $derived(cliStatus != null && !cliStatus.available);
  const areaRows = $derived(map ? buildAreaTree(map.subsystems) : []);
  const filtering = $derived(treeFilter.trim() !== "");
  const treeCollapsed = $derived(filtering ? NO_KEYS : collapsed);
  const visibleRows = $derived(visibleAreaRows(areaRows, collapsed, treeFilter));
  // The first subsystem is selected until the user picks another.
  const selectedRow = $derived(
    areaRows.find((r) => r.subsystemIndex !== null && r.key === selectedKey) ??
      areaRows.find((r) => r.subsystemIndex === 0) ??
      null,
  );
  const selected = $derived.by((): RepoMapSubsystem | null => {
    if (!map || selectedRow?.subsystemIndex == null) return null;
    return map.subsystems[selectedRow.subsystemIndex] ?? null;
  });
  // The one row in the tab order: the focused row, else the selection, else the first.
  const tabStopKey = $derived(
    visibleRows.find((r) => r.key === treeFocusKey)?.key ??
      visibleRows.find((r) => r.key === selectedRow?.key)?.key ??
      visibleRows[0]?.key ??
      null,
  );
  const deadLists = $derived(map ? preferredDeadLists(map) : null);
  const tests = $derived(selected ? roleSample(selected, "tests") : null);
  const docsHonesty = $derived(docsStatusHonesty(docsStatus));
  const searchHonesty = $derived(docsSearchHonesty(docsHits, DOCS_SEARCH_DEFAULT_LIMIT));
  const brokenDisplay = $derived(brokenLinksHonesty(brokenLinks, BROKEN_LINKS_DISPLAY_CAP));
  const searchPrompt = $derived(docsSearchPrompt(docsQuery, searchedQuery, docsHits.length, docsSearching, docsError !== null));
  const hitsStale = $derived(docsHitsStale(docsQuery, searchedQuery, docsHits.length));
  const brokenSection = $derived(
    brokenLinksSection({ loading: brokenLoading, error: brokenError, links: brokenLinks }),
  );
  const linksHonesty =$derived(linkCandidatesHonesty(linkResult));

  const currentRepo = $derived($currentRepoPath);
  const indexSnapshots = liveIndex.snapshots;
  const indexPhase = $derived(currentRepo ? $indexSnapshots[currentRepo]?.phase : undefined);
  const indexBusy = $derived(indexPhase === "scheduled" || indexPhase === "running");
  const buildBlockedReason = $derived(buildBlockReason(cliKnownAbsent, indexPhase));
  const contentRevisions = repoStore.contentRevisions;
  const indexRevision = $derived(currentRepo ? ($indexSnapshots[currentRepo]?.revision ?? 0) : 0);
  const contentRevision = $derived(currentRepo ? ($contentRevisions[currentRepo] ?? "") : "");
  let observedRepo: string | null = null;
  let observedIndex = 0;
  let observedContent = "";
  const detailScope = paneDetails.register("map");
  const requestIds: Record<string, number> = {};
  function trackedRequests(operation: string) {
    const generation = beginGeneration();
    return {
      isCurrent: generation.isCurrent,
      next() {
        const request = generation.next();
        requestIds[operation] = request;
        detailScope.update(currentRepo, mapView, requestIds);
        return request;
      },
    };
  }
  const mapRequests = trackedRequests("map");
  const graphRequests = trackedRequests("graph");
  const docsRequests = trackedRequests("docs");
  const searchRequests = trackedRequests("search");
  const linkRequests = trackedRequests("links");
  const buildRequests = trackedRequests("build");
  const copyRequests = trackedRequests("copy-logs");

  function cancelRequests() {
    for (const requests of [mapRequests, graphRequests, docsRequests, searchRequests, linkRequests, buildRequests, copyRequests]) requests.next();
  }
  onDestroy(cancelRequests);
  onDestroy(() => {
    if (noteTimer !== null) window.clearTimeout(noteTimer);
    noteTimer = null;
  });
  onDestroy(detailScope.dispose);

  $effect(() => {
    const path = currentRepo;
    const index = indexRevision;
    const content = contentRevision;
    untrack(() => {
      if (path && path === observedRepo) {
        if (index && index !== observedIndex) void reload(path);
        else if (content !== observedContent && (mapView === "docs" || mapView === "docgraph")) reloadView(path, mapView);
      }
      observedRepo = path;
      observedIndex = index;
      observedContent = content;
    });
  });

  function reportMapFailure(operation: string, repo: string, error: unknown): string {
    return reportPanelError("code-map", `Operation: ${operation}\nRepository: ${repo}\n${formatError(error)}`);
  }

  async function reload(path?: string, refreshView = true) {
    const repoPath = path ?? $repoStore.currentPath;
    if (!repoPath) return;
    loading = true;
    errorMsg = null;
    const request = mapRequests.next();
    const live = () => mapRequests.isCurrent(request) && currentRepo === repoPath;
    await settleMapBeforeStatus({
      map: getDevmapRepoMap(repoPath),
      status: getDevmapCliStatus(repoPath),
      isCurrent: live,
      onMap(mapResult) {
        loading = false;
        if (mapResult.status === "fulfilled") {
          load = mapResult.value;
          if (!mapResult.value.available) {
            errorMsg = mapResult.value.reason ?? "code map unavailable";
          }
        } else {
          load = null;
          errorMsg = reportMapFailure("map", repoPath, mapResult.reason);
        }
        if (refreshView) reloadView(repoPath, mapView);
      },
      onStatus(statusResult) {
        if (statusResult.status === "fulfilled") {
          cliStatus = statusResult.value;
        } else {
          cliStatus = null;
          reportMapFailure("status", repoPath, statusResult.reason);
        }
      },
    });
  }

  async function reloadGraph(path?: string) {
    const repoPath = path ?? $repoStore.currentPath;
    if (!repoPath || !CODE_GRAPH_VIEWS.has(mapView)) return;
    graphLoading = true;
    const request = graphRequests.next();
    const view = mapView;
    const live = () => graphRequests.isCurrent(request) && currentRepo === repoPath && mapView === view;
    try {
      let result: GraphVizLoad;
      if (view === "files") {
        result = await getCodeGraphViz(repoPath, false);
      } else if (view === "symbols") {
        result = await getCodeGraphViz(repoPath, true);
      } else if (view === "docgraph") {
        const graph = await docsGraph(repoPath);
        result = docGraphToLoad(graph, null);
      } else {
        result = await getMapPreviewViz(repoPath);
      }
      if (!live()) return;
      graphLoad = result;
    } catch (err) {
      if (!live()) return;
      graphLoad = {
        available: false,
        reason: reportMapFailure(view, repoPath, err),
        kind:
          view === "subsystems"
            ? "map_preview"
            : view === "docgraph"
              ? "doc_graph"
              : "code_graph",
        payload: null,
      };
    } finally {
      if (live()) graphLoading = false;
    }
  }

  async function reloadDocs(path?: string) {
    const repoPath = path ?? $repoStore.currentPath;
    if (!repoPath) return;
    const request = docsRequests.next();
    const live = () => docsRequests.isCurrent(request) && currentRepo === repoPath && mapView === "docs";
    brokenError = null;
    brokenLoading = true;
    let statusCommitted = false;
    try {
      const status = await docsRefresh(repoPath);
      if (!live()) return;
      // Committed before the links read so "N notes indexed" survives a
      // links-only failure.
      docsStatus = status;
      statusCommitted = true;
      const links = await docsBrokenLinks(repoPath);
      if (!live()) return;
      brokenLinks = links;
    } catch (err) {
      if (!live()) return;
      brokenError = reportMapFailure("documents", repoPath, err);
      brokenLinks = [];
      // A refresh that itself failed must not leave the previous run's count
      // on screen as if it were current.
      if (!statusCommitted) docsStatus = null;
    } finally {
      if (live()) brokenLoading = false;
    }
  }

  async function runDocsSearch() {
    const request = searchRequests.next();
    const repoPath = $repoStore.currentPath;
    const q = docsQuery.trim();
    if (!repoPath || !q) {
      docsHits = [];
      searchedQuery = null;
      docsSearching = false;
      rememberDocsSearch();
      return;
    }
    docsSearching = true;
    docsError = null;
    const live = () => searchRequests.isCurrent(request) && currentRepo === repoPath && docsQuery.trim() === q && mapView === "docs";
    try {
      const hits = await docsSearch(repoPath, q, DOCS_SEARCH_DEFAULT_LIMIT);
      if (live()) {
        docsHits = hits;
        searchedQuery = q;
        rememberDocsSearch();
      }
    } catch (err) {
      if (!live()) return;
      // The previous hits stay, still labelled with the query they answer.
      docsError = reportMapFailure("search", repoPath, err);
    } finally {
      if (live()) docsSearching = false;
    }
  }

  function rememberDocsSearch() {
    const repo = currentRepo;
    if (!repo) return;
    docsSearchMemo.set(repo, {
      docsQuery,
      searchedQuery,
      docsHits: $state.snapshot(docsHits),
    });
  }

  async function reloadLinks(path?: string) {
    const registryRoot = path ?? $repoStore.currentPath;
    if (!registryRoot) return;
    linksLoading = true;
    linksError = null;
    const request = linkRequests.next();
    const live = () => linkRequests.isCurrent(request) && currentRepo === registryRoot && mapView === "links";
    try {
      const result = await getWorkspaceLinkCandidates(registryRoot);
      if (!live()) return;
      linkResult = result;
    } catch (err) {
      if (!live()) return;
      linkResult = null;
      linksError = reportMapFailure("links", registryRoot, err);
    } finally {
      if (live()) linksLoading = false;
    }
  }

  function clearNote() {
    if (noteTimer !== null) window.clearTimeout(noteTimer);
    noteTimer = null;
    actionNote = null;
    actionNoteKind = null;
  }

  /**
   * A success note is a transient confirmation and fades; a failure stays
   * until the next build or a repository switch, because it is the only
   * account of why the map on screen is not the one just asked for.
   */
  function setNote(text: string, kind: "ok" | "error") {
    clearNote();
    actionNote = text;
    actionNoteKind = kind;
    if (kind === "ok") noteTimer = window.setTimeout(clearNote, SUCCESS_NOTE_MS);
  }

  async function runBuild(kind: "build" | "refresh") {
    const repoPath = $repoStore.currentPath;
    if (!repoPath) return;
    const request = buildRequests.next();
    const live = () => buildRequests.isCurrent(request) && currentRepo === repoPath;
    building = true;
    buildKind = kind;
    clearNote();
    errorMsg = null;
    try {
      const outcome = kind === "build" ? await buildDevmap(repoPath) : await refreshDevmap(repoPath);
      if (!live()) return;
      if (!outcome.ok) {
        setNote(reportMapFailure(kind, repoPath,
          buildFailureReason(outcome, outcome.stdout.trim() || `the code map ${kind} failed`)), "error");
      } else {
        setNote(`Map ${kind === "build" ? "built" : "refreshed"}.`, "ok");
      }
      await reload(repoPath);
    } catch (err) {
      if (live()) setNote(reportMapFailure(kind, repoPath, err), "error");
    } finally {
      if (live()) {
        building = false;
        buildKind = null;
      }
    }
  }

  async function copyMapLogs() {
    const repository = currentRepo;
    if (!repository || copyingLogs) return;
    const request = copyRequests.next();
    const live = () => copyRequests.isCurrent(request) && currentRepo === repository;
    copyingLogs = true;
    copyLogsNote = null;
    try {
      const result = await copyDevmapLogs({
        repository, view: mapView, building, cli: cliStatus,
        map: load ? { available: load.available, path: load.path, reason: load.reason } : null,
        graph: graphLoad ? { available: graphLoad.available, kind: graphLoad.kind, reason: graphLoad.reason } : null,
        liveIndex: liveIndex.get(repository),
        errors: [errorMsg, actionNote, docsError, brokenError, linksError],
      }, live);
      if (live()) copyLogsNote = result === "copied" ? "Code map logs copied." : "Couldn't copy the code map logs. Try again.";
    } catch (error) {
      if (live()) copyLogsNote = `Couldn't copy the code map logs. ${reportMapFailure("copy logs", repository, error)}`;
    } finally {
      if (live()) copyingLogs = false;
    }
  }

  $effect(() => {
    const path = currentRepo;
    untrack(() => {
      cancelRequests();
      load = null;
      cliStatus = null;
      selectedKey = null;
      treeFocusKey = null;
      treeFilter = "";
      collapsed = new Set();
      focusNodeId = null;
      graphLoad = null;
      docsStatus = null;
      // A repository's docs search comes back as it was left; nothing re-runs.
      const memo = path ? docsSearchMemo.get(path) : undefined;
      docsQuery = memo?.docsQuery ?? "";
      searchedQuery = memo?.searchedQuery ?? null;
      docsHits = memo?.docsHits ?? [];
      brokenLinks = [];
      linkResult = null;
      errorMsg = null;
      docsError = null;
      brokenError = null;
      linksError = null;
      clearNote();
      buildKind = null;
      copyLogsNote = null;
      copyingLogs = false;
      loading = building = graphLoading = brokenLoading = docsSearching = linksLoading = false;
      // The view effect below owns the initial subview request.
      if (path) void reload(path, false);
    });
  });

  $effect(() => {
    const view = mapView;
    const path = currentRepo;
    untrack(() => {
      graphRequests.next();
      docsRequests.next();
      searchRequests.next();
      linkRequests.next();
      graphLoading = brokenLoading = docsSearching = linksLoading = false;
      // The previous view's payload must not stay painted (and interactive)
      // under the new tab while the new one loads.
      graphLoad = null;
      // A "Show on map" request belongs to the visit it started.
      if (view !== "subsystems") focusNodeId = null;
      if (actionNoteKind === "ok") clearNote();
      if (path) reloadView(path, view);
    });
  });

  $effect(() => {
    const query = docsQuery.trim();
    untrack(() => {
      // Typing retires a search in flight but keeps the hits on screen: they
      // are labelled with the query they answer until the next search lands.
      searchRequests.next();
      docsSearching = false;
      // Search-only since broken links own `brokenError`; an edit supersedes it.
      docsError = null;
      // Earlier results stay (dimmed) while the query is edited; only an
      // emptied box discards them.
      if (query === "") {
        docsHits = [];
        searchedQuery = null;
      }
      rememberDocsSearch();
    });
  });

  function reloadView(path: string, view: MapView) {
    if (CODE_GRAPH_VIEWS.has(view)) {
      void reloadGraph(path);
    } else if (view === "docs") {
      void reloadDocs(path);
    } else if (view === "links") {
      void reloadLinks(path);
    }
  }

  /** Move the roving tab stop to a row and give it DOM focus once it is drawn. */
  function focusRow(key: string) {
    treeFocusKey = key;
    void tick().then(() => {
      for (const el of treeEl?.querySelectorAll<HTMLElement>('[role="treeitem"]') ?? []) {
        if (el.dataset.rowKey === key) {
          el.focus();
          el.scrollIntoView?.({ block: "nearest" });
          return;
        }
      }
    });
  }

  /** Select a row, opening what hides it: collapsed ancestors and a filter that excludes it. */
  function selectKey(key: string) {
    const ancestors = ancestorKeys(areaRows, key);
    if (ancestors.some((k) => collapsed.has(k))) {
      collapsed = new Set([...collapsed].filter((k) => !ancestors.includes(k)));
    }
    if (!visibleAreaRows(areaRows, collapsed, treeFilter).some((r) => r.key === key)) treeFilter = "";
    selectedKey = key;
    focusRow(key);
  }

  function toggleRow(key: string) {
    if (filtering) return;
    const next = new Set(collapsed);
    if (!next.delete(key)) next.add(key);
    collapsed = next;
  }

  function onRowClick(row: (typeof areaRows)[number]) {
    treeFocusKey = row.key;
    if (row.subsystemIndex === null) toggleRow(row.key);
    else selectedKey = row.key;
  }

  function onRowKeydown(e: KeyboardEvent, row: (typeof areaRows)[number]) {
    if (e.target !== e.currentTarget || e.altKey || e.ctrlKey || e.metaKey) return;
    const outcome = treeKeyOutcome(e.key, visibleRows, row.key, treeCollapsed);
    if (!outcome) return;
    e.preventDefault();
    if (outcome.collapsed && !filtering) collapsed = outcome.collapsed;
    if (outcome.select) selectedKey = outcome.select;
    focusRow(outcome.focusKey);
  }

  /** Switch to the subsystem map and ask the canvas to focus this subsystem's node. */
  function showOnMap(sub: RepoMapSubsystem) {
    focusNodeId = subsystemNodeId(sub.area);
    setView("subsystems");
  }

  function openFile(path: string) {
    repoStore.selectFilePath(path);
    repoStore.setActiveTab("code", "explorer");
  }

  /** A docs hit lands on its own line, not the top of the file. */
  function openHit(hit: DocsSearchHit) {
    requestReveal(hit.path, hit.line, null);
    openFile(hit.path);
  }

  function openGraphNode(path: string, _nodeId: string) {
    // The canvas resolves explicit file ownership, including root-level and
    // extensionless files. Do not add a second language whitelist here.
    openFile(path);
  }

  function setView(view: MapView) {
    mapView = view;
  }

  let tablistEl: HTMLDivElement | undefined = $state();

  function onTablistKeydown(e: KeyboardEvent) {
    const move = handleTablistKeydown(e.key, MAP_VIEWS.findIndex((v) => v.id === mapView), MAP_VIEWS.length);
    if (!move) return;
    e.preventDefault();
    setView(MAP_VIEWS[move.index].id);
    focusTabAt(tablistEl, move.index);
  }

  // The strip hides its scrollbar, same as the repository bar. A vertical
  // wheel pans it once the tabs no longer fit; while they fit, the wheel
  // stays with the page.
  $effect(() => {
    const el = tablistEl;
    if (!el) return;
    return bindHorizontalWheel(el);
  });

  // Keep the selected tab inside this strip. `scrollIntoView` would also
  // pan the map canvas.
  $effect(() => {
    const scroller = tablistEl;
    const id = tabId("map-view", mapView);
    if (!scroller) return;
    const child = scroller.querySelector<HTMLElement>(`#${CSS.escape(id)}`);
    if (!child) return;
    scroller.scrollLeft = scrollChildIntoHorizontalView(scroller, child);
  });
</script>

<div class="flex-1 flex flex-col min-h-0 font-sans text-sm text-textPrimary">
  <!-- Freshness strip from cmd_devmap_status -->
  <div
    class="shrink-0 flex flex-wrap items-center gap-x-3 gap-y-1 bg-surface/60 px-3 py-2 text-meta text-textMuted"
    role="status"
  >
    {#if loading && !map}
      <div class="w-28"><Skeleton variant="text" height="0.625rem" /></div>
    {:else if statusPayload}
      <!-- Fresh is quiet; Stale is a toned chip. The word carries the state, the tone only reinforces it. -->
      <span
        class={statusPayload.is_fresh ? "text-ok" : "gp-chip"}
        data-tone={statusPayload.is_fresh ? undefined : "warn"}
        title={statusPayload.is_fresh ? "The index matches the latest commit" : "The index is behind the latest commit"}
      >
        {statusPayload.is_fresh ? "Fresh" : "Stale"}
      </span>
      <span title="Index build number">index {statusPayload.generation_id ?? "—"}</span>
      {#if statusPayload.schema_outdated}
        <span class="gp-chip" data-tone="danger" title="The index format is older than this version of GitPulse expects">
          index format out of date ({statusPayload.schema_version ?? "?"} → {statusPayload.expected_schema_version ?? "?"})
        </span>
      {:else if statusPayload.schema_version != null}
        <span>format {statusPayload.schema_version}</span>
      {/if}
      {#if statusPayload.node_count != null}
        <span>{statusPayload.node_count} nodes · {statusPayload.edge_count ?? "?"} edges</span>
      {/if}
      {#if coverageGapSummary(statusPayload)}
        <span class="gp-chip" data-tone="warn" title="Languages and files the index couldn't fully cover">
          gaps: {coverageGapSummary(statusPayload)}
        </span>
      {/if}
    <!-- `mapFailureMessage` bounds what it returns, but these fallbacks reach
         the strip without passing through it, and both are process output. -->
    {:else if cliStatus && !cliStatus.available}
      <span class="text-warn">{mapFailureText ?? boundStrip(cliStatus.reason) ?? "The code map tool isn't installed"}</span>
    {:else if load && !load.available}
      <span class="text-warn">{mapFailureText ?? boundStrip(load.reason) ?? "The code map isn't available"}</span>
    {:else}
      <span>Status unknown</span>
    {/if}
    <span class="ml-auto flex items-center gap-1.5">
      <button
        type="button"
        class="gp-btn text-meta px-2 py-0.5"
        disabled={copyingLogs || !currentRepo}
        onclick={() => void copyMapLogs()}
      >
        {copyingLogs ? "Copying logs…" : "Copy code map logs"}
      </button>
      <button
        type="button"
        class="gp-btn text-meta px-2 py-0.5"
        disabled={building || !$repoStore.currentPath || buildBlockedReason != null}
        title={buildBlockedReason ?? "Refresh the map index"}
        aria-busy={building && buildKind === "refresh"}
        onclick={() => void runBuild("refresh")}
      >
        {#if building && buildKind === "refresh"}
          <Loader2 size={12} class="animate-spin inline-block align-[-2px]" aria-hidden="true" /> Refreshing…
        {:else}
          Refresh index
        {/if}
      </button>
      <button
        type="button"
        class="gp-btn text-meta px-2 py-0.5"
        disabled={building || !$repoStore.currentPath || buildBlockedReason != null}
        title={buildBlockedReason ?? "Build the map index"}
        aria-busy={building && buildKind === "build"}
        onclick={() => void runBuild("build")}
      >
        {#if building && buildKind === "build"}
          <Loader2 size={12} class="animate-spin inline-block align-[-2px]" aria-hidden="true" /> Building…
        {:else}
          Build index
        {/if}
      </button>
    </span>
  </div>

  {#if copyLogsNote}
    <p class="shrink-0 px-3 py-1 text-meta text-textMuted" role="status">{copyLogsNote}</p>
  {/if}

  {#if cliStatus && !cliStatus.available}
    <div class="gp-notice gp-notice-warn shrink-0 space-y-2">
      <p class="text-xs text-textPrimary">{mapFailureMessage("no_cli")}</p>
      <button
        type="button"
        class="gp-btn text-meta px-2 py-0.5"
        onclick={() => openSetupWizard("devmap", "explain")}
      >
        Set up code map
      </button>
      <ExternalToolsPanel
        only={SETUP_TOOL}
        compact
        onInstalled={() => {
          void reload();
        }}
      />
    </div>
  {:else if load && !load.available && mapFailureMode !== "other"}
    <div class={["gp-notice shrink-0 space-y-1", !indexActivity && "gp-notice-warn"]}>
      {#if indexActivity}
        <p class="text-xs text-textMuted" data-testid="map-index-activity">{indexActivity}</p>
      {:else}
        <p class="text-xs text-textPrimary">{mapFailureText}</p>
      {/if}
    </div>
  {:else if indexPaused}
    <div class="gp-notice gp-notice-warn shrink-0">
      <p class="text-xs text-textPrimary" data-testid="map-index-paused">{indexPaused}</p>
    </div>
  {/if}

  <!-- View switcher: the header's segmented tabs, one scrolling line. -->
  <div class="relative flex w-full min-w-0 shrink-0 px-3 py-1">
    <div
      bind:this={tablistEl}
      class={[
        "gp-header-scroll gp-segmented relative min-w-0 w-max max-w-full flex-nowrap items-center",
        { "gp-liquid-tabs": macos },
      ]}
      role="tablist"
      aria-label="Map views"
      tabindex="-1"
      onkeydown={onTablistKeydown}
    >
      {#each MAP_VIEWS as view (view.id)}
        {@const selected = mapView === view.id}
        <button
          type="button"
          {...tabProps("map-view", view.id, selected)}
          data-active={selected ? "true" : "false"}
          class="gp-seg-btn relative inline-flex h-7 shrink-0 items-center whitespace-nowrap py-0!"
          onclick={() => setView(view.id)}
          title={view.title}
        >
          {#if macos && selected}
            <span
              class="gp-liquid-selection gp-gpu"
              aria-hidden="true"
              in:receiveSelection={{ key: "map-view" }}
              out:sendSelection={{ key: "map-view" }}
            ></span>
          {/if}
          <span class="whitespace-nowrap">{view.label}</span>
        </button>
      {/each}
    </div>
    <ScrollCue target={tablistEl} axis="x" />
  </div>

  {#if actionNote}
    <div class={["gp-notice shrink-0", actionNoteKind === "error" ? "gp-notice-error" : "text-textMuted"]} role="status">
      {boundStrip(actionNote)}
    </div>
  {/if}

  <div
    class="flex-1 min-h-0 flex flex-col"
    role="tabpanel"
    id={panelId("map-view", mapView)}
    aria-labelledby={tabId("map-view", mapView)}
  >
  {#if CODE_GRAPH_VIEWS.has(mapView)}
    <div class="flex-1 min-h-0 flex flex-col">
      <CodeGraphCanvas scopeKey={JSON.stringify([currentRepo, mapView])} load={graphLoad} loading={graphLoading} onOpenNode={openGraphNode} focusNodeId={mapView === "subsystems" ? focusNodeId : null} />
    </div>
  {:else if mapView === "docs"}
    <div class="flex-1 min-h-0 overflow-y-auto p-3 space-y-4" data-testid="docs-panel">
      {#if docsStatus}
        <p class="text-meta text-textMuted" role="status">
          {docsStatus.noteCount} note{docsStatus.noteCount === 1 ? "" : "s"} indexed
        </p>
      {/if}
      {#if docsHonesty}
        <p class="text-micro text-warn" role="note" data-testid="docs-status-honesty">
          {docsHonesty}
        </p>
      {/if}

      <section aria-labelledby="docs-search-heading">
        <h3 id="docs-search-heading" class="gp-label mb-1.5">
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
            class="gp-field flex-1"
            placeholder="Full-text markdown search…"
            bind:value={docsQuery}
            aria-label="Search repository markdown"
          />
          <button type="submit" class="gp-btn text-meta px-2 py-1" disabled={docsSearching || !$repoStore.currentPath || !docsQuery.trim()}>
            {docsSearching ? "Searching…" : "Search"}
          </button>
        </form>
        {#if searchHonesty}
          <p class="mt-1 text-micro text-warn" data-testid="docs-search-honesty">
            {searchHonesty}
          </p>
        {/if}
        {#if docsError}
          <p class="mt-2 text-meta text-danger" role="alert" data-testid="docs-search-error">{boundStrip(docsError)}</p>
        {/if}
        {#if searchPrompt === "none"}
          <p class="mt-2 text-meta text-textMuted" data-testid="docs-no-hits">No hits for "{searchedQuery}".</p>
        {:else if searchPrompt === "press-enter"}
          <p class="mt-2 text-meta text-textMuted" data-testid="docs-search-prompt">Press Enter to search.</p>
        {/if}
        {#if docsHits.length > 0}
          {#if hitsStale}
            <p class="mt-2 text-micro text-textMuted" data-testid="docs-hits-stale">Results for "{searchedQuery}"</p>
          {/if}
          <ul class="mt-2 space-y-1.5" class:opacity-60={hitsStale}>
            {#each keyedList(docsHits, (hit) => JSON.stringify([hit.path, hit.line, hit.score])) as { item: hit, key: renderKey } (renderKey)}
              <li>
                <button
                  type="button"
                  class="w-full text-left rounded-md px-2 py-1.5 hover:bg-surface/80"
                  onclick={() => openHit(hit)}
                >
                  <span class="font-mono text-meta text-accent block truncate">{hit.path}:{hit.line}</span>
                  <span class="text-meta text-textPrimary block truncate">{hit.title}</span>
                  <span class="text-micro text-textMuted block truncate">{hit.context}</span>
                </button>
              </li>
            {/each}
          </ul>
        {/if}
      </section>

      <section aria-labelledby="docs-broken-heading" class="pt-3">
        <h3 id="docs-broken-heading" class="gp-label mb-1.5">
          Broken links
          {#if brokenSection === "list" || brokenSection === "empty"}
            <span class="font-normal ml-1">({brokenLinks.length})</span>
          {/if}
        </h3>
        {#if brokenSection === "loading"}
          <Skeleton variant="rect" count={2} height="2.75rem" />
        {:else if brokenSection === "error"}
          <p class="text-meta text-danger" role="alert" data-testid="docs-broken-error">Couldn't check links. {boundStrip(brokenError)}</p>
          <button type="button" class="gp-btn text-meta px-2 py-0.5 mt-1" onclick={() => void reloadDocs()}>Try again</button>
        {:else if brokenSection === "empty"}
          <p class="text-meta text-textMuted">None found in the indexed vault.</p>
        {:else}
          {#if brokenDisplay.honesty}
            <p class="mb-1 text-micro text-warn" data-testid="docs-broken-honesty">
              {brokenDisplay.honesty}
            </p>
          {/if}
          <ul class="space-y-0.5 max-h-64 overflow-y-auto">
            {#each keyedList(brokenDisplay.shown, (link) => JSON.stringify([link.source, link.target])) as { item: link, key: renderKey } (renderKey)}
              <li class="text-meta">
                <button
                  type="button"
                  class="gp-btn font-mono max-w-full"
                  onclick={() => openFile(link.source)}
                >
                  <span class="truncate">{link.source}</span>
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
        <h3 class="gp-label">Cross-repository link candidates</h3>
        <p class="text-meta text-textMuted mt-0.5">
          Import-graph edges between registered workspace repositories — not name-matched call edges.
        </p>
      </header>
      {#if linksHonesty}
        <p class="text-micro text-textMuted" role="status" data-testid="link-candidates-honesty">
          {linksHonesty}
        </p>
      {/if}
      {#if linksError}
        <p class="text-meta text-danger" role="alert">{boundStrip(linksError)}</p>
      {/if}
      {#if linksLoading}
        <Skeleton variant="rect" count={2} height="2.75rem" />
      {:else if linkResult && linkResult.links.length > 0}
        <ul class="space-y-1.5">
          {#each keyedList(linkResult.links, (link) => JSON.stringify([link.from_repo, link.from_file, link.module_specifier, link.to_repo])) as { item: link, key: renderKey } (renderKey)}
            <li class="rounded-md bg-surface/60 px-2 py-1.5 text-meta">
              <div class="font-mono text-textPrimary truncate">
                [{link.from_repo}] {link.from_file}
              </div>
              <div class="text-textMuted mt-0.5">
                <span class="font-mono">{link.module_specifier}</span>
                <span> → </span>
                <span class="font-mono text-accent">[{link.to_repo}]</span>
              </div>
              {#if link.evidence}
                <div class="text-micro text-textMuted mt-0.5 truncate" title={link.evidence}>
                  {link.evidence}
                </div>
              {/if}
            </li>
          {/each}
        </ul>
      {/if}
      <button
        type="button"
        class="gp-btn text-meta px-2 py-1"
        disabled={linksLoading || !$repoStore.currentPath}
        onclick={() => void reloadLinks()}
      >
        Refresh links
      </button>
    </div>
  {:else if loading && !map}
    <!-- The test id is how harness/diagnostics.html tells a panel still
         loading from one that has its map: the skeleton carries no text. -->
    <div class="flex-1 p-4 flex flex-col gap-3" aria-busy="true" data-testid="map-loading">
      <Skeleton variant="text" width="40%" height="1rem" />
      <Skeleton variant="card" />
      <Skeleton variant="text" count={5} />
    </div>
  {:else if !map}
    {#if indexBusy && indexActivity}
      <div class="flex-1 flex flex-col items-center justify-center gap-2 p-6 text-center" role="status">
        <p class="text-sm text-textPrimary">Building the code map</p>
        <p class="text-meta text-textMuted flex items-center gap-1.5" aria-busy="true" data-testid="map-index-progress">
          <Loader2 size={12} class="animate-spin" aria-hidden="true" />
          {indexActivity}
        </p>
      </div>
    {:else}
      <div class="flex-1 flex items-center justify-center" role="status">
        <EmptyState
          icon={Network}
          title="No code map yet"
          hint={mapFailureText ?? boundStrip(errorMsg) ?? "Build the code map to navigate subsystems, entry points and area tests."}
          action={{
            label: building && buildKind === "build" ? "Building…" : "Build index",
            onClick: () => void runBuild("build"),
            variant: "secondary",
            disabled: building || !$repoStore.currentPath || buildBlockedReason != null,
            title: buildBlockedReason ?? "Build the map index",
            busy: building && buildKind === "build",
          }}
        />
      </div>
    {/if}
  {:else}
    {@const doc = map as RepoMapDocument}
    <div class="flex-1 flex min-h-0 gap-2 p-2">
      <!-- Subsystem tree -->
      <nav
        class="w-64 shrink-0 flex flex-col min-h-0 rounded-xl bg-surface/60"
        aria-label="Subsystems"
      >
        <div class="shrink-0 space-y-2 px-2.5 pt-2.5 pb-2">
          <div class="gp-label">
            Subsystems
            <span class="font-normal ml-1">
              {formatCap(
                doc.liveness_meta.subsystems.shown,
                doc.liveness_meta.subsystems.total,
                doc.liveness_meta.subsystems.truncated,
              )}
            </span>
          </div>
          {#if doc.liveness_meta.subsystems.truncated}
            <span class="gp-chip" data-tone="warn" data-testid="map-subsystems-cap">
              Showing {doc.liveness_meta.subsystems.shown} of {doc.liveness_meta.subsystems.total}
              {#if doc.liveness_meta.subsystems.dropped_no_area > 0}
                · {doc.liveness_meta.subsystems.dropped_no_area} dropped (no area)
              {/if}
            </span>
          {/if}
          <input
            type="search"
            class="gp-field w-full"
            placeholder="Filter areas"
            aria-label="Filter subsystem areas"
            bind:value={treeFilter}
          />
        </div>
        <div
          bind:this={treeEl}
          role="tree"
          aria-label="Subsystem areas"
          class="min-h-0 flex-1 overflow-y-auto px-1 pb-2"
        >
          {#each visibleRows as row (row.key)}
            {@const isSelected = row.subsystemIndex !== null && row.key === selectedRow?.key}
            {@const open = row.hasChildren && !treeCollapsed.has(row.key)}
            {@const rowSummary = row.subsystemIndex === null ? "" : summaryWithoutArea(doc.subsystems[row.subsystemIndex])}
            <div
              role="treeitem"
              data-row-key={row.key}
              tabindex={row.key === tabStopKey ? 0 : -1}
              aria-level={row.depth + 1}
              aria-expanded={row.hasChildren ? open : undefined}
              aria-selected={row.subsystemIndex === null ? undefined : isSelected}
              title={row.area}
              style="padding-left: {0.25 + row.depth * 0.875}rem"
              class="flex cursor-pointer items-start gap-1 rounded-md py-1.5 pr-2 text-xs outline-none hover:bg-surfaceHover/70 focus-visible:ring-1 focus-visible:ring-accent/60 {isSelected
                ? 'bg-accent/15 text-textPrimary'
                : 'text-textMuted'}"
              onclick={() => onRowClick(row)}
              onkeydown={(e) => onRowKeydown(e, row)}
            >
              <span class="mt-0.5 flex h-4 w-4 shrink-0 items-center justify-center" aria-hidden="true">
                {#if row.hasChildren}
                  {#if open}<ChevronDown size={12} />{:else}<ChevronRight size={12} />{/if}
                {/if}
              </span>
              <span class="min-w-0 flex-1">
                <span class="flex items-center gap-1.5">
                  <span class="min-w-0 flex-1 truncate font-mono">{row.label}</span>
                  {#if row.duplicateCount > 1}
                    <span
                      class="gp-chip shrink-0"
                      title="{row.duplicateCount} subsystems share this area"
                    >×{row.duplicateCount}</span>
                  {/if}
                </span>
                {#if rowSummary}
                  <span class="block truncate opacity-80" title={rowSummary}>{rowSummary}</span>
                {/if}
              </span>
            </div>
          {:else}
            <p class="px-2 py-2 text-xs text-textMuted" role="status">No areas match "{treeFilter.trim()}".</p>
          {/each}
        </div>
      </nav>

      <!-- Detail -->
      <div class="flex-1 min-w-0 overflow-y-auto p-3 space-y-4">
        {#if selected}
          {@const detailSummary = summaryWithoutArea(selected)}
          <header class="flex items-start justify-between gap-3">
            <div class="min-w-0">
              <h2 class="font-mono text-sm text-textPrimary break-all">{selected.area}</h2>
              {#if detailSummary}
                <p class="text-meta text-textMuted mt-0.5">{detailSummary}</p>
              {/if}
              {#if selectedRow && selectedRow.duplicateCount > 1}
                <p class="text-meta text-textMuted mt-0.5" data-testid="map-duplicate-area">
                  {selectedRow.duplicateCount} subsystems share this area; showing the first.
                </p>
              {/if}
            </div>
            <button
              type="button"
              class="gp-btn shrink-0"
              title="Open the subsystem map focused on this subsystem"
              onclick={() => showOnMap(selected)}
            >
              Show on map
            </button>
          </header>

          <section aria-labelledby="map-entry-points">
            <h3 id="map-entry-points" class="gp-label mb-1">
              Entry points
            </h3>
            {#if selected.entry_points.length === 0}
              <p class="text-meta text-textMuted">None listed.</p>
            {:else}
              <ul class="space-y-0.5">
                {#each selected.entry_points as path, i (`${path}#${i}`)}
                  <li>
                    <button
                      type="button"
                      class="gp-btn font-mono max-w-full"
                      onclick={() => openFile(path)}
                    >
                      <span class="truncate">{path}</span>
                    </button>
                  </li>
                {/each}
              </ul>
            {/if}
          </section>

          <section aria-labelledby="map-critical">
            <h3 id="map-critical" class="gp-label mb-1">
              Critical files
            </h3>
            {#if selected.critical_files.length === 0}
              <p class="text-meta text-textMuted">None listed.</p>
            {:else}
              <ul class="space-y-0.5">
                {#each selected.critical_files as path, i (`${path}#${i}`)}
                  <li>
                    <button
                      type="button"
                      class="gp-btn font-mono max-w-full"
                      onclick={() => openFile(path)}
                    >
                      <span class="truncate">{path}</span>
                    </button>
                  </li>
                {/each}
              </ul>
            {/if}
          </section>

          <section aria-labelledby="map-tests">
            <h3 id="map-tests" class="gp-label mb-1">
              Tests
              {#if tests}
                <span class="font-normal ml-1">
                  {formatCap(tests.paths.length, tests.total, tests.truncated)}
                </span>
              {/if}
            </h3>
            {#if !tests || tests.paths.length === 0}
              <p class="text-meta text-textMuted">
                {tests && tests.total > 0
                  ? `Sample empty; the index lists ${tests.total} test file(s).`
                  : "No test role files in this area."}
              </p>
            {:else}
              <ul class="space-y-0.5">
                {#each tests.paths as path, i (`${path}#${i}`)}
                  <li>
                    <button
                      type="button"
                      class="gp-btn font-mono max-w-full"
                      onclick={() => openFile(path)}
                    >
                      <span class="truncate">{path}</span>
                    </button>
                  </li>
                {/each}
              </ul>
              {#if tests.truncated}
                <p class="mt-1 text-micro text-warn">
                  Sample of {tests.paths.length}; {tests.total} total for this area.
                </p>
              {/if}
            {/if}
          </section>

          {#if selected.neighbors.length > 0}
            <section aria-labelledby="map-neighbors">
              <h3 id="map-neighbors" class="gp-label mb-1">
                Neighbors
              </h3>
              <ul class="flex flex-wrap gap-1.5">
                {#each selected.neighbors as neighbor, i (`${neighbor}#${i}`)}
                  {@const neighborKey = neighborRowKey(areaRows, neighbor)}
                  <li class="min-w-0 max-w-full">
                    {#if neighborKey}
                      <button
                        type="button"
                        class="gp-btn font-mono max-w-full"
                        title="Select {neighbor} in the tree"
                        onclick={() => selectKey(neighborKey)}
                      >
                        <span class="truncate">{neighbor}</span>
                      </button>
                    {:else}
                      <span class="gp-chip font-mono text-textMuted" title="{neighbor} is not in this map's subsystem list">{neighbor}</span>
                    {/if}
                  </li>
                {/each}
              </ul>
            </section>
          {/if}
        {/if}

        <!-- Dead / unwired candidates (prefer over unreachable) -->
        {#if deadLists}
          <section class="pt-3 space-y-3" aria-label="Dead and unwired candidates">
            <div>
              <h3 class="gp-label mb-1">
                Unwired candidates
                <span class="font-normal ml-1">
                  {formatCap(
                    doc.liveness_meta.unwired.shown,
                    doc.liveness_meta.unwired.total,
                    doc.liveness_meta.unwired.truncated,
                  )}
                </span>
              </h3>
              {#if deadLists.unwired.length === 0}
                <p class="text-meta text-textMuted">None in this sample.</p>
              {:else}
                <ul class="space-y-0.5 max-h-32 overflow-y-auto">
                  {#each deadLists.unwired as path, i (`${path}#${i}`)}
                    <li>
                      <button
                        type="button"
                        class="font-mono text-meta text-textMuted hover:text-accent truncate max-w-full"
                        onclick={() => openFile(path)}
                      >
                        {path}
                      </button>
                    </li>
                  {/each}
                </ul>
              {/if}
              {#if unwiredExclusionSummary(doc.liveness_meta.unwired).length > 0}
                <p class="mt-1 text-micro text-textMuted">
                  Excluded: {unwiredExclusionSummary(doc.liveness_meta.unwired).join(", ")}
                </p>
              {/if}
            </div>
            <div>
              <h3 class="gp-label mb-1">
                Dead symbol candidates
                <span class="font-normal ml-1">
                  {formatCap(
                    doc.liveness_meta.dead_symbol.shown,
                    doc.liveness_meta.dead_symbol.total,
                    doc.liveness_meta.dead_symbol.truncated,
                  )}
                </span>
              </h3>
              {#if deadLists.deadSymbols.length === 0}
                <p class="text-meta text-textMuted">None in this sample.</p>
              {:else}
                <ul class="space-y-0.5 max-h-40 overflow-y-auto">
                  {#each deadLists.deadSymbols as id, i (`${id}#${i}`)}
                    <li class="font-mono text-meta text-textMuted truncate" title={id}>{id}</li>
                  {/each}
                </ul>
              {/if}
            </div>
            {#if deadLists.unreachableSuppressed}
              <p class="text-micro text-warn">
                Unreachable files are hidden because the index marks that list as unreliable.
                Prefer unwired and dead-symbol candidates above.
              </p>
            {:else if deadLists.unreachable.length > 0}
              <div>
                <h3 class="gp-label mb-1">
                  Unreachable files (secondary)
                </h3>
                <ul class="space-y-0.5 max-h-24 overflow-y-auto">
                  {#each deadLists.unreachable as path, i (`${path}#${i}`)}
                    <li class="font-mono text-meta text-textMuted truncate">{path}</li>
                  {/each}
                </ul>
              </div>
            {/if}
          </section>
        {/if}

        {#if doc.liveness_meta.entry_roots.truncated || doc.liveness_meta.subsystems.role_files_truncated}
          <p class="text-micro text-textMuted pt-2">
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
</div>
