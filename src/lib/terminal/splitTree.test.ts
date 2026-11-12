import { describe, expect, it } from "vitest";
import {
  adjacentPane,
  clampSplitRatio,
  computeSplitLayout,
  equalizeSplits,
  hasPane,
  hydrateSplitTree,
  leafPaneIds,
  MAX_SPLIT_PANES,
  movePaneToSplit,
  removePane,
  replacePane,
  resizeBranch,
  serializeSplitTree,
  SPLIT_RATIO_MAX,
  SPLIT_RATIO_MIN,
  splitPane,
  swapPanes,
  visiblePaneIds,
  minimumSplitHeight,
  MIN_PANE_HEIGHT_PX,
} from "./splitTree";

describe("minimumSplitHeight", () => {
  const min = MIN_PANE_HEIGHT_PX;

  it("asks one pane's minimum for no split, a lone leaf and a zoomed pane", () => {
    expect(minimumSplitHeight(null, "p1")).toBe(min);
    expect(minimumSplitHeight(null, null)).toBe(min);
    const root = splitPane(null, "p1", "p2", "col", "after", 0.5);
    expect(minimumSplitHeight(root, "p1", "p1")).toBe(min);
  });

  it("stacks the minimums of a split down and keeps the taller side of a split right", () => {
    const down = splitPane(null, "p1", "p2", "col", "after", 0.5);
    expect(minimumSplitHeight(down, "p1")).toBe(2 * min);
    const right = splitPane(null, "p1", "p2", "row", "after", 0.5);
    expect(minimumSplitHeight(right, "p1")).toBe(min);
    // A split right whose right side is split down needs that side's height.
    const nested = splitPane(right, "p2", "p3", "col", "after", 0.5);
    expect(minimumSplitHeight(nested, "p1")).toBe(2 * min);
  });

  it("honours an uneven ratio, so the smaller side still gets its minimum", () => {
    const uneven = splitPane(null, "p1", "p2", "col", "after", 0.25);
    const total = minimumSplitHeight(uneven, "p1");
    const boxes = computeSplitLayout(uneven, "p1").panes;
    for (const id of ["p1", "p2"]) expect(boxes[id].height * total).toBeGreaterThanOrEqual(min - 1e-9);
    expect(total).toBeCloseTo(min / 0.25);
  });

  it("gives every pane of a full grid at least its minimum at the returned height", () => {
    let root = splitPane(null, "p1", "p2", "col", "after", 0.3);
    root = splitPane(root, "p2", "p3", "col", "after", 0.6);
    root = splitPane(root, "p1", "p4", "row", "after", 0.5);
    const total = minimumSplitHeight(root, "p1");
    for (const box of Object.values(computeSplitLayout(root, "p1").panes)) {
      expect(box.height * total).toBeGreaterThanOrEqual(min - 1e-9);
    }
  });
});

