import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import CommitTable from "./CommitTable.svelte";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "CommitTable.svelte"),
  "utf8",
);

describe("CommitTable graph node hover", () => {
  it("does not put a title on the graph viewport that would steal node tooltips", () => {
    // The global Tooltip upgrades every title= via closest(). A layout hint
    // on the gutter would replace GraphNodeTooltip on every branch-lane hover.
    expect(source).not.toMatch(/title=\{graphLayout\.isHorizontallyScrollable/);
    expect(source).not.toContain("Wide graph — scroll horizontally to see more lanes");
  });

  it("portals the node tooltip to body so gp-pane paint containment cannot clip it", () => {
    expect(source).toContain('use:portal={"body"}');
    expect(source).toContain("LAYERS.TOOLTIP");
    expect(source).toContain("position.left + rootRect.left");
  });

  it("hit-tests in content space, including horizontal gutter pan", () => {
    expect(source).toContain("bind:this={graphViewport}");
    expect(source).toContain("canvasPointFromClient");
    expect(source).toContain("graphViewport.scrollLeft");
    expect(source).toContain("onscroll={handleGraphScroll}");
  });

  it("does not let $state-proxied scrollLeft retrigger the clamp effect", () => {
    const effect = source.slice(
      source.indexOf("graphViewportWidth;\n    graphContentWidth;"),
      source.indexOf("$effect(() => {\n    filteredRows;"),
    );
    expect(effect).toContain("untrack(() => {");
    expect(effect).toContain("viewport.scrollLeft = next");
    expect(effect.indexOf("untrack(() => {")).toBeLessThan(effect.indexOf("viewport.scrollLeft = next"));
  });
});

describe("CommitTable keyboard parity for graph context", () => {
  it("shows the same tooltip card on keyboard row focus, not only on pointer hover", () => {
    expect(source).toContain("onFocusRow");
    expect(source).toContain("onBlurRow");
    expect(source).toContain("handleRowFocus");
    expect(source).toContain('tooltipSource = "focus"');
  });

  it("announces the focused commit's graph context through a live region", () => {
    expect(source).toContain('aria-live="polite"');
    const { body } = render(CommitTable);
    expect(body).toContain('aria-live="polite"');
  });

  it("exposes the focus card to assistive tech while pointer cards stay hidden", () => {
    // aria-hidden must be conditional on the tooltip's source: a keyboard
    // user's card is their UI, not a duplicate of something they can see.
    expect(source).toMatch(/aria-hidden=\{tooltipSource/);
  });

  it("hands each row its merge destination so the relationship is in the accessible path", () => {
    expect(source).toContain("closeTargetById");
    expect(source).toMatch(/mergeTarget=\{closeTargetById\.get\(row\.id\)/);
  });

  it("names the pinned mainline and explains fading stubs in both the card and the announcement", () => {
    // The tooltip and the live region must say the same things: which
    // branch the straight rail is, and that a stub means the parent is past
    // the loaded window (with the load-more hint when older history exists).
    // Filters never cause stubs: every term runs in the backend, which
    // relinks survivors to their nearest kept ancestors.
    expect(source).toContain("mainlineName={$graphStore.mainlineName}");
    expect(source).toContain("the first-parent line");
    expect(source).toContain("hasMore={$graphStore.hasMore}");
    expect(source).toContain("outside the loaded history");
    expect(source).not.toContain("hidden by the current filter");
  });

  it("renders the backend's rows as-is: no client-side re-filtering", () => {
    // A second filter implementation on the client drifted from the
    // backend's and dropped rows AFTER lanes were solved, leaving every
    // survivor's edges as stubs. The payload is the view.
    expect(source).toContain("let filteredRows = $derived($graphStore.rows);");
    expect(source).not.toMatch(/filter\/(queryMemo|parseQuery)/);
    expect(source).not.toContain("createRowFilterMemo");
  });
});

describe("CommitTable graph horizontal overflow", () => {
  it("caps the gutter as a flex item so extra lanes overflow instead of shoving commits", () => {
    // Flex items default to min-width:auto, which is the canvas content
    // width. Without an explicit 0-floor and a max-width lock, a 40-lane
    // graph grows the column and the commit list disappears off the right.
    expect(source).toContain("graphViewportBoxStyle");
    expect(source).toContain("graphContentBoxStyle");
    expect(source).toContain("min-w-0");
    expect(source).toContain("applyGraphGutterWheel");
    expect(source).toContain("panGraphHorizontally");
  });

  it("does not GPU-promote the scroll container (transform kills overflow in WKWebView)", () => {
    const classMatch = source.match(
      /bind:this=\{graphViewport\}\s+class="([^"]+)"/,
    );
    expect(classMatch?.[1]).toContain("overflow-x-auto");
    expect(classMatch?.[1]).not.toContain("gp-gpu");
  });

  it("renders the capped gutter as a horizontally scrollable pane", () => {
    const { body } = render(CommitTable);
    expect(body).toContain("gp-graph-hscroll");
    expect(body).toContain("overflow-x-auto");
    expect(body).toContain("min-width:0px");
    expect(body).toContain("max-width:");
    expect(body).toContain("flex-basis:");
  });

  it("advertises horizontal overflow with the shared chevron cue, not a title", () => {
    expect(source).toContain("ScrollCue");
    expect(source).toContain("hint={graphOverflow}");
    expect(source).not.toContain("showStartFade");
    expect(source).not.toContain("gp-edge-fade-start");
  });
});

describe("tag standing on graph chips", () => {
  it("reuses the loaded tag listing instead of asking git again", () => {
    // The graph reloads on every repo switch; a second command here would be a
    // per-load cost for a chip. repoStore.tags already carries the comparison.
    expect(source).toContain("for (const tag of $repoStore.tags)");
    expect(source).not.toContain("cmd_tag_cleanup_plan");
    expect(source).not.toContain("cmd_list_tags");
  });

  it("attaches the standing only to tag refs", () => {
    expect(source).toContain('const standing = kind === "tag" ? tagStanding.get(ref.name) : undefined;');
    // Absent stays absent: null, not a zero that would read as "merged".
    expect(source).toContain("aheadOfBase: standing?.ahead ?? null");
    expect(source).toContain("comparedTo: standing?.base ?? null");
  });
});

describe("CommitTable palette invalidation", () => {
  it("drops cached graph tiles only when the theme or accent changes", () => {
    // interfaceStore publishes a fresh object per preference write, so
    // `$interfaceStore.accent` inside the effect invalidated every tile on
    // unrelated emissions. The accent is read through a strictly-equal derived.
    expect(source).toContain("const graphAccent = $derived($interfaceStore.accent);");
    const marker = source.indexOf("    graphAccent;\n");
    expect(marker).toBeGreaterThan(-1);
    const start = source.lastIndexOf("$effect(() => {", marker);
    const effect = source.slice(start, source.indexOf("\n  });\n", marker));
    expect(effect).toContain("$themeStore;");
    expect(effect).toContain("graphCache.invalidate();");
    expect(effect).not.toContain("$interfaceStore");
  });
});

describe("CommitTable failed and loading reloads", () => {
  it("exposes the error banner and progress bar to assistive tech", () => {
    const banner = source.slice(
      source.indexOf("{#if $graphStore.error && !$graphStore.isLoading}"),
      source.indexOf("class=\"relative z-10 flex min-w-0 shrink-0"),
    );
    expect(banner).toContain('role="alert"');
    const bar = source.slice(source.indexOf("{#if $graphStore.isLoading}"), source.indexOf("{#if $graphStore.error && !$graphStore.isLoading}"));
    expect(bar).toContain('role="status"');
    expect(bar).toContain('aria-label="Loading commits"');
    expect(source).toContain("aria-busy={$graphStore.isLoading}");
  });

  it("retries the failed request at its own page size", () => {
    expect(source).toContain("$graphStore.errorRequest?.limit");
    expect(source).toContain("{ forceLimit: limit }");
  });

  it("dims the previous filter's rows and says so when a refining reload failed", () => {
    expect(source).toContain("$graphStore.errorRequest?.refining === true");
    expect(source).toContain("{previousFilterShown ? 'opacity-60' : ''}");
    expect(source).toContain("Showing the previous filter's results — the new filter failed to load.");
  });
});

describe("CommitTable scroll on a new filter", () => {
  const start = source.indexOf("const listRepoPath = $derived");
  const block = source.slice(start, source.indexOf("// Theme changes are rare"));

  it("keys the reset on the normalized query and branch, and records a repository switch without scrolling", () => {
    expect(start).toBeGreaterThan(-1);
    expect(block).toContain("normalizeGraphQuery($filterStore.searchQuery)");
    expect(block).toContain('$filterStore.selectedBranch ?? ""');
    const switchBranch = block.slice(block.indexOf("path !== landedPath"), block.indexOf("if (loading) return;"));
    expect(switchBranch).toContain("landedSignature = signature;");
    expect(switchBranch).toContain("return;");
    expect(switchBranch).not.toContain("scrollTop");
  });

  it("scrolls only when rows land under a changed signature, never mid-load or on a same-signature reload", () => {
    expect(block).toContain("if (loading) return;");
    expect(block).toContain("if (!landed || signature === landedSignature) return;");
    expect(block).toContain("scrollTopForNewFilter({");
    expect(block).toContain("container.scrollTop = next;");
    expect(block).toContain("scrollTop = next;");
    expect(block.indexOf("if (loading) return;")).toBeLessThan(block.indexOf("scrollTopForNewFilter({"));
  });
});

describe("CommitTable single current commit", () => {
  it("highlights the row from the shared resolver, not from the repo store's id alone", () => {
    expect(source).toContain("resolveCurrentCommitId");
    expect(source).toContain("isSelected={currentId === row.id}");
    expect(source).not.toContain("isSelected={$repoStore.selectedCommitId === row.id}");
    const selectedIdFn = source.slice(source.indexOf("function selectedId()"), source.indexOf("function paintNow()"));
    expect(selectedIdFn).toContain("return currentId;");
  });

  it("reconciles the two stores on id changes only, reading everything else untracked", () => {
    const start = source.indexOf("let reconcileSeen");
    expect(start).toBeGreaterThan(-1);
    const effect = source.slice(start, source.indexOf("// A new query or branch lands"));
    expect(effect).toContain("planCommitReconcile(");
    expect(effect).toContain("untrack(() => {");
    expect(effect).toContain("get(repoStore)");
    expect(effect).toContain("graphStore.selectCommit(row)");
    expect(effect).toContain("repoStore.selectCommitDiff(plan.loadDiffId)");
    expect(effect).not.toContain("$repoStore");
  });
});
