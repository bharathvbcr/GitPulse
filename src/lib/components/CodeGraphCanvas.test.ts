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
