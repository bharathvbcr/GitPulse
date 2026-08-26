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
});
