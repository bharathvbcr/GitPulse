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

    // Round-trips sanitized title metadata (name, launchName, userNamed, title) and ignores bogus types.
    const titledRaw = mod.serializeSplitWorkspace(root, {
      "old-shell": {
        launcher: "claude",
        slotIndex: 0,
        backendSessionId: "ptyd-99",
        name: "Pinned\u0007 Name",
        launchName: "Claude · feat",
        userNamed: true,
        title: "✳ Running\u200b tests",
      },
    });
    const parsedTitled = mod.parsePersistedSplitWorkspace(titledRaw);
    expect(parsedTitled?.leaves["old-shell"]).toMatchObject({
      launcher: "claude",
      backendSessionId: "ptyd-99",
      name: "Pinned Name",
      launchName: "Claude · feat",
      userNamed: true,
      title: "✳ Running tests",
    });

    const malformedJson = JSON.stringify({
      version: 2,
      tree: root,
      leaves: {
        "old-shell": {
          launcher: "claude",
          slotIndex: 0,
          userNamed: "yes",
          title: "x".repeat(500),
          name: "   ",
        },
      },
    });
    const parsedMalformed = mod.parsePersistedSplitWorkspace(malformedJson);
    expect(parsedMalformed?.leaves["old-shell"].userNamed).toBeUndefined();
    expect(parsedMalformed?.leaves["old-shell"].name).toBeUndefined();
    expect(parsedMalformed?.leaves["old-shell"].title?.length).toBeLessThanOrEqual(256);

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

  it("summarizes zoomed hidden split sibling attention by priority (blocked > error > done > unread)", async () => {
    const {
      splitPane,
      summarizeHiddenSplitSiblings,
    } = await import("./splitTree");
    let tree = splitPane(null, "p1", "p2", "row");
    tree = splitPane(tree, "p2", "p3", "col");
    tree = splitPane(tree, "p3", "p4", "row");

    // Unzoomed -> empty summary
    expect(summarizeHiddenSplitSiblings(tree, null, new Set(["p2"]), {}).badgeLabel).toBeNull();

    // Unread only
    const unreadOnly = summarizeHiddenSplitSiblings(tree, "p1", new Set(["p3", "p4"]), {});
    expect(unreadOnly).toMatchObject({
      hiddenCount: 3,
      unreadCount: 2,
      urgentPaneId: "p3",
      badgeLabel: "2 unread",
      tone: "info",
    });

    // Done beats unread
    const doneBeatsUnread = summarizeHiddenSplitSiblings(
      tree,
      "p1",
      new Set(["p2"]),
      { p4: { state: "done" }, p2: null },
    );
    expect(doneBeatsUnread).toMatchObject({
      doneCount: 1,
      unreadCount: 1,
      urgentPaneId: "p4",
      badgeLabel: "Done",
      tone: "accent",
    });

    // Error beats done
    const errorBeatsDone = summarizeHiddenSplitSiblings(
      tree,
      "p1",
      new Set(),
      { p3: { state: "error" }, p4: { state: "done" } },
    );
    expect(errorBeatsDone).toMatchObject({
      errorCount: 1,
      urgentPaneId: "p3",
      badgeLabel: "Failed",
      tone: "danger",
    });

    // Blocked beats error, done, and unread
    const blockedWins = summarizeHiddenSplitSiblings(
      tree,
      "p1",
      new Set(["p2"]),
      {
        p2: { state: "error" },
        p3: { state: "blocked", detail: "Approve command?" },
        p4: { state: "blocked" },
      },
    );
    expect(blockedWins).toMatchObject({
      blockedCount: 2,
      errorCount: 1,
      unreadCount: 1,
      urgentPaneId: "p3",
      badgeLabel: "2 need input",
      tone: "warn",
    });
  });

  it("classifies center swap vs outer split drop zones, protects pinned panes, and round-trips snippet + pinned metadata", async () => {
    const {
      MAX_PERSISTED_SNIPPET_CHARS,
      MAX_SPLIT_PANES,
      chooseSplitReplacementSlot,
      computeTabDropHint,
      leafPaneIds,
      parsePersistedSplitWorkspace,
      serializeSplitWorkspace,
      splitPane,
      swapPanes,
    } = await import("./splitTree");

    const rect = { left: 100, top: 200, width: 400, height: 200 };
    expect(computeTabDropHint({ ...rect, width: 0 }, 200, 250, "p1")).toBeNull();
    expect(computeTabDropHint(rect, 300, 300, "p1")).toEqual({ paneId: "p1", mode: "swap" });
    expect(computeTabDropHint(rect, 120, 300, "p1")).toEqual({
      paneId: "p1",
      mode: "split",
      direction: "row",
      position: "before",
    });
    expect(computeTabDropHint(rect, 490, 300, "p1")).toEqual({
      paneId: "p1",
      mode: "split",
      direction: "row",
      position: "after",
    });
    expect(computeTabDropHint(rect, 300, 210, "p1")).toEqual({
      paneId: "p1",
      mode: "split",
      direction: "col",
      position: "before",
    });
    expect(computeTabDropHint(rect, 300, 395, "p1")).toEqual({
      paneId: "p1",
      mode: "split",
      direction: "col",
      position: "after",
    });

    const swapped = swapPanes(splitPane(null, "p1", "p2", "row"), "p1", "p2");
    expect(leafPaneIds(swapped)).toEqual(["p2", "p1"]);

    // Replacement slot selection with pinned leaves
    const twoPane = splitPane(null, "p1", "p2", "row");
    expect(chooseSplitReplacementSlot(twoPane, "p1", new Set())).toEqual({
      mode: "replace",
      targetPaneId: "p1",
    });
    expect(chooseSplitReplacementSlot(twoPane, "p1", new Set(["p1"]))).toEqual({
      mode: "replace",
      targetPaneId: "p2",
    });
    expect(chooseSplitReplacementSlot(twoPane, "p1", new Set(["p1", "p2"]))).toEqual({
      mode: "split",
      anchorPaneId: "p1",
    });

    let eightPane = twoPane;
    for (let i = 3; i <= MAX_SPLIT_PANES; i += 1) {
      eightPane = splitPane(eightPane, `p${i - 1}`, `p${i}`, "row");
    }
    expect(
      chooseSplitReplacementSlot(
        eightPane,
        "p1",
        new Set(Array.from({ length: MAX_SPLIT_PANES }, (_, idx) => `p${idx + 1}`)),
      ),
    ).toEqual({ mode: "blocked" });

    // Persist & parse snippet (bounded to MAX_PERSISTED_SNIPPET_CHARS) + pinned
    const raw = serializeSplitWorkspace(twoPane, {
      p1: { launcher: "claude", slotIndex: 0, pinned: true, title: "Agent 1" },
      p2: {
        launcher: "file",
        slotIndex: 1,
        snippet: { title: "cargo test", content: "x".repeat(MAX_PERSISTED_SNIPPET_CHARS + 500) },
      },
    });
    const parsed = parsePersistedSplitWorkspace(raw);
    expect(parsed?.leaves.p1.pinned).toBe(true);
    expect(parsed?.leaves.p2.snippet?.title).toBe("cargo test");
    expect(parsed?.leaves.p2.snippet?.content.length).toBe(MAX_PERSISTED_SNIPPET_CHARS);
  });

  it("cycles visible split panes non-destructively and parses cycle, slot-jump, and unsplit chords across layouts", async () => {
    const {
      cycleVisibleSplitPane,
      splitPane,
      terminalSplitChord,
    } = await import("./splitTree");
    const tree = splitPane(splitPane(null, "p1", "p2", "row"), "p2", "p3", "col");
    expect(cycleVisibleSplitPane(tree, "p1", 1)).toBe("p2");
    expect(cycleVisibleSplitPane(tree, "p3", 1)).toBe("p1");
    expect(cycleVisibleSplitPane(tree, "p1", -1)).toBe("p3");

    // macOS Option-modified characters often produce curly quotes or symbols in event.key, so event.code fallback is essential
    expect(
      terminalSplitChord(
        { key: "‘", code: "BracketRight", ctrlKey: false, metaKey: true, altKey: true, shiftKey: false },
        "macos",
      ),
    ).toBe("focus-next-pane");
    expect(
      terminalSplitChord(
        { key: "“", code: "BracketLeft", ctrlKey: false, metaKey: true, altKey: true, shiftKey: false },
        "macos",
      ),
    ).toBe("focus-prev-pane");
    expect(
      terminalSplitChord(
        { key: "¡", code: "Digit1", ctrlKey: false, metaKey: true, altKey: true, shiftKey: false },
        "macos",
      ),
    ).toBe("focus-pane-1");
    expect(
      terminalSplitChord(
        { key: "º", code: "Digit0", ctrlKey: false, metaKey: true, altKey: true, shiftKey: false },
        "macos",
      ),
    ).toBe("unsplit");
    expect(
      terminalSplitChord(
        { key: "]", code: "BracketRight", ctrlKey: true, metaKey: false, altKey: true, shiftKey: false },
        "windows",
      ),
    ).toBe("focus-next-pane");
    expect(
      terminalSplitChord(
        { key: "4", code: "Digit4", ctrlKey: true, metaKey: false, altKey: true, shiftKey: false },
        "linux",
      ),
    ).toBe("focus-pane-4");
  });

  it("manages multiple disjoint SplitGroup pairs, co-location, cycling, group renaming, v3 persistence, and pair chords", async () => {
    const mod = await import("./splitTree");
    const tree1 = mod.splitPane(null, "t1", "t2", "row", "after", 0.5)!;
    const tree2 = mod.splitPane(null, "t3", "t4", "col", "after", 0.4)!;
    let groups = mod.upsertSplitGroup([], tree1, { groupId: "pair-1", activePaneId: "t2" });
    groups = mod.upsertSplitGroup(groups, tree2, { groupId: "pair-2", name: "Build Pair", activePaneId: "t3" });
    expect(groups).toHaveLength(2);
    expect([...mod.allGroupedPaneIds(groups)]).toEqual(["t1", "t2", "t3", "t4"]);

    // Prevent cross-group duplicate leaves: moving t2 into pair-2 via split/replace dissolves pair-1 cleanly (< 2 leaves).
    const movedTree2 = mod.splitPane(groups[1].tree, "t3", "t2", "row", "after", 0.5)!;
    const afterMove = mod.upsertSplitGroup(groups, movedTree2, { groupId: "pair-2", activePaneId: "t2" });
    expect(afterMove).toHaveLength(1);
    expect(afterMove[0].id).toBe("pair-2");
    expect(mod.leafPaneIds(afterMove[0].tree)).toEqual(["t3", "t2", "t4"]);

    // Co-locates tabs inside their group's leaf order and segments the strip cleanly.
    const scattered = [{ id: "t4" }, { id: "solo" }, { id: "t2" }, { id: "t3" }, { id: "t1" }];
    const coLocated = mod.coLocateTabsForGroups(scattered, groups);
    expect(coLocated.map((t) => t.id)).toEqual(["t3", "t4", "solo", "t1", "t2"]);
    // Calling again on already co-located list preserves exact array reference.
    expect(mod.coLocateTabsForGroups(coLocated, groups)).toBe(coLocated);

    const segments = mod.buildTabStripSegments(coLocated, groups);
    expect(segments.map((s) => (s.kind === "group" ? `${s.group.id}:${s.tabs.map((t) => t.id).join("+")}` : s.tab.id))).toEqual([
      "pair-2:t3+t4",
      "solo",
      "pair-1:t1+t2",
    ]);

    // Moving a tab inside a group swaps within the group; moving across outer boundary swaps whole segment.
    const orderedPairs = mod.orderGroupsByTabOrder(groups, coLocated);
    const swapInner = mod.moveTabWithGroups(coLocated, orderedPairs, "t3", 1);
    expect(swapInner.tabs.map((t) => t.id)).toEqual(["t4", "t3", "solo", "t1", "t2"]);
    expect(mod.leafPaneIds(swapInner.groups[0].tree)).toEqual(["t4", "t3"]);

    const jumpSoloPastGroup = mod.moveTabWithGroups(coLocated, orderedPairs, "solo", -1);
    expect(jumpSoloPastGroup.tabs.map((t) => t.id)).toEqual(["solo", "t3", "t4", "t1", "t2"]);

    // Cycling pairs remembers each pair's activePaneId and wraps cleanly.
    expect(mod.cycleSplitGroup(orderedPairs, null, 1)).toMatchObject({ group: { id: "pair-2" }, targetPaneId: "t3" });
    expect(mod.cycleSplitGroup(orderedPairs, "pair-2", 1)).toMatchObject({ group: { id: "pair-1" }, targetPaneId: "t2" });
    expect(mod.cycleSplitGroup(orderedPairs, "pair-1", 1)).toMatchObject({ group: { id: "pair-2" }, targetPaneId: "t3" });
    expect(mod.cycleSplitGroup(orderedPairs, "pair-2", -1)).toMatchObject({ group: { id: "pair-1" }, targetPaneId: "t2" });

    // Group rename + display labels + attention summary when backgrounded.
    const renamed = mod.renameSplitGroup(orderedPairs, "pair-2", "Review\u0007 Pair");
    expect(mod.splitGroupDisplayLabel(renamed[0], 0, 2)).toBe("Review Pair");
    expect(mod.splitGroupDisplayLabel(renamed[1], 1, 2)).toBe("Pair 2");
    expect(mod.summarizeSplitGroupAttention(renamed[1], true, new Set(["t1"]), { t2: { state: "blocked" } }).badgeLabel).toBeNull();
    expect(mod.summarizeSplitGroupAttention(renamed[1], false, new Set(["t1"]), { t2: { state: "blocked" } })).toMatchObject({
      urgentPaneId: "t2",
      badgeLabel: "Needs input",
      tone: "warn",
    });

    // v3 multi-group serialization & parsing round-trip.
    const serializedV3 = mod.serializeSplitWorkspace(
      renamed,
      {
        t1: { launcher: "claude", slotIndex: 0, pinned: true },
        t2: { launcher: "shell", slotIndex: 1 },
        t3: { launcher: "shell", slotIndex: 2 },
        t4: { launcher: "file", slotIndex: 3, filePath: "README.md" },
      },
      "pair-1",
    );
    const parsedV3 = mod.parsePersistedSplitWorkspace(serializedV3);
    expect(parsedV3?.groups).toHaveLength(2);
    expect(parsedV3?.activeGroupId).toBe("pair-1");
    expect(parsedV3?.groups[0].name).toBe("Review Pair");
    expect(parsedV3?.groups[1].activePaneId).toBe("t2");
    const remappedGroups = mod.remapSplitGroups(
      parsedV3?.groups ?? [],
      new Map([
        ["t1", "n1"],
        ["t2", "n2"],
        ["t3", "n3"],
        ["t4", "n4"],
      ]),
    );
    expect(mod.leafPaneIds(remappedGroups[0].tree)).toEqual(["n4", "n3"].reverse());

    // Pair-switching chords across macOS and Windows/Linux without breaking Ctrl+Shift+Alt+D.
    expect(
      mod.terminalSplitChord(
        { key: "’", code: "BracketRight", ctrlKey: false, metaKey: true, altKey: true, shiftKey: true },
        "macos",
      ),
    ).toBe("focus-next-group");
    expect(
      mod.terminalSplitChord(
        { key: "“", code: "BracketLeft", ctrlKey: false, metaKey: true, altKey: true, shiftKey: true },
        "macos",
      ),
    ).toBe("focus-prev-group");
    expect(
      mod.terminalSplitChord(
        { key: "}", code: "BracketRight", ctrlKey: true, metaKey: false, altKey: true, shiftKey: true },
        "linux",
      ),
    ).toBe("focus-next-group");
    expect(
      mod.terminalSplitChord(
        { key: "D", ctrlKey: true, metaKey: false, altKey: true, shiftKey: true },
        "linux",
      ),
    ).toBe("split-down");
  });

  it("plans and executes predictable tab-strip drag-and-drop without guessing splits", async () => {
    const mod = await import("./splitTree");

    // 1. Default tab-strip drag reorders (before/after), never silently auto-splitting; Alt+Drag opts into pair.
    expect(mod.classifyTabStripDropZone(0.1, false)).toBe("before");
    expect(mod.classifyTabStripDropZone(0.49, false)).toBe("before");
    expect(mod.classifyTabStripDropZone(0.5, false)).toBe("after");
    expect(mod.classifyTabStripDropZone(0.9, false)).toBe("after");
    expect(mod.classifyTabStripDropZone(0.5, true)).toBe("pair");

    // 2. Reorders ungrouped tabs cleanly without creating any split group.
    const tabs0 = [{ id: "a" }, { id: "b" }, { id: "c" }];
    const reordered = mod.applyTabStripDrop(tabs0, [], {
      movingTabId: "c",
      targetTabId: "a",
      mode: "before",
    });
    expect(reordered.tabs.map((t: { id: string }) => t.id)).toEqual(["c", "a", "b"]);
    expect(reordered.groups).toEqual([]);

    // 3. Alt+Drop ("pair") explicitly pairs two tabs into a split group.
    const paired = mod.applyTabStripDrop(reordered.tabs, [], {
      movingTabId: "c",
      targetTabId: "a",
      mode: "pair",
    });
    expect(paired.groups).toHaveLength(1);
    expect(mod.leafPaneIds(paired.groups[0].tree)).toEqual(["a", "c"]);
    expect(paired.activeTabId).toBe("c");

    // 4. Dragging tabs within the same split group swaps/reorders their split slots cleanly.
    const swappedInside = mod.applyTabStripDrop(paired.tabs, paired.groups, {
      movingTabId: "c",
      targetTabId: "a",
      mode: "before",
    });
    expect(mod.leafPaneIds(swappedInside.groups[0].tree)).toEqual(["c", "a"]);

    // 5. Dragging an ungrouped tab ("b") before the outer left edge of a split group places it before the group WITHOUT adding a 3rd pane!
    const bBeforeGroup = mod.applyTabStripDrop(swappedInside.tabs, swappedInside.groups, {
      movingTabId: "b",
      targetTabId: "c",
      mode: "before",
    });
    expect(bBeforeGroup.groups).toHaveLength(1);
    expect(mod.leafPaneIds(bBeforeGroup.groups[0].tree)).toEqual(["c", "a"]);
    expect(bBeforeGroup.tabs.map((t: { id: string }) => t.id)).toEqual(["b", "c", "a"]);

    // 6. Dragging a grouped tab onto an ungrouped tab detaches it from the group and reorders it cleanly.
    const tree3 = mod.splitPane(bBeforeGroup.groups[0].tree, "a", "d", "row", "after", 0.5)!;
    const tabs4 = [{ id: "b" }, { id: "c" }, { id: "a" }, { id: "d" }];
    const groups3 = mod.upsertSplitGroup([], tree3, { groupId: "g1", activePaneId: "c" });
    const detachedToBeforeB = mod.applyTabStripDrop(tabs4, groups3, {
      movingTabId: "d",
      targetTabId: "b",
      mode: "before",
    });
    expect(mod.leafPaneIds(detachedToBeforeB.groups[0].tree)).toEqual(["c", "a"]);
    expect(detachedToBeforeB.tabs.map((t: { id: string }) => t.id)).toEqual(["d", "b", "c", "a"]);

    // 7. Detaching a grouped tab onto the open strip tail removes it from its split group and moves it to the end.
    const detachedTail = mod.detachTabToStripEnd(detachedToBeforeB.tabs, detachedToBeforeB.groups, "c");
    expect(detachedTail.groups).toHaveLength(0); // 2-leaf group dissolved cleanly when c detached
    expect(detachedTail.tabs.map((t: { id: string }) => t.id)).toEqual(["d", "b", "a", "c"]);

    // 8. Reordering entire SplitGroup pair capsules relative to other groups/tabs.
    const gA = mod.upsertSplitGroup([], mod.splitPane(null, "t1", "t2", "row")!, { groupId: "ga" })[0];
    const gB = mod.upsertSplitGroup([], mod.splitPane(null, "t3", "t4", "row")!, { groupId: "gb" })[0];
    const multiTabs = [{ id: "t1" }, { id: "t2" }, { id: "solo" }, { id: "t3" }, { id: "t4" }];
    const movedGroup = mod.reorderSplitGroupInStrip(multiTabs, [gA, gB], "gb", "ga", "before");
    expect(movedGroup.tabs.map((t: { id: string }) => t.id)).toEqual(["t3", "t4", "t1", "t2", "solo"]);
    expect(movedGroup.groups.map((g: { id: string }) => g.id)).toEqual(["gb", "ga"]);

    // 9. Moving a SplitGroup capsule to the end of the strip (e.g. dropping Pair badge onto empty strip tail).
    const tailMoved = mod.moveSplitGroupToStripEnd(movedGroup.tabs, movedGroup.groups, "gb");
    expect(tailMoved.tabs.map((t: { id: string }) => t.id)).toEqual(["t1", "t2", "solo", "t3", "t4"]);
    expect(tailMoved.groups.map((g: { id: string }) => g.id)).toEqual(["ga", "gb"]);

    // 10. Reordering within a 3-pane mixed row+col SplitGroup (Full-Stack layout) preserves col direction + custom ratios,
    //     and a drop that leaves leaf order unchanged is a pure no-op!
    const baseRow = mod.splitPane(null, "agent", "shell", "row", "after", 0.35)!;
    const fullStackTree = mod.splitPane(baseRow, "shell", "browser", "col", "after", 0.6)!;
    const fsGroup = mod.upsertSplitGroup([], fullStackTree, { groupId: "fs", activePaneId: "agent" });
    const fsTabs = [{ id: "agent" }, { id: "shell" }, { id: "browser" }];

    // Dropping "shell" "after" "agent" keeps ["agent", "shell", "browser"] — must NOT mutate tree or reset ratios
    const noOpDrop = mod.applyTabStripDrop(fsTabs, fsGroup, {
      movingTabId: "shell",
      targetTabId: "agent",
      mode: "after",
    });
    expect(noOpDrop.groups[0].tree).toBe(fsGroup[0].tree);

    // Dropping "browser" "before" "shell" permutes leaves to ["agent", "browser", "shell"] while keeping row(0.35) + col(0.6)
    const reorderedFs = mod.applyTabStripDrop(fsTabs, fsGroup, {
      movingTabId: "browser",
      targetTabId: "shell",
      mode: "before",
    });
    const nextRoot = reorderedFs.groups[0].tree;
    expect(mod.leafPaneIds(nextRoot)).toEqual(["agent", "browser", "shell"]);
    expect(nextRoot.direction).toBe("row");
    expect(nextRoot.ratio).toBeCloseTo(0.35, 5);
    expect(nextRoot.b.kind).toBe("branch");
    if (nextRoot.b.kind === "branch") {
      expect(nextRoot.b.direction).toBe("col");
      expect(nextRoot.b.ratio).toBeCloseTo(0.6, 5);
    }
  });

  it("flips split orientation, swaps adjacent panes, and moves pair groups by step without destroying ratios or trees", async () => {
    const mod = await import("./splitTree");

    // 1. flipSplitOrientation handles null/leaf, 2-pane row/col toggle with custom ratio, and targeted inner branch in 3+ pane tree.
    expect(mod.flipSplitOrientation(null)).toBeNull();
    const singleLeaf = mod.leaf("t1");
    expect(mod.flipSplitOrientation(singleLeaf)).toBe(singleLeaf);

    const row2 = mod.splitPane(null, "t1", "t2", "row", "after", 0.35)!;
    const flippedCol = mod.flipSplitOrientation(row2)!;
    expect(flippedCol.kind).toBe("branch");
    if (flippedCol.kind === "branch" && row2.kind === "branch") {
      expect(flippedCol.id).toBe(row2.id);
      expect(flippedCol.direction).toBe("col");
      expect(flippedCol.ratio).toBeCloseTo(0.35, 5);
      expect(mod.leafPaneIds(flippedCol)).toEqual(["t1", "t2"]);
    }
    const flippedBack = mod.flipSplitOrientation(flippedCol)!;
    if (flippedBack.kind === "branch") {
      expect(flippedBack.direction).toBe("row");
    }

    const nested3 = mod.splitPane(row2, "t2", "t3", "col", "after", 0.65)!;
    const flippedInner = mod.flipSplitOrientation(nested3, "t2")!;
    if (flippedInner.kind === "branch" && flippedInner.b.kind === "branch") {
      expect(flippedInner.direction).toBe("row");
      expect(flippedInner.ratio).toBeCloseTo(0.35, 5);
      expect(flippedInner.b.direction).toBe("row");
      expect(flippedInner.b.ratio).toBeCloseTo(0.65, 5);
    }
    const flippedRootOnly = mod.flipSplitOrientation(nested3)!;
    if (flippedRootOnly.kind === "branch" && flippedRootOnly.b.kind === "branch") {
      expect(flippedRootOnly.direction).toBe("col");
      expect(flippedRootOnly.b.direction).toBe("col");
    }

    // 2. swapWithAdjacentSplitPane swaps next cyclic pane and preserves branch structure/ratios.
    expect(mod.swapWithAdjacentSplitPane(null, "t1")).toBeNull();
    expect(mod.swapWithAdjacentSplitPane(singleLeaf, "t1")).toBe(singleLeaf);
    expect(mod.swapWithAdjacentSplitPane(row2, "missing")).toBe(row2);

    const swapped2 = mod.swapWithAdjacentSplitPane(row2, "t1")!;
    expect(mod.leafPaneIds(swapped2)).toEqual(["t2", "t1"]);
    if (swapped2.kind === "branch") {
      expect(swapped2.direction).toBe("row");
      expect(swapped2.ratio).toBeCloseTo(0.35, 5);
    }

    const swappedMid = mod.swapWithAdjacentSplitPane(nested3, "t2")!;
    expect(mod.leafPaneIds(swappedMid)).toEqual(["t1", "t3", "t2"]);
    const swappedWrap = mod.swapWithAdjacentSplitPane(nested3, "t3")!;
    expect(mod.leafPaneIds(swappedWrap)).toEqual(["t3", "t2", "t1"]);

    // 3. moveSplitGroupByStep shifts a group capsule left or right across segments without ungrouping.
    const g1 = mod.upsertSplitGroup([], mod.splitPane(null, "a1", "a2", "row", "after", 0.4)!, {
      groupId: "g1",
      activePaneId: "a1",
    })[0];
    const g2 = mod.upsertSplitGroup([], mod.splitPane(null, "b1", "b2", "col", "after", 0.6)!, {
      groupId: "g2",
      activePaneId: "b1",
    })[0];
    const tabs = [{ id: "a1" }, { id: "a2" }, { id: "solo" }, { id: "b1" }, { id: "b2" }];
    const stepRight = mod.moveSplitGroupByStep(tabs, [g1, g2], "g1", 1);
    expect(stepRight.tabs.map((t) => t.id)).toEqual(["solo", "a1", "a2", "b1", "b2"]);
    expect(stepRight.groups.map((g) => g.id)).toEqual(["g1", "g2"]);

    const stepPastG2 = mod.moveSplitGroupByStep(stepRight.tabs, stepRight.groups, "g1", 1);
    expect(stepPastG2.tabs.map((t) => t.id)).toEqual(["solo", "b1", "b2", "a1", "a2"]);
    expect(stepPastG2.groups.map((g) => g.id)).toEqual(["g2", "g1"]);
    expect(stepPastG2.groups.find((g) => g.id === "g1")?.tree.ratio).toBeCloseTo(0.4, 5);

    const stepEdgeNoop = mod.moveSplitGroupByStep(stepPastG2.tabs, stepPastG2.groups, "g1", 1);
    expect(stepEdgeNoop.tabs.map((t) => t.id)).toEqual(["solo", "b1", "b2", "a1", "a2"]);

    const stepLeft = mod.moveSplitGroupByStep(stepPastG2.tabs, stepPastG2.groups, "g1", -1);
    expect(stepLeft.tabs.map((t) => t.id)).toEqual(["solo", "a1", "a2", "b1", "b2"]);

    const stepMissing = mod.moveSplitGroupByStep(tabs, [g1, g2], "unknown-group", -1);
    expect(stepMissing.tabs.map((t) => t.id)).toEqual(["a1", "a2", "solo", "b1", "b2"]);
  });
});

