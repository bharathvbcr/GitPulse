/**
 * Adversarial Challenger Stress Harness for Feature 10: TaskBoard & FleetView Virtualization.
 *
 * Empirically challenges:
 * 1. Scale Stress: 10,000 tasks and 5,000 repositories with bounded DOM rendered count (<= 40).
 * 2. Mathematical Invariants: topPadding + renderedCount * rowHeight + bottomPadding === totalCount * rowHeight across 1,000 randomized scroll positions.
 * 3. Rapid Bidirectional Jumping: chaotic scroll jumps (0, max, middle, negative, out-of-bounds, float, NaN) with integer-aligned non-negative offsets and no NaN.
 * 4. Dynamic Density Switching: seamless transitions between compact (32px/36px/58px) and spacious (40px/44px/72px) preserving total height and boundary integrity.
 * 5. Boundary Extremes & Thresholds: 0 items, 1 item, exact threshold boundaries (35 items for TaskBoard, 30 for FleetView), and negative viewport dimensions.
 * 6. Component AST & Contract Verification: TaskBoard and FleetView AST template analysis for VirtualList integration, row heights, and layout safety.
 */
import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { compile } from "svelte/compiler";
import { computeVirtualWindow, type VirtualWindow } from "./test-utils";
import {
  clampScrollTop,
  computeWindow,
  ensureNonEmptyWindow,
} from "@/lib/dom/virtualWindow";

const SRC_COMPONENTS = join(__dirname, "..", "..", "..", "src", "lib", "components");

/**
 * Model helper executing the exact VirtualList.svelte runtime pipeline.
 */
function virtualListRuntimePipeline(
  total: number,
  rowHeight: number,
  viewportHeight: number,
  scrollTop: number,
  overscan = 8
): VirtualWindow {
  const effectiveScroll = clampScrollTop(scrollTop, total, rowHeight, viewportHeight);
  const rawWin = computeWindow(effectiveScroll, viewportHeight, total, rowHeight, overscan);
  const win = ensureNonEmptyWindow(rawWin, total, rowHeight, viewportHeight);

  const startIndex = win.start;
  const endIndex = win.end;
  const renderedCount = Math.max(0, endIndex - startIndex);
  const topPadding = startIndex * rowHeight;
  const bottomPadding = Math.max(0, (total - endIndex) * rowHeight);
  const totalHeight = total * rowHeight;

  return {
    startIndex,
    endIndex,
    renderedCount,
    totalHeight,
    topPadding,
    bottomPadding,
  };
}

