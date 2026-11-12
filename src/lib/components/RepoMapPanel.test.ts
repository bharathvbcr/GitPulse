import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { compile } from "svelte/compiler";
import { render } from "svelte/server";
import RepoMapPanel, { settleMapBeforeStatus } from "./RepoMapPanel.svelte";

const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, "RepoMapPanel.svelte"), "utf8");
const codeView = readFileSync(join(here, "CodeView.svelte"), "utf8");

describe("RepoMapPanel", () => {
  it("compiles", () => {
    const result = compile(source, { filename: "RepoMapPanel.svelte", generate: "server" });
    expect(result.js.code.length).toBeGreaterThan(0);
  });

  it("loads the typed map and status freshness strip", () => {
    expect(source).toContain("getDevmapRepoMap");
    expect(source).toContain("getDevmapCliStatus");
    expect(source).toContain("is_fresh");
    expect(source).toContain("schema_outdated");
    // The strip names coverage gaps in words; the raw `coverage_gaps` id is no
    // longer printed as a tooltip, so the read of it is what is pinned.
    expect(source).toContain("coverageGapSummary(statusPayload)");
    expect(source).toContain("generation_id");
  });

  it("navigates subsystems with entry points, critical files and role tests", () => {
    expect(source).toContain("entry_points");
    expect(source).toContain("critical_files");
    expect(source).toContain('roleSample(selected, "tests")');
    expect(source).toContain("role_file_counts");
    expect(source).toContain("liveness_meta");
  });

  it("prefers unwired and dead candidates over unreliable unreachable", () => {
    expect(source).toContain("preferredDeadLists");
    expect(source).toContain("unreachableSuppressed");
    expect(source).toContain("liveness_unreachable_unreliable");
  });

  it("does not present capped samples as complete", () => {
    expect(source).toContain("formatCap");
    expect(source).toContain("truncated");
  });

  it("offers graph modes backed by viz, map_preview and docs graph payloads", () => {
    expect(source).toContain("getCodeGraphViz");
    expect(source).toContain("getMapPreviewViz");
    expect(source).toContain("docsGraph");
    expect(source).toContain("docGraphToLoad");
    expect(source).toContain("CodeGraphCanvas");
    expect(source).toContain('"files"');
    expect(source).toContain('"symbols"');
    expect(source).toContain('"subsystems"');
    expect(source).toContain('"docgraph"');
    expect(source).not.toContain("VisualCommitRow");
    expect(source).not.toMatch(/from ["'].*\/GraphRenderer["']/);
  });

  it("exposes docs search, broken links and link candidates with honesty", () => {
    expect(source).toContain('"docs"');
    expect(source).toContain('"links"');
    expect(source).toContain("docsSearch");
    expect(source).toContain("docsBrokenLinks");
    expect(source).toContain("docsStatusHonesty");
    expect(source).toContain("docsSearchHonesty");
    expect(source).toContain("brokenLinksHonesty");
    expect(source).toContain("getWorkspaceLinkCandidates");
    expect(source).toContain("linkCandidatesHonesty");
    expect(source).toContain('data-testid="docs-panel"');
    expect(source).toContain('data-testid="link-candidates-panel"');
  });

  it("disables Build and Refresh when the CLI is known absent", () => {
    expect(source).toContain("cliKnownAbsent");
    // The reason text is owned by buildBlock.ts (asserted in buildBlock.test.ts).
    expect(source).toContain("buildBlockReason(cliKnownAbsent, indexPhase)");
    expect(source).toContain("openSetupWizard");
    expect(source).toContain("classifyMapFailure");
    expect(source).toContain("mapFailureMessage");
    expect(source).toContain('mapFailureMessage("no_cli")');
    expect(source).toContain("mapFailureMode");
  });

  it("keys dead-symbol and path lists with an index so map duplicates cannot crash the pane", () => {
    // preferredDeadLists dedupes, and the template still suffixes `#${i}` —
    // either layer alone is enough; both stay so a regression in one cannot
    // bring back each_key_duplicate (svelte.dev/e/each_key_duplicate).
    expect(source).toContain("deadLists.deadSymbols as id, i");
    expect(source).toContain("deadLists.unwired as path, i");
    expect(source).toContain("selected.entry_points as path, i");
  });
});

describe("RepoMapPanel bounds process output before the status strip", () => {
  /**
   * Derived: find every place the panel renders a `reason`, and require each
   * one to pass through the clip. A hand-listed set would miss the next one.
   */
  it("routes every rendered reason through boundStrip", () => {
    expect(source).toContain("boundStrip");
    // Only the TEMPLATE renders; the script block builds diagnostic payloads
    // that legitimately carry a raw reason, and `reason:` marks those — a
    // property being written, not a value being shown.
    const template = source.slice(source.lastIndexOf("</script>"));
    expect(template.length).toBeGreaterThan(0);
    // Bare error state is process output too (`errorMsg` is the raw reason of a
    // failed map read, the others are build and docs failures). Control-flow
    // blocks (`{#if actionNote}`, `{:else}`, `{/if}`) only test the value.
    const unbounded = [
      ...template.matchAll(
        /\{[^{}]*?(?:\b(?:cliStatus|load|snapshot)\??\.reason\b|\b(?:errorMsg|actionNote|docsError|brokenError|linksError)\b)[^{}]*?\}/g,
      ),
    ]
      .map((m) => m[0])
      .filter((expr) => !/^\{[#:/]/.test(expr))
      .filter((expr) => !expr.includes("boundStrip") && !/\breason:/.test(expr));
    expect(
      unbounded,
      `these render a reason without clipping it: ${unbounded.join(" | ")}`,
    ).toEqual([]);
  });

  it("says the index is too slow while automatic indexing is paused, over a loaded map too", () => {
    // A timed-out build pauses the live index for at least ten minutes. Read
    // as "queued" — or, over a map that did load, as nothing at all — the
    // strip hid the one fact that explains why the map stopped updating.
    expect(source).toContain('case "paused":');
    expect(source).toContain('snapshot?.phase === "paused"');
    expect(source).toContain('data-testid="map-index-paused"');
    expect(source).toContain("Index too slow");
    const template = source.slice(source.lastIndexOf("</script>"));
    expect(template).toMatch(/\{:else if indexPaused\}/);
  });

  it("interpolates the failed-build reason through the clip too", () => {
    // devmap's stderr on a failed build is a log, not a sentence.
    expect(source).toContain("The automatic index failed: ${boundStrip(snapshot.reason)}");
    expect(source).not.toContain("The automatic index failed: ${snapshot.reason}");
  });
});

describe("RepoMapPanel template honesty", () => {
  const template = source.slice(source.lastIndexOf("</script>"));

  it("never reads a failed broken-links check as 'None found'", () => {
    expect(source).toContain("brokenLinksSection(");
    expect(source).toContain("brokenError = reportMapFailure(");
    expect(source).toContain('data-testid="docs-broken-error"');
    expect(template).toContain("Couldn't check links. {boundStrip(brokenError)}");
    const empty = template.indexOf('{:else if brokenSection === "empty"}');
    const noneFound = template.indexOf("None found in the indexed vault.");
    expect(empty).toBeGreaterThan(-1);
    expect(noneFound).toBeGreaterThan(empty);
    expect(template.indexOf("None found in the indexed vault.", noneFound + 1)).toBe(-1);
    // Between the empty arm and the claim there is no other branch.
    expect(template.slice(empty + 1, noneFound)).not.toMatch(/\{:else/);
    // The count is only claimed after a read that succeeded.
    expect(template).toContain('{#if brokenSection === "list" || brokenSection === "empty"}');
  });

  it("keeps the indexed-note count when only the links read fails", () => {
    const reload = source.slice(source.indexOf("async function reloadDocs"), source.indexOf("async function runDocsSearch"));
    expect(reload.indexOf("docsStatus = status;")).toBeGreaterThan(-1);
    expect(reload.indexOf("docsStatus = status;")).toBeLessThan(reload.indexOf("await docsBrokenLinks"));
    // ...but a refresh that itself failed clears the previous run's count.
    expect(reload).toContain("statusCommitted = true;");
    expect(reload).toContain("if (!statusCommitted) docsStatus = null;");
    expect(reload.match(/docsStatus = null/g)).toHaveLength(1);
    expect(reload).not.toContain("docsError");
  });
});

describe("RepoMapPanel build gating", () => {
  const template = source.slice(source.lastIndexOf("</script>"));

  it("gates all three build buttons on one reason", () => {
    expect(template.match(/buildBlockedReason != null/g)).toHaveLength(3);
    expect(template).not.toContain("cliKnownAbsent ?");
  });

  it("shows progress, not an invitation to build, while the automatic index runs", () => {
    expect(source).toContain('indexPhase === "scheduled" || indexPhase === "running"');
    const busy = template.indexOf("{#if indexBusy && indexActivity}");
    const progress = template.indexOf('data-testid="map-index-progress"');
    const otherwise = template.indexOf("{:else}", busy);
    const emptyButton = template.indexOf("Build index", busy);
    expect(busy).toBeGreaterThan(-1);
    expect(progress).toBeGreaterThan(busy);
    expect(progress).toBeLessThan(otherwise);
    // The empty-state button lives only in the else branch.
    expect(emptyButton).toBeGreaterThan(otherwise);
  });
});

describe("RepoMapPanel build feedback", () => {
  const template = source.slice(source.lastIndexOf("</script>"));

  it("shows a spinner and a progressive label on the pressed button", () => {
    expect(source).toContain('let buildKind = $state<"build" | "refresh" | null>(null);');
    expect(template).toContain("Building…");
    expect(template).toContain("Refreshing…");
    expect(template).toContain('class="animate-spin');
    expect(template).toContain("Refresh index");
    expect(template).toContain("Build index");
    expect(template).not.toMatch(/\n\s*Build Map\s*\n/);
  });

  it("fades only success notes and clears the timer on teardown", () => {
    expect(source).toContain("SUCCESS_NOTE_MS");
    expect(source).toContain('if (kind === "ok") noteTimer = window.setTimeout(clearNote, SUCCESS_NOTE_MS);');
    expect(source).toContain('if (actionNoteKind === "ok") clearNote();');
    const destroy = source.slice(source.indexOf("onDestroy(() => {"), source.indexOf("onDestroy(detailScope.dispose)"));
    expect(destroy).toContain("window.clearTimeout(noteTimer)");
    // The only scheduled fade is the success one.
    expect(source.match(/window\.setTimeout\(/g)).toHaveLength(1);
    // The note row stays a live region.
    expect(template).toMatch(/\{#if actionNote\}\s*<div[^>]*role="status"/);
  });
});

describe("RepoMapPanel docs search", () => {
  const template = source.slice(source.lastIndexOf("</script>"));

  it("only says 'No hits' through docsSearchPrompt, naming the query it answers", () => {
    expect(source).toContain(
      "docsSearchPrompt(docsQuery, searchedQuery, docsHits.length, docsSearching, docsError !== null)",
    );
    const none = template.indexOf('No hits for "{searchedQuery}".');
    expect(none).toBeGreaterThan(-1);
    expect(template.slice(template.lastIndexOf("{#if", none), none)).toMatch(
      /^\{#if searchPrompt === "none"\}\s*<p[^>]*>\s*$/,
    );
    expect(template.indexOf("No hits", none + 1)).toBe(-1);
    expect(template).toContain("Press Enter to search.");
  });

  it("labels hits that answer an earlier query, through one predicate", () => {
    expect(source).toContain("docsHitsStale(docsQuery, searchedQuery, docsHits.length)");
    expect(template).toContain('data-testid="docs-hits-stale"');
    expect(template).toContain('Results for "{searchedQuery}"');
    // The label and the dimming read the same derived value, not two copies.
    expect(template).toContain("class:opacity-60={hitsStale}");
    expect(template.match(/\{#if hitsStale\}/g)).toHaveLength(1);
  });

  it("keeps the earlier hits when a search fails and shows the failure", () => {
    const run = source.slice(source.indexOf("async function runDocsSearch"), source.indexOf("function rememberDocsSearch"));
    const failed = run.slice(run.indexOf("} catch (err) {"), run.indexOf("} finally {"));
    expect(failed).toContain('docsError = reportMapFailure("search"');
    expect(failed).not.toContain("docsHits = []");
    expect(failed).not.toContain("searchedQuery = ");
    expect(template).toContain('data-testid="docs-search-error"');
    expect(template).toMatch(/\{#if docsError\}\s*<p[^>]*role="alert"[^>]*data-testid="docs-search-error"/);
  });

  it("restores the query, the hits and what they answer when the panel remounts", () => {
    expect(source).toContain("searchedQuery = memo?.searchedQuery ?? null;");
    expect(source).not.toContain("docsSubmittedQuery");
  });

  it("keeps earlier results (dimmed) while the query is edited", () => {
    const effect = source.slice(source.indexOf("const query = docsQuery.trim();"), source.indexOf("function reloadView"));
    expect(effect).toContain('if (query === "")');
    expect(effect).toContain("docsError = null;");
    // The unconditional wipe is gone: docsHits is only cleared inside the empty-query branch.
    expect(effect.slice(0, effect.indexOf('if (query === "")'))).not.toContain("docsHits = []");
    expect(template).toContain("class:opacity-60={hitsStale}");
  });

  it("disables Search for an empty query", () => {
    expect(template).toContain("disabled={docsSearching || !$repoStore.currentPath || !docsQuery.trim()}");
  });
});

const TREEITEM_SELECTED = "aria-selected={row.subsystemIndex === null ? undefined : isSelected}";

describe("RepoMapPanel map view tablist", () => {
  const template = source.slice(source.lastIndexOf("</script>"));

  it("uses the shared tablist helpers instead of seven hand-written tabs", () => {
    expect(source).toContain('tabProps("map-view"');
    expect(source).toContain("handleTablistKeydown(");
    expect(source).toContain("focusTabAt(tablistEl");
    expect(template).not.toMatch(/role="tab"/);
    // Tabs get aria-selected from tabProps. The navigator's tree rows carry their
    // own (aria-selected on a treeitem), pinned in "navigator tree" below.
    expect(template.replace(TREEITEM_SELECTED, "")).not.toMatch(/aria-selected=/);
    expect(source).toContain("const MAP_VIEWS");
    for (const id of ["navigator", "files", "symbols", "subsystems", "docgraph", "docs", "links"]) {
      expect(source).toContain(`{ id: "${id}"`);
    }
  });

  it("scrolls the seven views as tabs instead of a wrapping 11px text row", () => {
    const labels = ["Navigator", "Files", "Symbols", "Subsystems", "Doc graph", "Docs", "Links"];
    const views = source.slice(source.indexOf("const MAP_VIEWS"), source.indexOf("const CODE_GRAPH_VIEWS"));
    const positions = labels.map((label) => views.indexOf(`label: "${label}"`));
    expect(positions.every((at) => at >= 0)).toBe(true);
    expect([...positions].sort((a, b) => a - b)).toEqual(positions);

    const marker = template.indexOf('aria-label="Map views"');
    const open = template.lastIndexOf("<div", marker);
    const tablistTag = template.slice(open, template.indexOf(">", marker) + 1);
    const classSource = tablistTag.match(/class=\{(\[[\s\S]*?\])\}/)?.[1]
      ?? tablistTag.match(/class="([^"]*)"/)?.[1]
      ?? "";
    const tokens = classSource.split(/[\s'"]+/).filter(Boolean);
    // The old row was `flex flex-wrap ... text-[11px]`. `flex-nowrap` is a
    // different token and must not be mistaken for the wrapping class.
    expect(tokens).not.toContain("flex-wrap");
    expect(tokens).not.toContain("text-[11px]");
    expect(tokens).toContain("flex-nowrap");
    expect(tokens).toContain("gp-header-scroll");
    expect(tablistTag).toContain('role="tablist"');

    const stripOpen = template.lastIndexOf("<div", open - 1);
    const stripTag = template.slice(stripOpen, template.indexOf(">", stripOpen) + 1);
    const stripClass = stripTag.match(/class="([^"]*)"/)?.[1] ?? "";
    expect(stripClass).toContain("relative");
    expect(stripClass.split(/\s+/)).not.toContain("border-b");
    expect(stripClass).not.toContain("border-border/50");

    const { body } = render(RepoMapPanel);
    const at = body.indexOf('aria-label="Map views"');
    expect(at).toBeGreaterThan(-1);
    const renderedTag = body.slice(body.lastIndexOf("<div", at), body.indexOf(">", at) + 1);
    const renderedClass = renderedTag.match(/class="([^"]*)"/)?.[1] ?? "";
    expect(renderedClass.split(/\s+/)).not.toContain("flex-wrap");
    expect(renderedClass).not.toContain("text-[11px]");
    const region = body.slice(body.lastIndexOf("<div", at), body.indexOf('role="tabpanel"', at));
    const tabs = [...region.matchAll(/<button\b([^>]*)>([\s\S]*?)<\/button>/g)].filter((match) =>
      /\brole="tab"/.test(match[1] ?? ""),
    );
    expect(tabs).toHaveLength(7);
    expect(tabs.map((match) => (match[2] ?? "").replace(/<[^>]+>/g, "").trim())).toEqual(labels);
    expect(tabs[0]?.[1]).toContain('aria-selected="true"');
    expect(tabs[0]?.[1]).toContain('tabindex="0"');
    for (const tab of tabs.slice(1)) {
      expect(tab[1]).toContain('aria-selected="false"');
      expect(tab[1]).toContain('tabindex="-1"');
    }
  });

  it("wraps the whole content chain in the tabpanel the selected tab controls", () => {
    expect(template).toContain('role="tabpanel"');
    expect(template).toContain('id={panelId("map-view", mapView)}');
    expect(template).toContain('aria-labelledby={tabId("map-view", mapView)}');
    const panel = template.indexOf('role="tabpanel"');
    const chain = template.indexOf("{#if CODE_GRAPH_VIEWS.has(mapView)}");
    expect(chain).toBeGreaterThan(panel);
    // Nothing but the panel's own opening tag sits between them.
    expect(template.slice(panel, chain)).not.toMatch(/\{#if/);
  });
});

function deferred<T>(): {
  promise: Promise<T>;
  resolve: (value: T) => void;
} {
  let resolve: (value: T) => void = () => {};
  const promise = new Promise<T>((res) => {
    resolve = res;
  });
  return { promise, resolve };
}

describe("RepoMapPanel map reload does not wait on devmap status", () => {
  it("applies the map while the status invoke is still stalled", async () => {
    const status = deferred<string>();
    const applied: string[] = [];
    const done = settleMapBeforeStatus({
      map: Promise.resolve({ area: "NEWER-MAP-AREA" }),
      status: status.promise,
      isCurrent: () => true,
      onMap: (result) => {
        applied.push(result.status === "fulfilled" ? result.value.area : "rejected");
      },
      onStatus: () => {
        applied.push("status");
      },
    });
    await done;
    expect(applied).toEqual(["NEWER-MAP-AREA"]);
    status.resolve("fresh");
    await Promise.resolve();
    await Promise.resolve();
    expect(applied).toEqual(["NEWER-MAP-AREA", "status"]);
  }, 1000);

  it("a superseded reload does not apply after the newer map has settled", async () => {
    let generation = 0;
    const start = () => {
      const token = ++generation;
      const map = deferred<{ area: string }>();
      const status = deferred<string>();
      const applied: string[] = [];
      const done = settleMapBeforeStatus({
        map: map.promise,
        status: status.promise,
        isCurrent: () => token === generation,
        onMap: (result) => {
          if (result.status === "fulfilled") applied.push(result.value.area);
        },
        onStatus: () => {
          applied.push("status");
        },
      });
      return { map, status, applied, done };
    };
    const first = start();
    const newer = start();
    newer.map.resolve({ area: "NEWER-MAP-AREA" });
    await newer.done;
    expect(newer.applied).toEqual(["NEWER-MAP-AREA"]);
    first.map.resolve({ area: "STALE-MAP-AREA" });
    first.status.resolve("old");
    await Promise.resolve();
    await Promise.resolve();
    expect(first.applied).toEqual([]);
    expect(newer.applied).toEqual(["NEWER-MAP-AREA"]);
    newer.status.resolve("fresh");
    await Promise.resolve();
    await Promise.resolve();
    expect(newer.applied).toEqual(["NEWER-MAP-AREA", "status"]);
    expect(first.applied).toEqual([]);
  }, 1000);

  it("clears loading from the map settlement, not from the status probe", () => {
    const reload = source.slice(
      source.indexOf("async function reload("),
      source.indexOf("async function reloadGraph("),
    );
    expect(reload).toContain("settleMapBeforeStatus(");
    expect(reload).not.toContain("Promise.all");
    const onMap = reload.indexOf("onMap(mapResult)");
    const onStatus = reload.indexOf("onStatus(statusResult)");
    expect(onMap).toBeGreaterThan(-1);
    expect(onStatus).toBeGreaterThan(onMap);
    expect(reload.slice(onMap, onStatus)).toContain("loading = false");
    expect(reload.slice(onStatus)).not.toContain("loading");
  });
});

describe("RepoMapPanel view switching", () => {
  it("drops the previous view's graph before requesting the new one", () => {
    // The view effect, not reloadGraph (which also begins `const view = mapView;`).
    const start = source.indexOf("const view = mapView;\n    const path = currentRepo;");
    const end = source.indexOf("reloadView(path, view)", start);
    expect(start).toBeGreaterThan(-1);
    expect(end).toBeGreaterThan(start);
    expect(source.slice(start, end)).toContain("graphLoad = null;");
  });
});

describe("RepoMapPanel navigator tree", () => {
  const template = source.slice(source.lastIndexOf("</script>"));
  const nav = template.slice(template.indexOf('aria-label="Subsystems"'), template.indexOf("<!-- Detail -->"));

  it("renders the area tree, not the flat subsystem list keyed by area string", () => {
    expect(source).toContain("buildAreaTree(map.subsystems)");
    expect(nav).toContain("{#each visibleRows as row (row.key)}");
    expect(template).not.toContain("{#each doc.subsystems as sub");
    expect(source).not.toContain("selectedArea");
    expect(source).not.toContain("s.area === selectedArea");
  });

  it("is an ARIA tree with a roving tab stop", () => {
    expect(nav).toContain('role="tree"');
    expect(nav).toContain('role="treeitem"');
    expect(nav).toContain("aria-level={row.depth + 1}");
    expect(nav).toContain("aria-expanded={row.hasChildren ? open : undefined}");
    expect(nav).toContain("aria-selected={row.subsystemIndex === null ? undefined : isSelected}");
    expect(nav).toContain("tabindex={row.key === tabStopKey ? 0 : -1}");
    expect(nav).toContain("onkeydown={(e) => onRowKeydown(e, row)}");
    expect(source).toContain("treeKeyOutcome(e.key, visibleRows, row.key, treeCollapsed)");
  });

  it("selects by row key and keeps the first subsystem as the default", () => {
    expect(source).toContain("areaRows.find((r) => r.subsystemIndex === 0)");
    expect(source).toContain("map.subsystems[selectedRow.subsystemIndex]");
    // A group row only expands and collapses.
    expect(source).toContain("if (row.subsystemIndex === null) toggleRow(row.key);");
  });

  it("shows duplicates as a count, the full area in the title and the summary without its area", () => {
    expect(nav).toContain("row.duplicateCount > 1");
    expect(nav).toContain("title={row.area}");
    expect(nav).toContain("summaryWithoutArea(doc.subsystems[row.subsystemIndex])");
    expect(template).toContain("summaryWithoutArea(selected)");
  });

  it("filters through the same visibility function the keyboard uses, with a real field class", () => {
    expect(source).toContain("visibleAreaRows(areaRows, collapsed, treeFilter)");
    expect(nav).toContain('class="gp-field w-full"');
    expect(nav).toContain("bind:value={treeFilter}");
    expect(template).not.toContain("gp-input");
  });

  it("keeps the cap line as a visible chip that names what was dropped", () => {
    expect(nav).toContain('data-testid="map-subsystems-cap"');
    expect(nav).toContain("Showing {doc.liveness_meta.subsystems.shown} of {doc.liveness_meta.subsystems.total}");
    expect(nav).toContain("dropped (no area)");
  });

  it("turns neighbors that exist in the map into buttons that select them", () => {
    expect(template).toContain("neighborRowKey(areaRows, neighbor)");
    expect(template).toContain("onclick={() => selectKey(neighborKey)}");
    // A neighbor outside the map stays text.
    expect(template).toMatch(/\{:else\}\s*<span class="gp-chip font-mono text-textMuted"/);
    expect(template).not.toContain("selected.neighbors.join");
  });

  it("selecting a neighbor opens whatever hides it", () => {
    const select = source.slice(source.indexOf("function selectKey"), source.indexOf("function toggleRow"));
    expect(select).toContain("ancestorKeys(areaRows, key)");
    expect(select).toContain('treeFilter = "";');
    expect(select).toContain("selectedKey = key;");
  });

  it("Show on map switches to the subsystems view and hands the canvas the node id", () => {
    const show = source.slice(source.indexOf("function showOnMap"), source.indexOf("function openFile"));
    expect(show).toContain("focusNodeId = subsystemNodeId(sub.area);");
    expect(show).toContain('setView("subsystems");');
    expect(template).toContain("onclick={() => showOnMap(selected)}");
    expect(template).toContain('focusNodeId={mapView === "subsystems" ? focusNodeId : null}');
    // The request does not outlive the subsystems view.
    expect(source).toContain('if (view !== "subsystems") focusNodeId = null;');
  });

  it("leaves no uppercase labels, palette colours or hairline region separators in the template", () => {
    expect(template).not.toMatch(/uppercase|tracking-wide/);
    expect(template).not.toMatch(/\b(?:text|bg|border)-(?:amber|rose|emerald|red|green|yellow)-\d/);
    expect(template).not.toMatch(/dark:/);
    // The view tablist is another owner's block (header segmented tabs).
    const outsideTablist = template.replace(/<!-- View switcher[\s\S]*?(?=\n  \{#if actionNote\})/, "");
    expect(outsideTablist.length).toBeLessThan(template.length);
    expect(outsideTablist).not.toMatch(/(?:^|\s)border-[btlr](?:\s|")/);
  });

  it("uses the shared notice, empty and loading shapes and plain-word copy", () => {
    expect(template).toContain("gp-notice gp-notice-warn");
    expect(template).toContain("<EmptyState");
    expect(template).toContain("No code map yet");
    expect(template).not.toContain("Loading…");
    expect(template).not.toContain(">Retry<");
    expect(template).toContain(">Try again<");
    expect(template).not.toMatch(/title="(?:schema_outdated|coverage_gaps|generation_id|devmap status is_fresh)"/);
    expect(template).not.toContain("devmap CLI missing");
  });
});

describe("CodeView map section", () => {
  it("lazy-loads the Map panel beside Explorer and Blame", () => {
    expect(codeView).toContain('section === "map"');
    expect(codeView).toContain("loadMap");
  });
});