describe("splitTree", () => {
  it("creates a 2-pane split from null and preserves order for before/after", () => {
    const right = splitPane(null, "p1", "p2", "row", "after");
    expect(right?.kind).toBe("branch");
    expect(leafPaneIds(right)).toEqual(["p1", "p2"]);

    const left = splitPane(null, "p1", "p2", "row", "before");
    expect(leafPaneIds(left)).toEqual(["p2", "p1"]);
  });

  it("supports recursive nested row/col splits up to MAX_SPLIT_PANES and refuses past the ceiling", () => {
    let root = splitPane(null, "p1", "p2", "row", "after");
    root = splitPane(root, "p2", "p3", "col", "after");
    expect(leafPaneIds(root)).toEqual(["p1", "p2", "p3"]);

    for (let i = 4; i <= MAX_SPLIT_PANES; i += 1) {
      root = splitPane(root, `p${i - 1}`, `p${i}`, i % 2 === 0 ? "row" : "col");
    }
    expect(leafPaneIds(root)).toHaveLength(MAX_SPLIT_PANES);

    // Past MAX_SPLIT_PANES returns the exact same tree reference.
    const refused = splitPane(root, "p1", "p-overflow", "row");
    expect(refused).toBe(root);
  });

  it("refuses duplicate or self splits without mutating", () => {
    const root = splitPane(null, "p1", "p2", "row");
    expect(splitPane(root, "p1", "p2", "col")).toBe(root);
    expect(splitPane(root, "p1", "p1", "row")).toBe(root);
    expect(splitPane(null, "p1", "p1", "row")).toBeNull();
  });

  it("removes a leaf, collapses its parent branch, and normalizes a lone survivor back to null", () => {
    let root = splitPane(null, "p1", "p2", "row");
    root = splitPane(root, "p2", "p3", "col");
    expect(leafPaneIds(root)).toEqual(["p1", "p2", "p3"]);

    const twoLeft = removePane(root, "p2");
    expect(twoLeft?.kind).toBe("branch");
    expect(leafPaneIds(twoLeft)).toEqual(["p1", "p3"]);

    const lone = removePane(twoLeft, "p3");
    expect(lone).toBeNull();
  });

  it("clamps branch ratios to [0.15, 0.85] and handles NaN/equalize", () => {
    expect(clampSplitRatio(0.01)).toBe(SPLIT_RATIO_MIN);
    expect(clampSplitRatio(0.99)).toBe(SPLIT_RATIO_MAX);
    expect(clampSplitRatio(Number.NaN)).toBe(0.5);

    const root = splitPane(null, "p1", "p2", "row", "after", 0.7);
    expect(root?.kind).toBe("branch");
    if (root?.kind !== "branch") return;

    const resized = resizeBranch(root, root.id, 0.02);
    expect(resized?.kind === "branch" && resized.ratio).toBe(SPLIT_RATIO_MIN);

    const equalized = equalizeSplits(resized);
    expect(equalized?.kind === "branch" && equalized.ratio).toBe(0.5);
  });

  it("replaces and swaps panes cleanly", () => {
    let root = splitPane(null, "p1", "p2", "row");
    root = splitPane(root, "p2", "p3", "col");

    // Replacing with an already-visible pane is a no-op so duplicate leaves cannot form.
    expect(replacePane(root, "p1", "p3")).toBe(root);

    const replaced = replacePane(root, "p2", "p4");
    expect(leafPaneIds(replaced)).toEqual(["p1", "p4", "p3"]);
    expect(hasPane(replaced, "p2")).toBe(false);

    const swapped = swapPanes(replaced, "p1", "p3");
    expect(leafPaneIds(swapped)).toEqual(["p3", "p4", "p1"]);
  });

  it("moves an existing split pane to a new split edge without duplicating it", () => {
    let root = splitPane(null, "p1", "p2", "row");
    root = splitPane(root, "p2", "p3", "row");
    const moved = movePaneToSplit(root, "p3", "p1", "col", "before");
    expect(leafPaneIds(moved)).toEqual(["p3", "p1", "p2"]);
  });

  it("computes normalized layout boxes, sashes, and zoom overrides", () => {
    let root = splitPane(null, "left", "topRight", "row", "after", 0.4);
    root = splitPane(root, "topRight", "bottomRight", "col", "after", 0.25);

    const layout = computeSplitLayout(root, "left", null);
    expect(layout.sashes).toHaveLength(2);
    expect(layout.panes.left).toEqual({ left: 0, top: 0, width: 0.4, height: 1 });
    expect(layout.panes.topRight.left).toBeCloseTo(0.4);
    expect(layout.panes.topRight.width).toBeCloseTo(0.6);
    expect(layout.panes.topRight.height).toBeCloseTo(0.25);
    expect(layout.panes.bottomRight.top).toBeCloseTo(0.25);
    expect(layout.panes.bottomRight.height).toBeCloseTo(0.75);

    // Zooming one pane hides sashes and expands that pane to 100% without mutating root.
    expect(visiblePaneIds(root, "left", "bottomRight")).toEqual(["bottomRight"]);
    const zoomed = computeSplitLayout(root, "left", "bottomRight");
    expect(zoomed.sashes).toHaveLength(0);
    expect(zoomed.panes).toEqual({
      bottomRight: { left: 0, top: 0, width: 1, height: 1 },
    });
  });

  it("navigates spatially across 2D split geometry", () => {
    let root = splitPane(null, "left", "topRight", "row", "after", 0.5);
    root = splitPane(root, "topRight", "bottomRight", "col", "after", 0.5);

    expect(adjacentPane(root, "left", "right")).toBe("topRight");
    expect(adjacentPane(root, "left", "left")).toBeNull();
    expect(adjacentPane(root, "topRight", "down")).toBe("bottomRight");
    expect(adjacentPane(root, "bottomRight", "up")).toBe("topRight");
    expect(adjacentPane(root, "bottomRight", "left")).toBe("left");
  });

  it("serializes and hydrates safely, pruning missing or duplicate panes", () => {
    let root = splitPane(null, "p1", "p2", "row", "after", 0.6);
    root = splitPane(root, "p2", "p3", "col", "after", 0.3);
    const raw = serializeSplitTree(root);

    const hydratedFull = hydrateSplitTree(raw, new Set(["p1", "p2", "p3"]));
    expect(leafPaneIds(hydratedFull)).toEqual(["p1", "p2", "p3"]);

    // p2 closed while persisted: collapses cleanly to [p1, p3].
    const hydratedPartial = hydrateSplitTree(raw, new Set(["p1", "p3"]));
    expect(leafPaneIds(hydratedPartial)).toEqual(["p1", "p3"]);

    // Only 1 valid pane left: collapses to null (unsplit).
    expect(hydrateSplitTree(raw, new Set(["p1"]))).toBeNull();
    expect(hydrateSplitTree("{corrupt", new Set(["p1", "p2"]))).toBeNull();
  });

  it("serializes and remaps v2 persisted split workspaces across cold-boot tab id changes", async () => {
    const mod = await import("./splitTree");
    const root = splitPane(null, "old-shell", "old-file", "row", "after", 0.55);
    const raw = mod.serializeSplitWorkspace(root, {
      "old-shell": { launcher: "shell", slotIndex: 0, backendSessionId: "ptyd-42" },
      "old-file": { launcher: "file", slotIndex: 0, filePath: "src/App.svelte", revealLine: 19 },
    });
    expect(raw).not.toBeNull();
    const parsed = mod.parsePersistedSplitWorkspace(raw);
    expect(parsed?.leaves["old-shell"]).toMatchObject({ launcher: "shell", backendSessionId: "ptyd-42" });
    expect(parsed?.leaves["old-file"]).toMatchObject({ launcher: "file", filePath: "src/App.svelte", revealLine: 19 });

    const remapped = mod.remapSplitTree(
      parsed?.tree ?? null,
      new Map([
        ["old-shell", "tab-1"],
        ["old-file", "tab-2"],
      ]),
    );
    expect(leafPaneIds(remapped)).toEqual(["tab-1", "tab-2"]);
    expect(remapped?.kind === "branch" && remapped.ratio).toBeCloseTo(0.55);
  });

  it("parses macOS and Linux/Windows split and zoom chords without stealing shell Ctrl+D", async () => {
    const { terminalSplitChord } = await import("./splitTree");
    // Plain Ctrl+D is EOF in shells — never intercepted!
    expect(
      terminalSplitChord(
        { key: "d", ctrlKey: true, metaKey: false, altKey: false, shiftKey: false },
        "macos",
      ),
    ).toBeNull();
    expect(
      terminalSplitChord(
        { key: "d", ctrlKey: false, metaKey: true, altKey: false, shiftKey: false },
        "macos",
      ),
    ).toBe("split-right");
    expect(
      terminalSplitChord(
        { key: "D", ctrlKey: false, metaKey: true, altKey: false, shiftKey: true },
        "macos",
      ),
    ).toBe("split-down");
    expect(
      terminalSplitChord(
        { key: "Enter", ctrlKey: false, metaKey: true, altKey: false, shiftKey: true },
        "macos",
      ),
    ).toBe("zoom-pane");
    expect(
      terminalSplitChord(
        { key: "ArrowRight", ctrlKey: false, metaKey: true, altKey: true, shiftKey: false },
        "macos",
      ),
    ).toBe("focus-right");
    expect(
      terminalSplitChord(
        { key: "D", ctrlKey: true, metaKey: false, altKey: false, shiftKey: true },
        "linux",
      ),
    ).toBe("split-right");
  });
});