describe("split partner choice", () => {
  const tabs = [
    { id: "s1", launcher: "shell" },
    { id: "f1", launcher: "file" },
    { id: "c1", launcher: "claude" },
    { id: "s2", launcher: "shell" },
    { id: "w1", launcher: "shell", checkout: "/repo/.worktrees/a" },
  ];

  it("never pulls a loose tab of another kind into the split", async () => {
    const mod = await import("./splitTree");
    // The reported bug: a loose File Explorer tab became the new pane of a
    // shell split. Only a shell may answer a shell split.
    expect(mod.chooseSplitPartner(tabs, [], "s1", "shell")).toBe("s2");
    expect(mod.chooseSplitPartner(tabs, [], "s2", "shell")).toBe("s1");
    expect(mod.chooseSplitPartner(tabs, [], "s1", "claude")).toBe("c1");
    expect(mod.chooseSplitPartner([tabs[0], tabs[1]], [], "s1", "shell")).toBeNull();
  });

  it("keeps a worktree tab and a root tab apart", async () => {
    const mod = await import("./splitTree");
    expect(mod.chooseSplitPartner(tabs, [], "w1", "shell")).toBeNull();
    expect(
      mod.chooseSplitPartner(
        [...tabs, { id: "w2", launcher: "shell", checkout: "/repo/.worktrees/a" }],
        [],
        "w1",
        "shell",
      ),
    ).toBe("w2");
  });

  it("does not grow an existing pair from the strip, and skips tabs another pair holds", async () => {
    const mod = await import("./splitTree");
    const pair = mod.upsertSplitGroup([], mod.splitPane(null, "s1", "c1", "row", "after", 0.5), {
      groupId: "pair-1",
    });
    // The active tab is already paired: Split means a new pane, not a guess.
    expect(mod.chooseSplitPartner(tabs, pair, "s1", "shell")).toBeNull();
    // s2 is loose; s1 is held by pair-1, so it is not a candidate either.
    expect(mod.chooseSplitPartner(tabs, pair, "s2", "shell")).toBeNull();
    expect(mod.chooseSplitPartner(tabs, pair, "f1", "shell")).toBe("s2");
  });

  it("never re-pairs a session that has ended", async () => {
    const mod = await import("./splitTree");
    expect(mod.chooseSplitPartner(tabs, [], "s1", "shell", ["s2"])).toBeNull();
    expect(mod.chooseSplitPartner(tabs, [], "s1", "claude", ["s2"])).toBe("c1");
  });
});

