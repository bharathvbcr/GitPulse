import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { compile } from "svelte/compiler";

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
    expect(source).toContain("coverage_gaps");
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
    expect(template).toContain("Could not check links. {boundStrip(brokenError)}");
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

describe("RepoMapPanel map view tablist", () => {
  const template = source.slice(source.lastIndexOf("</script>"));

  it("uses the shared tablist helpers instead of seven hand-written tabs", () => {
    expect(source).toContain('tabProps("map-view"');
    expect(source).toContain("handleTablistKeydown(");
    expect(source).toContain("focusTabAt(tablistEl");
    expect(template).not.toMatch(/role="tab"/);
    expect(template).not.toMatch(/aria-selected=/);
    expect(source).toContain("const MAP_VIEWS");
    for (const id of ["navigator", "files", "symbols", "subsystems", "docgraph", "docs", "links"]) {
      expect(source).toContain(`{ id: "${id}"`);
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

describe("CodeView map section", () => {
  it("lazy-loads the Map panel beside Explorer and Blame", () => {
    expect(codeView).toContain('section === "map"');
    expect(codeView).toContain("loadMap");
  });
});
