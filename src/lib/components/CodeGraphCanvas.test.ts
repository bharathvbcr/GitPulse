import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { compile } from "svelte/compiler";

const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, "CodeGraphCanvas.svelte"), "utf8");

describe("CodeGraphCanvas", () => {
  it("compiles", () => {
    const result = compile(source, { filename: "CodeGraphCanvas.svelte", generate: "server" });
    expect(result.js.code.length).toBeGreaterThan(0);
  });

  it("renders truncation honesty from counts.nodes_truncated", () => {
    expect(source).toContain("truncationLegend");
    expect(source).toContain("code-graph-truncation");
    expect(source).toContain("code-graph-nodes-label");
    expect(source).toContain("legend.honesty");
  });

  it("uses code-graph hit testing rather than commit GraphRenderer", () => {
    expect(source).toContain("hitTestCodeGraph");
    expect(source).toContain("paintCodeGraph");
    expect(source).toContain("acquireGpu2dContext");
    expect(source).toContain("createFrameScheduler");
    expect(source).toContain("observeResize");
    expect(source).not.toMatch(/from ["'].*\/GraphRenderer["']/);
    expect(source).not.toContain("VisualCommitRow");
  });

  it("lets the shared material system paint the pane instead of covering it with an opaque hex plate", () => {
    const rule = source.match(/\.code-map\s*\{([^}]*)\}/)?.[1];
    expect(rule, "missing .code-map rule").toBeDefined();
    expect(rule, ".code-map covers the shared material").not.toMatch(
      /(?:^|;)\s*background(?:-color)?:/,
    );
    expect(source).not.toMatch(/\.code-map\.light\s*\{[^}]*background(?:-color)?:/);
    expect(source).toContain('class="code-map bg-background');
  });

  it("offers accessible search and keyboard navigation without compiler warnings", () => {
    const { warnings } = compile(source, { filename: "CodeGraphCanvas.svelte", generate: "client" });
    expect(warnings.filter(w => w.code.startsWith("a11y"))).toEqual([]);
    expect(source).toContain('aria-label="Find a file or symbol"');
    expect(source).toContain("onkeydown={onKeyDown}");
    expect(source).toContain("graphNodeOpenPath(selectedNode)");
    expect(source).toContain("onOpenNode(path, selectedNode.id)");
  });
});

describe("CodeGraphCanvas interaction cost and recovery", () => {
  it("lays out in a fixed content space, so a resize never re-lays out", () => {
    expect(source).toContain("buildCodeGraphModel(payload, CODE_GRAPH_LAYOUT_WIDTH, CODE_GRAPH_LAYOUT_HEIGHT)");
    expect(source).not.toMatch(/buildCodeGraphModel\(payload,\s*width,\s*height\)/);
  });

  it("hit-tests hover at most once per frame, from the newest pointer position", () => {
    expect(source).toContain("const hoverScheduler = createFrameScheduler()");
    // Every hitTestCodeGraph on the move path sits inside the scheduled job.
    const move = source.slice(source.indexOf("function onPointerMove"), source.indexOf("function onPointerUp"));
    expect(move).toContain("hoverScheduler.schedule(");
    expect(move.indexOf("hitTestCodeGraph")).toBeGreaterThan(move.indexOf("hoverScheduler.schedule("));
    expect(move).not.toContain("getBoundingClientRect()\n    return");
    expect(source).toContain("hoverScheduler.cancel()");
  });

  it("drops the cached context on loss and repaints on restore", () => {
    expect(source).toContain('canvas?.addEventListener("contextlost", onContextLost)');
    expect(source).toContain('canvas?.addEventListener("contextrestored", onContextRestored)');
    expect(source).toContain('canvas?.removeEventListener("contextlost", onContextLost)');
    expect(source).toMatch(/function onContextLost\(event: Event\) \{\s*event\.preventDefault\(\);\s*gpuCtx = null;\s*schedulePaint\(\);/);
    expect(source).toMatch(/function onContextRestored\(\) \{\s*gpuCtx = null;\s*schedulePaint\(\);/);
  });
});

/** The text of `function <name>(...) { ... }` in the component, braces balanced. */
function functionBody(name: string): string {
  const start = source.indexOf(`function ${name}(`);
  if (start < 0) throw new Error(`no function ${name}`);
  const open = source.indexOf("{", source.indexOf(")", start));
  let depth = 0;
  for (let i = open; i < source.length; i += 1) {
    if (source[i] === "{") depth += 1;
    else if (source[i] === "}" && (depth -= 1) === 0) return source.slice(start, i + 1);
  }
  throw new Error(`unbalanced function ${name}`);
}

describe("CodeGraphCanvas fit follows the stage", () => {
  // The toolbar grows when a payload lands, so the stage shrinks after the
  // first fit (702 px to 646 px in the app) and the fitted graph sat 28 px
  // off-centre. The fit follows the stage until the reader moves the view.
  const clears = "fitFollowsStage = false;";

  it("is set by resetView, the one place that fits", () => {
    expect(functionBody("resetView")).toContain("fitFollowsStage = true;");
    expect(source.match(/fitFollowsStage = true;/g)).toHaveLength(1);
  });

  it("re-fits on a resize while it is set, and only repaints once the reader has moved the view", () => {
    expect(functionBody("measure")).toMatch(/if \(fitFollowsStage\) resetView\(\);\s*else schedulePaint\(\);/);
  });

  it("is cleared by zoom (buttons, keys and wheel all go through zoomAt)", () => {
    expect(functionBody("zoomAt")).toContain(clears);
    expect(functionBody("onWheel")).toContain("zoomAt(");
  });

  it("is cleared by a drag, on the move that pans", () => {
    const move = functionBody("onPointerMove");
    const drag = move.slice(move.indexOf("if (dragging) {"));
    expect(drag.indexOf(clears)).toBeGreaterThan(-1);
    expect(drag.indexOf(clears)).toBeLessThan(drag.indexOf("panX +="));
  });

  it("is cleared by an arrow-key pan, but not by the fit and zoom keys", () => {
    const keys = functionBody("onKeyDown");
    const arrows = keys.slice(keys.indexOf('["ArrowLeft"'));
    expect(arrows.indexOf(clears)).toBeGreaterThan(-1);
    expect(arrows.indexOf(clears)).toBeLessThan(arrows.indexOf("panX +="));
    expect(keys.slice(0, keys.indexOf('["ArrowLeft"'))).not.toContain(clears);
    expect(keys).toContain('e.key === "0") resetView()');
  });

  it("is cleared by focusNode before it recentres", () => {
    const focus = functionBody("focusNode");
    expect(focus.indexOf(clears)).toBeGreaterThan(-1);
    expect(focus.indexOf(clears)).toBeLessThan(focus.indexOf("panX ="));
  });

  it("leaves no other function that moves the view without clearing it", () => {
    const moving = /\b(?:panX|panY|scale)\s*(?:\+=|-=|=(?!=))/;
    const names = [...source.matchAll(/^\s*function (\w+)\(/gm)].map((m) => m[1]);
    const offenders = names.filter((name) => {
      const body = functionBody(name);
      return moving.test(body) && !body.includes(clears) && name !== "resetView";
    });
    expect(offenders).toEqual([]);
  });
});

describe("buildCodeGraphModel layout space", () => {
  it("defaults to the fixed content space the canvas passes", async () => {
    const graph = await import("../codeintel/graphPayload");
    const payload = { nodes: [{ id: "a", name: "a" }, { id: "b", name: "b" }, { id: "c", name: "c" }], links: [{ source: "a", target: "b" }] };
    const fixed = graph.buildCodeGraphModel(payload, graph.CODE_GRAPH_LAYOUT_WIDTH, graph.CODE_GRAPH_LAYOUT_HEIGHT);
    expect(graph.buildCodeGraphModel(payload).nodes.map((n) => [n.x, n.y])).toEqual(fixed.nodes.map((n) => [n.x, n.y]));
  });
});