describe("pair join targets", () => {
  it("lists every other pair with room, by strip label, anchored on its remembered pane", async () => {
    const mod = await import("./splitTree");
    let groups = mod.upsertSplitGroup([], mod.splitPane(null, "a1", "a2", "row", "after", 0.5), {
      groupId: "pair-1",
      activePaneId: "a2",
    });
    groups = mod.upsertSplitGroup(groups, mod.splitPane(null, "b1", "b2", "row", "after", 0.5), {
      groupId: "pair-2",
      name: "Build",
    });
    expect(mod.pairJoinTargets(groups, "solo")).toEqual([
      { groupId: "pair-1", label: "Pair 1", anchorPaneId: "a2" },
      { groupId: "pair-2", label: "Build", anchorPaneId: "b1" },
    ]);
    // A tab is never offered its own pair.
    expect(mod.pairJoinTargets(groups, "a1").map((t) => t.groupId)).toEqual(["pair-2"]);
    expect(mod.pairJoinTargets([], "solo")).toEqual([]);
  });

  it("leaves out a pair at the pane limit", async () => {
    const mod = await import("./splitTree");
    let tree: ReturnType<typeof mod.splitPane> = mod.splitPane(null, "p0", "p1", "row", "after", 0.5);
    for (let i = 2; i < mod.MAX_SPLIT_PANES; i += 1) {
      tree = mod.splitPane(tree, `p${i - 1}`, `p${i}`, "row", "after", 0.5);
    }
    const full = mod.upsertSplitGroup([], tree, { groupId: "pair-1" });
    expect(mod.pairJoinTargets(full, "solo")).toEqual([]);
  });
});