describe("Feature 10 Adversarial Stress: TaskBoard & FleetView Virtualization", () => {
  // ---------------------------------------------------------------------------
  // 1. Scale Stress: 10,000 Tasks & 5,000 Repositories
  // ---------------------------------------------------------------------------
  describe("1. Scale Stress: 10,000 Tasks & 5,000 Repositories", () => {
    it("1.1: 10,000 tasks in TaskBoard list mode maintains renderedCount <= 40 across standard viewports and scroll positions", () => {
      const totalTasks = 10_000;
      const viewportHeights = [400, 600, 700, 800];
      const buffer = 5; // Standard buffer in computeVirtualWindow (matches test 10.2)

      // Test both compact (32px) and spacious (40px)
      for (const rowHeight of [32, 40]) {
        for (const viewportHeight of viewportHeights) {
          const maxScroll = totalTasks * rowHeight - viewportHeight;
          const probePositions = [
            0,
            Math.floor(maxScroll * 0.1),
            Math.floor(maxScroll * 0.25),
            Math.floor(maxScroll * 0.5),
            Math.floor(maxScroll * 0.75),
            Math.floor(maxScroll * 0.9),
            maxScroll,
            maxScroll + 5000, // overscroll past bottom
          ];

          for (const scroll of probePositions) {
            const win = computeVirtualWindow(totalTasks, rowHeight, viewportHeight, scroll, buffer);
            expect(win.renderedCount).toBeLessThanOrEqual(40);
            expect(win.startIndex).toBeGreaterThanOrEqual(0);
            expect(win.endIndex).toBeLessThanOrEqual(totalTasks);
            expect(win.startIndex).toBeLessThanOrEqual(win.endIndex);

            // Also verify against the pure VirtualList runtime pipeline
            const pipeWin = virtualListRuntimePipeline(totalTasks, rowHeight, viewportHeight, scroll, buffer);
            expect(pipeWin.renderedCount).toBeLessThanOrEqual(40);
            expect(pipeWin.startIndex).toBeGreaterThanOrEqual(0);
            expect(pipeWin.endIndex).toBeLessThanOrEqual(totalTasks);
          }
        }
      }
    });

    it("1.2: 10,000 tasks in TaskBoard board (kanban) mode maintains renderedCount <= 40 per column", () => {
      const totalCards = 10_000;
      const viewportHeights = [500, 720, 800];
      const buffer = 8;

      // Board card heights: compact (58px) and spacious (72px)
      for (const cardHeight of [58, 72]) {
        for (const viewportHeight of viewportHeights) {
          const maxScroll = totalCards * cardHeight - viewportHeight;
          for (let step = 0; step <= 10; step++) {
            const scroll = Math.floor(maxScroll * (step / 10));
            const win = computeVirtualWindow(totalCards, cardHeight, viewportHeight, scroll, buffer);
            expect(win.renderedCount).toBeLessThanOrEqual(40);
            expect(win.startIndex).toBeGreaterThanOrEqual(0);
            expect(win.endIndex).toBeLessThanOrEqual(totalCards);
          }
        }
      }
    });

    it("1.3: 5,000 repositories in FleetView table maintains renderedCount <= 40 across standard viewports", () => {
      const totalRepos = 5_000;
      const viewportHeights = [400, 600, 700];
      const buffer = 6; // Standard buffer in FleetView (as in test 10.3)

      // Fleet row heights: compact (36px) and spacious (44px)
      for (const rowHeight of [36, 44]) {
        for (const viewportHeight of viewportHeights) {
          const maxScroll = totalRepos * rowHeight - viewportHeight;
          for (let step = 0; step <= 20; step++) {
            const scroll = Math.floor(maxScroll * (step / 20));
            const win = computeVirtualWindow(totalRepos, rowHeight, viewportHeight, scroll, buffer);
            expect(win.renderedCount).toBeLessThanOrEqual(40);
            expect(win.startIndex).toBeGreaterThanOrEqual(0);
            expect(win.endIndex).toBeLessThanOrEqual(totalRepos);
          }
        }
      }
    });
  });

  // ---------------------------------------------------------------------------
  // 2. Mathematical Invariants: 1,000 Randomized Scroll Positions
  // ---------------------------------------------------------------------------
  describe("2. Mathematical Invariants across 1,000 Randomized Scroll Positions", () => {
    it("2.1: Holds topPadding + renderedCount * rowHeight + bottomPadding === totalCount * rowHeight across 1,000 randomized scroll positions", () => {
      const testCases = [
        { total: 10_000, rowHeight: 40, viewHeight: 800, buffer: 10 },
        { total: 5_000, rowHeight: 44, viewHeight: 700, buffer: 6 },
        { total: 1_200, rowHeight: 32, viewHeight: 600, buffer: 8 },
        { total: 800, rowHeight: 58, viewHeight: 650, buffer: 5 },
        { total: 350, rowHeight: 72, viewHeight: 900, buffer: 8 },
      ];

      let totalIterations = 0;
      // Simple deterministic seedable PRNG (mulberry32)
      let seed = 0x2f4a6b8c;
      const nextRand = () => {
        let t = (seed += 0x6d2b79f5);
        t = Math.imul(t ^ (t >>> 15), t | 1);
        t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
        return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
      };

      for (const tc of testCases) {
        const totalHeight = tc.total * tc.rowHeight;
        for (let i = 0; i < 200; i++) {
          totalIterations++;
          // Random scroll range covering [0, maxScroll * 1.5] (including overscroll)
          const scroll = Math.floor(nextRand() * (totalHeight * 1.2));
          const win = computeVirtualWindow(tc.total, tc.rowHeight, tc.viewHeight, scroll, tc.buffer);

          // Assert exact mathematical invariant
          const reconstructedHeight = win.topPadding + win.renderedCount * tc.rowHeight + win.bottomPadding;
          expect(reconstructedHeight).toBe(tc.total * tc.rowHeight);
          expect(win.totalHeight).toBe(tc.total * tc.rowHeight);

          // Assert padding alignment
          expect(win.topPadding).toBe(win.startIndex * tc.rowHeight);
          expect(win.bottomPadding).toBe((tc.total - win.endIndex) * tc.rowHeight);
          expect(win.renderedCount).toBe(win.endIndex - win.startIndex);

          // Verify runtime pipeline satisfies exact invariant as well
          const pipeWin = virtualListRuntimePipeline(tc.total, tc.rowHeight, tc.viewHeight, scroll, tc.buffer);
          const pipeReconstructed = pipeWin.topPadding + pipeWin.renderedCount * tc.rowHeight + pipeWin.bottomPadding;
          expect(pipeReconstructed).toBe(tc.total * tc.rowHeight);
        }
      }

      expect(totalIterations).toBe(1000);
    });
  });

  // ---------------------------------------------------------------------------
  // 3. Rapid Bidirectional Jumping & Chaotic Scenarios
  // ---------------------------------------------------------------------------
  describe("3. Rapid Bidirectional Jumping & Chaotic Scenarios", () => {
    it("3.1: Chaotic scroll jumps between 0, max, middle, negative, and extreme values preserve non-negative integer alignment without NaN", () => {
      const total = 5000;
      const rowHeight = 40;
      const viewHeight = 800;
      const maxScroll = total * rowHeight - viewHeight;

      const chaoticScrolls = [
        0,
        maxScroll,
        Math.floor(maxScroll / 2),
        -500, // elastic overscroll top
        maxScroll + 15_000, // elastic overscroll bottom
        0.0001, // subpixel drift
        123.4567, // fractional scroll
        maxScroll - 0.75,
        -0.000001,
        1e9, // extreme out-of-bounds
        Number.NaN,
        Number.POSITIVE_INFINITY,
        Number.NEGATIVE_INFINITY,
        maxScroll / 3,
        (maxScroll * 2) / 3,
        1,
        maxScroll - 1,
      ];

      for (const scroll of chaoticScrolls) {
        const win = computeVirtualWindow(total, rowHeight, viewHeight, scroll, 8);

        // Verify no NaN values anywhere
        expect(Number.isNaN(win.startIndex)).toBe(false);
        expect(Number.isNaN(win.endIndex)).toBe(false);
        expect(Number.isNaN(win.renderedCount)).toBe(false);
        expect(Number.isNaN(win.topPadding)).toBe(false);
        expect(Number.isNaN(win.bottomPadding)).toBe(false);
        expect(Number.isNaN(win.totalHeight)).toBe(false);

        // Verify integer-aligned and non-negative
        expect(Number.isInteger(win.startIndex)).toBe(true);
        expect(Number.isInteger(win.endIndex)).toBe(true);
        expect(Number.isInteger(win.renderedCount)).toBe(true);
        expect(win.startIndex).toBeGreaterThanOrEqual(0);
        expect(win.endIndex).toBeGreaterThanOrEqual(win.startIndex);
        expect(win.endIndex).toBeLessThanOrEqual(total);
        expect(win.topPadding).toBeGreaterThanOrEqual(0);
        expect(win.bottomPadding).toBeGreaterThanOrEqual(0);

        // Mathematical sum holds
        expect(win.topPadding + win.renderedCount * rowHeight + win.bottomPadding).toBe(total * rowHeight);

        // Pipeline check
        const pipeWin = virtualListRuntimePipeline(total, rowHeight, viewHeight, scroll, 8);
        expect(pipeWin.startIndex).toBeGreaterThanOrEqual(0);
        expect(pipeWin.endIndex).toBeLessThanOrEqual(total);
        expect(Number.isNaN(pipeWin.startIndex)).toBe(false);
      }
    });

    it("3.2: 500 rapid interleaved scroll jumps simulate jittery user flick gestures without state hysteresis", () => {
      const total = 2000;
      const rowHeight = 36;
      const viewHeight = 600;
      const maxScroll = total * rowHeight - viewHeight;

      for (let i = 0; i < 500; i++) {
        // Ping-pong jumping between top and bottom quarters
        const scroll = i % 2 === 0 ? (i * 37) % 500 : maxScroll - ((i * 41) % 500);
        const win = computeVirtualWindow(total, rowHeight, viewHeight, scroll, 6);

        expect(win.renderedCount).toBeLessThanOrEqual(40);
        expect(win.startIndex).toBeGreaterThanOrEqual(0);
        expect(win.endIndex).toBeLessThanOrEqual(total);

        // Returning to same scroll position gives strictly idempotent output
        const repeat = computeVirtualWindow(total, rowHeight, viewHeight, scroll, 6);
        expect(repeat).toEqual(win);
      }
    });
  });

  // ---------------------------------------------------------------------------
  // 4. Dynamic Density Switching
  // ---------------------------------------------------------------------------
  describe("4. Dynamic Density Switching", () => {
    it("4.1: TaskBoard List Mode toggles between compact (32px) and spacious (40px) preserving boundary integrity", () => {
      const totalCards = 1500;
      const viewHeight = 700;
      const midScroll = 20_000;

      const compactWin = computeVirtualWindow(totalCards, 32, viewHeight, midScroll, 8);
      const spaciousWin = computeVirtualWindow(totalCards, 40, viewHeight, midScroll, 8);

      // Compact has lower total height than spacious
      expect(compactWin.totalHeight).toBe(1500 * 32);
      expect(spaciousWin.totalHeight).toBe(1500 * 40);
      expect(compactWin.totalHeight).toBeLessThan(spaciousWin.totalHeight);

      // Both strictly bound rendered elements
      expect(compactWin.renderedCount).toBeLessThanOrEqual(40);
      expect(spaciousWin.renderedCount).toBeLessThanOrEqual(40);

      // Invariant holds for both densities
      expect(compactWin.topPadding + compactWin.renderedCount * 32 + compactWin.bottomPadding).toBe(1500 * 32);
      expect(spaciousWin.topPadding + spaciousWin.renderedCount * 40 + spaciousWin.bottomPadding).toBe(1500 * 40);

      // Smaller row height allows more visible rows in same viewport
      expect(compactWin.renderedCount).toBeGreaterThanOrEqual(spaciousWin.renderedCount);
    });

    it("4.2: TaskBoard Board Mode toggles between compact (58px) and spacious (72px) correctly", () => {
      const totalCards = 500;
      const viewHeight = 800;
      const scroll = 10_000;

      const compactWin = computeVirtualWindow(totalCards, 58, viewHeight, scroll, 8);
      const spaciousWin = computeVirtualWindow(totalCards, 72, viewHeight, scroll, 8);

      expect(compactWin.totalHeight).toBe(500 * 58);
      expect(spaciousWin.totalHeight).toBe(500 * 72);
      expect(compactWin.renderedCount).toBeLessThanOrEqual(40);
      expect(spaciousWin.renderedCount).toBeLessThanOrEqual(40);
      expect(compactWin.topPadding + compactWin.renderedCount * 58 + compactWin.bottomPadding).toBe(500 * 58);
      expect(spaciousWin.topPadding + spaciousWin.renderedCount * 72 + spaciousWin.bottomPadding).toBe(500 * 72);
    });

    it("4.3: FleetView toggles between compact (36px) and spacious (44px) preserving exact spacer height", () => {
      const totalRepos = 3000;
      const viewHeight = 700;
      const scroll = 45_000;

      const compactWin = computeVirtualWindow(totalRepos, 36, viewHeight, scroll, 6);
      const spaciousWin = computeVirtualWindow(totalRepos, 44, viewHeight, scroll, 6);

      expect(compactWin.totalHeight).toBe(3000 * 36);
      expect(spaciousWin.totalHeight).toBe(3000 * 44);
      expect(compactWin.renderedCount).toBeLessThanOrEqual(40);
      expect(spaciousWin.renderedCount).toBeLessThanOrEqual(40);
      expect(compactWin.topPadding + compactWin.renderedCount * 36 + compactWin.bottomPadding).toBe(3000 * 36);
      expect(spaciousWin.topPadding + spaciousWin.renderedCount * 44 + spaciousWin.bottomPadding).toBe(3000 * 44);
    });
  });

  // ---------------------------------------------------------------------------
  // 5. Boundary Extremes & Thresholds
  // ---------------------------------------------------------------------------
  describe("5. Boundary Extremes & Thresholds", () => {
    it("5.1: Zero items degenerates to clean zeroed window without NaN or division by zero", () => {
      const win = computeVirtualWindow(0, 40, 800, 100);
      expect(win.startIndex).toBe(0);
      expect(win.endIndex).toBe(0);
      expect(win.renderedCount).toBe(0);
      expect(win.totalHeight).toBe(0);
      expect(win.topPadding).toBe(0);
      expect(win.bottomPadding).toBe(0);

      const pipeWin = virtualListRuntimePipeline(0, 40, 800, 100);
      expect(pipeWin.startIndex).toBe(0);
      expect(pipeWin.endIndex).toBe(0);
      expect(pipeWin.renderedCount).toBe(0);
    });

    it("5.2: Exactly 1 item renders startIndex=0, endIndex=1 without over-padding", () => {
      const win = computeVirtualWindow(1, 40, 800, 0);
      expect(win.startIndex).toBe(0);
      expect(win.endIndex).toBe(1);
      expect(win.renderedCount).toBe(1);
      expect(win.totalHeight).toBe(40);
      expect(win.topPadding).toBe(0);
      expect(win.bottomPadding).toBe(0);

      const pipeWin = virtualListRuntimePipeline(1, 40, 800, 0);
      expect(pipeWin.startIndex).toBe(0);
      expect(pipeWin.endIndex).toBe(1);
      expect(pipeWin.renderedCount).toBe(1);
    });

    it("5.3: TaskBoard threshold boundary at exactly 35 items vs 36 items", () => {
      // Threshold in TaskBoard: listCards.length > 35 and drawn.length > 35
      const boundaryAt35 = computeVirtualWindow(35, 40, 800, 0, 8);
      const boundaryAt36 = computeVirtualWindow(36, 40, 800, 0, 8);

      expect(boundaryAt35.totalHeight).toBe(35 * 40);
      expect(boundaryAt36.totalHeight).toBe(36 * 40);

      expect(boundaryAt35.startIndex).toBe(0);
      expect(boundaryAt35.endIndex).toBe(28); // 20 visible + 8 buffer
      expect(boundaryAt35.bottomPadding).toBe((35 - 28) * 40);

      expect(boundaryAt36.startIndex).toBe(0);
      expect(boundaryAt36.endIndex).toBe(28);
      expect(boundaryAt36.bottomPadding).toBe((36 - 28) * 40);
    });

    it("5.4: FleetView threshold boundary at exactly 30 items vs 31 items", () => {
      // Threshold in FleetView: visibleRows.length > 30
      const boundaryAt30 = computeVirtualWindow(30, 44, 700, 0, 6);
      const boundaryAt31 = computeVirtualWindow(31, 44, 700, 0, 6);

      expect(boundaryAt30.totalHeight).toBe(30 * 44);
      expect(boundaryAt31.totalHeight).toBe(31 * 44);

      expect(boundaryAt30.renderedCount).toBe(22); // 16 visible + 6 buffer
      expect(boundaryAt31.renderedCount).toBe(22);
      expect(boundaryAt30.bottomPadding).toBe((30 - 22) * 44);
      expect(boundaryAt31.bottomPadding).toBe((31 - 22) * 44);
    });

    it("5.5: Negative or zero viewport dimensions fail closed cleanly without crashing or NaN", () => {
      const negativeViewports = [-800, -1, 0, -0.001, Number.NaN];

      for (const vp of negativeViewports) {
        const win = computeVirtualWindow(100, 40, vp, 100);
        expect(win.startIndex).toBeGreaterThanOrEqual(0);
        expect(win.endIndex).toBeGreaterThanOrEqual(win.startIndex);
        expect(win.endIndex).toBeLessThanOrEqual(100);
        expect(win.renderedCount).toBeGreaterThanOrEqual(0);
        expect(Number.isNaN(win.renderedCount)).toBe(false);
        expect(Number.isNaN(win.topPadding)).toBe(false);
        expect(Number.isNaN(win.bottomPadding)).toBe(false);
        expect(win.topPadding + win.renderedCount * 40 + win.bottomPadding).toBe(100 * 40);

        const pipeWin = virtualListRuntimePipeline(100, 40, vp, 100);
        expect(pipeWin.startIndex).toBeGreaterThanOrEqual(0);
        expect(pipeWin.endIndex).toBeLessThanOrEqual(100);
        expect(Number.isNaN(pipeWin.renderedCount)).toBe(false);
      }
    });
  });

  // ---------------------------------------------------------------------------
  // 6. Component AST & Contract Verification
  // ---------------------------------------------------------------------------
  describe("6. Component AST & Contract Verification", () => {
    it("6.1: TaskBoard.svelte imports VirtualList and wires listRowHeight, boardCardHeight, and threshold > 35", () => {
      const source = readFileSync(join(SRC_COMPONENTS, "TaskBoard.svelte"), "utf8");

      // Verify VirtualList import
      expect(source).toMatch(/import\s+VirtualList\s+from\s+["']\.\/VirtualList\.svelte["']/);

      // Verify row and card height calculations for compact and spacious
      expect(source).toContain("listRowHeight");
      expect(source).toContain("boardCardHeight");
      expect(source).toContain("LIST_ROW_HEIGHT_COMPACT = 32");
      expect(source).toContain("LIST_ROW_HEIGHT_SPACIOUS = 40");
      expect(source).toContain("BOARD_CARD_HEIGHT_COMPACT = 58");
      expect(source).toContain("BOARD_CARD_HEIGHT_SPACIOUS = 72");
      expect(source).toContain("listRowHeight = $derived(compact ? LIST_ROW_HEIGHT_COMPACT : LIST_ROW_HEIGHT_SPACIOUS)");
      expect(source).toContain("boardCardHeight = $derived(compact ? BOARD_CARD_HEIGHT_COMPACT : BOARD_CARD_HEIGHT_SPACIOUS)");

      // Verify threshold > 35 in list mode and board mode
      expect(source).toContain("listCards.length > 35");
      expect(source).toContain("drawn.length > 35");

      // Verify AST compiles without errors
      const compiled = compile(source, { filename: "TaskBoard.svelte" });
      expect(compiled.js).toBeDefined();
    });

    it("6.2: FleetView.svelte imports VirtualList and wires fleetRowHeight with threshold > 30 and fixed table layout", () => {
      const source = readFileSync(join(SRC_COMPONENTS, "FleetView.svelte"), "utf8");

      // Verify VirtualList import
      expect(source).toMatch(/import\s+VirtualList\s+from\s+["']\.\/VirtualList\.svelte["']/);

      // Verify fleet row height derivation (36 compact / 44 spacious)
      expect(source).toContain("fleetRowHeight");
      expect(source).toContain("FLEET_ROW_COMPACT = 36");
      expect(source).toContain("FLEET_ROW_SPACIOUS = 44");
      expect(source).toContain("fleetRowHeight = $derived($interfaceStore.fleetCompact ? FLEET_ROW_COMPACT : FLEET_ROW_SPACIOUS)");

      // Verify threshold > 30 in flat list mode
      expect(source).toContain("visibleRows.length > 30");

      // Verify HTML table layout preservation
      expect(source).toContain('table class="w-full text-[11px] border-collapse table-fixed"');

      // Verify AST compiles cleanly
      const compiled = compile(source, { filename: "FleetView.svelte" });
      expect(compiled.js).toBeDefined();
    });
  });
});
