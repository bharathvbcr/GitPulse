import { describe, expect, it } from "vitest";
import {
  buildCodeGraphModel,
  layoutNodes,
  nodeLanguageKey,
  graphLanguageLegend,
  normalizeCounts,
  truncationLegend,
} from "./graphPayload";
import type { GraphVizNode, GraphVizPayload } from "./types";

describe("map languages", () => {
  it.each([
    [{ language: "Swift", path: "override.rs" }, "swift"],
    [{ language: "  RUST  " }, "rust"],
    [{ lang: "C++" }, "cpp"],
    [{ language: "unrecognized", path: "src/main.rs" }, "rust"],
    [{ path: "src/Component.tsx" }, "typescript"],
    [{ id: "src/main.rs::run" }, "rust"],
    [{ path: "C:\\src\\main.py" }, "python"],
    [{ kind: "file", name: "Dockerfile" }, "docker"],
    [{ kind: "subsystem", name: "Rust" }, "file"],
    [{}, "file"],
  ] satisfies Array<[Partial<GraphVizNode>, string]>)("resolves %j as %s", (fields, expected) => {
    expect(nodeLanguageKey({ id: "opaque", name: "opaque", ...fields })).toBe(expected);
  });

  it("counts aliases together and includes unknown nodes in the visible legend", () => {
    expect(graphLanguageLegend([
      {id: "a", name: "a", language: "Rust"}, {id: "b.rs::run", name: "run"},
      {id: "c", name: "c", lang: "Swift"}, {id: "opaque", name: "opaque"},
    ])).toEqual([
      {key: "rust", name: "Rust", count: 2},
      {key: "file", name: "Other / unknown", count: 1},
      {key: "swift", name: "Swift", count: 1},
    ]);
    expect(graphLanguageLegend([])).toEqual([]);
  });
});

describe("community layout", () => {
  const dense: GraphVizNode[] = Array.from({ length: 288 }, (_, i) => ({
    id: `src/file-${i}.ts`, name: `file-${i}.ts`, community: `group-${Math.floor(i / 83)}`,
    degree: i % 13,
  }));

  it("keeps dense communities apart and inside the viewport", () => {
    const nodes = layoutNodes(dense, 1000, 650);
    for (const [i, a] of nodes.entries()) {
      expect(a.x - a.radius).toBeGreaterThanOrEqual(0);
      expect(a.x + a.radius).toBeLessThanOrEqual(1000);
      expect(a.y - a.radius).toBeGreaterThanOrEqual(0);
      expect(a.y + a.radius).toBeLessThanOrEqual(650);
      for (const b of nodes.slice(i + 1)) {
        expect(Math.hypot(a.x - b.x, a.y - b.y)).toBeGreaterThan(a.radius + b.radius);
      }
    }
  });

  it("does not reshuffle coordinates or colors when payload order changes", () => {
    const forward = layoutNodes(dense, 1000, 650);
    const reverse = new Map(layoutNodes([...dense].reverse(), 1000, 650).map(n => [n.id, n]));
    for (const node of forward) expect(reverse.get(node.id)).toEqual(node);
  });

  it("uses dependencies to shorten edges without overlapping nodes or depending on row order", () => {
    const nodes = dense.slice(0, 70);
    const links = nodes.slice(1).map((n,i) => ({source:n.id,target:nodes[(i * 7) % nodes.length].id,kind:"calls"}));
    const before = buildCodeGraphModel({nodes,links:[]},1000,650);
    const after = buildCodeGraphModel({nodes,links},1000,650);
    const length = (rows: typeof after.nodes) => {
      const byId = new Map(rows.map(n=>[n.id,n]));
      return links.reduce((sum,l) => {
        const a=byId.get(l.source)!, b=byId.get(l.target)!;
        return sum + Math.hypot(a.x-b.x,a.y-b.y);
      },0);
    };
    expect(length(after.nodes)).toBeLessThan(length(before.nodes) * 0.9);
    const reversed = new Map(buildCodeGraphModel({nodes:[...nodes].reverse(),links:[...links].reverse()},1000,650).nodes.map(n=>[n.id,n]));
    for (const [i,a] of after.nodes.entries()) {
      expect(reversed.get(a.id)).toEqual(a);
      for (const b of after.nodes.slice(i+1)) expect(Math.hypot(a.x-b.x,a.y-b.y)).toBeGreaterThan(a.radius+b.radius);
    }
  });

  it("keeps singleton groups and invalid viewport dimensions finite", () => {
    const nodes = layoutNodes(dense.slice(0, 40).map(n => ({ ...n, community: n.id })), NaN, Infinity);
    for (const node of nodes) {
      expect(Number.isFinite(node.x)).toBe(true);
      expect(Number.isFinite(node.y)).toBe(true);
    }
    expect(layoutNodes([], 0, 0)).toEqual([]);
  });

  it("uses the node color for the legend even when community counts arrive in another order", () => {
    const model = buildCodeGraphModel({ nodes: dense, links: [], communities: {
      "group-3": 39, "group-2": 83, "group-1": 83, "group-0": 83,
    }});
    for (const group of model.communities) {
      expect(group.colorIndex).toBe(model.nodes.find(n => n.community === group.name)?.colorIndex);
    }
  });

  it("fits the maximum 5000-node payload in a narrow panel without mutating inputs", () => {
    const input = Array.from({length: 5000}, (_, i) => Object.freeze({
      id: `file-${i}`, name: `file-${i}`, community: `group-${i % 37}`, degree: i % 200,
    }));
    const model = buildCodeGraphModel({ nodes: input, links: [] }, 320, 240);
    expect(model.nodes).toHaveLength(5000);
    for (const node of model.nodes) {
      expect(node.x - node.radius).toBeGreaterThanOrEqual(0);
      expect(node.x + node.radius).toBeLessThanOrEqual(320);
      expect(node.y - node.radius).toBeGreaterThanOrEqual(0);
      expect(node.y + node.radius).toBeLessThanOrEqual(240);
    }
  });
});

describe("community labels", () => {
  const file = (path: string, community = "community-7"): GraphVizNode => ({
    id: path, name: path.split(/[\\/]/).pop() ?? path, path, community,
  });
  const groups = (nodes: GraphVizNode[]) => buildCodeGraphModel({ nodes, links: [] }).communities;

  it("names a mixed group for its source area without dropping tests from its count", () => {
    const nodes = [
      ...Array.from({ length: 46 }, (_, i) => file(`app/Tests/Editor${i}Tests.swift`)),
      ...Array.from({ length: 26 }, (_, i) => file(`app/MarkDevKit/Editor/Editor${i}.swift`)),
      file("app/MarkDevKit/Core/Model.swift"),
    ];
    expect(groups(nodes)[0]).toMatchObject({ label: "MarkDevKit/Editor", shown: 73, count: 73 });
  });

  it.each([
    "app/__tests__/editor.test.ts", "app/specs/editor.spec.ts",
    "app/Checks/EditorTests.swift", "app/checks/test_editor.py", "app/checks/editor_test.go",
    "app\\Tests\\EditorTests.swift",
  ])("recognizes test naming when choosing a label: %s", (path) => {
    expect(groups([file(path), { ...file(path), id: `${path}::second` }, file("app/Editor/Main.swift")])[0].label)
      .toBe("app/Editor");
  });

  it("preserves test-only and explicitly named communities", () => {
    expect(groups([file("app/Tests/EditorTests.swift")])[0].label).toBe("app/Tests");
    expect(groups([file("app/Tests/EditorTests.swift", "Editor lifecycle")])[0].label).toBe("Editor lifecycle");
    expect(groups([{ id: "opaque", name: "opaque", community: "community-9" }])[0].label).toBe("Group 9");
  });

  it("counts distinct files rather than letting one file's symbols dominate its area", () => {
    const nodes = [file("app/Editor/A.swift"), file("app/Editor/B.swift"),
      ...Array.from({ length: 8 }, (_, i) => ({ ...file("app/Core/C.swift"), id: `app/Core/C.swift::${i}` }))];
    expect(groups(nodes)[0].label).toBe("app/Editor");
  });

  it("disambiguates repeated inferred labels using stable community IDs", () => {
    const nodes = [file("app/Editor/A.swift", "community-7"), file("app/Editor/B.swift", "community-9")];
    const labels = groups(nodes).map(g => [g.name, g.label]);
    expect(labels).toEqual([["community-7", "app/Editor · #7"], ["community-9", "app/Editor · #9"]]);
    expect(groups([...nodes].reverse()).map(g => [g.name, g.label])).toEqual(labels);
  });
});

describe("normalizeCounts", () => {
  it("normalizes negative, fractional, and inconsistent counts", () => {
    expect(normalizeCounts({ nodes_shown: 3.9, nodes_total: -10, nodes_truncated: false, links_shown: -2, links_total: -9, max_nodes: NaN }))
      .toMatchObject({ nodes_shown: 3, nodes_total: 3, links_shown: 0, links_total: 0 });
  });
  it("treats shown < total as truncated even if the flag is false", () => {
    const counts = normalizeCounts({
      nodes_shown: 2,
      nodes_total: 10,
      nodes_truncated: false,
      max_nodes: 2,
    });
    expect(counts?.nodes_truncated).toBe(true);
  });

  it("preserves an honest untruncated sample", () => {
    const counts = normalizeCounts({
      nodes_shown: 5,
      nodes_total: 5,
      nodes_truncated: false,
    });
    expect(counts?.nodes_truncated).toBe(false);
  });
});

describe("untrusted graph payloads", () => {
  it("keeps deterministic malformed-payload fuzz cases finite and free of dangling endpoints", () => {
    let seed = 0x5eeda11;
    const random = (limit:number) => { seed ^= seed << 13; seed ^= seed >>> 17; seed ^= seed << 5; return (seed >>> 0) % limit; };
    const values: unknown[] = [null, false, 7, {}, [], NaN, Infinity, "valid"];
    for (let round=0;round<300;round++) {
      const nodes = Array.from({length:40},(_,i) => random(4) === 0 ? values[random(values.length)] : {
        id:random(5) === 0 ? "ambiguous" : `n${i}`, name:values[random(values.length)],
        path:random(2) ? `src/file-${i}.rs` : values[random(values.length)],
        community:values[random(values.length)], language:values[random(values.length)],
        degree:values[random(values.length)], x:values[random(values.length)], y:values[random(values.length)],
        flags:[values[random(values.length)],{flag:values[random(values.length)]}],
      });
      const links = Array.from({length:80},()=>random(4) === 0 ? values[random(values.length)] : {
        source:`n${random(50)}`,target:`n${random(50)}`,kind:values[random(values.length)],confidence:values[random(values.length)],
      });
      const graph = buildCodeGraphModel({nodes,links,counts:values[random(values.length)]},round%2 ? 320 : NaN,round%3 ? 240 : Infinity);
      expect(new Set(graph.nodes.map(n=>n.id)).size).toBe(graph.nodes.length);
      for (const n of graph.nodes) expect([n.x,n.y,n.radius].every(Number.isFinite)).toBe(true);
      for (const l of graph.links) {
        expect(graph.nodes[l.sourceIndex]?.id).toBe(l.source);
        expect(graph.nodes[l.targetIndex]?.id).toBe(l.target);
      }
      expect(graph.counts?.nodes_shown).toBe(graph.nodes.length);
      expect(graph.counts?.links_shown).toBe(graph.links.length);
    }
  });
  it("rejects malformed nodes, duplicate identities, and phantom links with visible diagnostics", () => {
    const payload = JSON.parse('{"nodes":[null,{"id":"a","name":7,"community":9,"language":{}},{"id":"duplicate","path":"one.ts"},{"id":"duplicate","path":"two.ts"},{"id":"b","name":"B"}],"links":[null,{"source":"a","target":"missing"},{"source":"duplicate","target":"b"},{"source":"a","target":"b"}]}');
    const model = buildCodeGraphModel(payload);
    expect(model.nodes.map(n => n.id).sort()).toEqual(["a", "b"]);
    expect(model.links).toHaveLength(1);
    expect(model.counts?.nodes_shown).toBe(2);
    expect(model.warnings.join(" ")).toMatch(/invalid|ambiguous/i);
  });

  it("reports link-only truncation and source projection omissions", () => {
    const model = buildCodeGraphModel({ nodes: [{id:"a",name:"A"},{id:"b",name:"B"}], links:[{source:"a",target:"b"}],
      counts:{nodes_shown:2,nodes_total:2,nodes_truncated:false,links_shown:1,links_total:8},
      meta:{projection:{invalid_edges:3,invalid_nodes:0}},
    });
    expect(truncationLegend(model.counts).honesty).toContain("1 of 8 links");
    expect(model.warnings.join(" ")).toContain("3");
  });

  it("bounds oversized input and reports the rendered sample accurately", () => {
    const nodes = Array.from({length: 5100}, (_, i) => ({id:`n${i}`,name:`N${i}`,degree:i}));
    const model = buildCodeGraphModel({nodes,links:[]});
    expect(model.nodes).toHaveLength(5000);
    expect(model.nodes.some(n => n.id === "n5099")).toBe(true);
    expect(model.counts).toMatchObject({nodes_shown:5000,nodes_total:5100,nodes_truncated:true});
  });
});

describe("truncationLegend", () => {
  it.each(['null','{}','{"indexed_total":"400","in_subsystems":3}','{"indexed_total":4,"in_subsystems":20}','{"indexed_total":4,"in_subsystems":{}}'])("omits invalid coverage metadata: %s", raw => {
    expect(truncationLegend(null, JSON.parse(raw)).coverageLabel).toBeNull();
  });
  it("never labels a truncated sample as just N nodes", () => {
    const legend = truncationLegend({
      nodes_shown: 1500,
      nodes_total: 12000,
      nodes_truncated: true,
      links_shown: 4000,
      links_total: 50000,
      max_nodes: 1500,
    });
    expect(legend.nodesTruncated).toBe(true);
    expect(legend.nodesLabel).toBe("1500 of 12000 nodes");
    expect(legend.nodesLabel).not.toMatch(/^1500 nodes$/);
    expect(legend.honesty).toContain("not the whole graph");
    expect(legend.honesty).toContain("cap 1500");
    expect(legend.linksLabel).toBe("4000 of 50000 links");
  });

  it("omits honesty when the sample is complete", () => {
    const legend = truncationLegend({
      nodes_shown: 12,
      nodes_total: 12,
      nodes_truncated: false,
      links_shown: 20,
      links_total: 20,
    });
    expect(legend.nodesTruncated).toBe(false);
    expect(legend.nodesLabel).toBe("12 nodes");
    expect(legend.honesty).toBeNull();
  });

  it("surfaces map-preview coverage without inventing node truncation", () => {
    const legend = truncationLegend(null, {
      indexed_total: 400,
      in_subsystems: 310,
    });
    expect(legend.coverageLabel).toBe("310 of 400 indexed files in subsystems");
    expect(legend.nodesTruncated).toBe(false);
  });
});

describe("buildCodeGraphModel", () => {
  const truncatedPayload: GraphVizPayload = {
    level: "file",
    generation_id: 9,
    communities: { core: 2, ui: 1 },
    counts: {
      nodes_shown: 2,
      nodes_total: 5,
      nodes_truncated: true,
      links_shown: 1,
      links_total: 4,
      max_nodes: 2,
    },
    nodes: [
      {
        id: "a.ts",
        name: "a.ts",
        kind: "file",
        path: "a.ts",
        community: "core",
        degree: 3,
      },
      {
        id: "b.ts",
        name: "b.ts",
        kind: "file",
        path: "b.ts",
        community: "ui",
        degree: 1,
      },
    ],
    links: [{ source: "a.ts", target: "b.ts", kind: "imports", confidence: 0.9 }],
  };

  it("maps payload nodes/links and keeps truncation counts", () => {
    const model = buildCodeGraphModel(truncatedPayload, 640, 400);
    expect(model.nodes).toHaveLength(2);
    expect(model.links).toHaveLength(1);
    expect(model.links[0].sourceIndex).toBe(0);
    expect(model.links[0].targetIndex).toBe(1);
    expect(model.counts?.nodes_truncated).toBe(true);
    expect(model.counts?.nodes_shown).toBe(2);
    expect(model.counts?.nodes_total).toBe(5);
    expect(model.level).toBe("file");
    expect(model.generationId).toBe("9");
    expect(model.communities.some((c) => c.name === "core")).toBe(true);
    // Laid out with finite coordinates.
    expect(Number.isFinite(model.nodes[0].x)).toBe(true);
    expect(Number.isFinite(model.nodes[0].y)).toBe(true);
  });

  it("honours payload coordinates when provided", () => {
    const model = buildCodeGraphModel(
      {
        ...truncatedPayload,
        nodes: [
          { ...truncatedPayload.nodes[0], x: 10, y: 20 },
          { ...truncatedPayload.nodes[1], x: 30, y: 40 },
        ],
      },
      640,
      400,
    );
    expect(model.nodes[0].x).toBe(10);
    expect(model.nodes[0].y).toBe(20);
    expect(model.nodes[1].x).toBe(30);
    expect(model.nodes[1].y).toBe(40);
  });

  it("drops links whose endpoints were truncated away", () => {
    const model = buildCodeGraphModel({
      ...truncatedPayload,
      links: [
        { source: "a.ts", target: "b.ts", kind: "imports" },
        { source: "a.ts", target: "missing.ts", kind: "imports" },
      ],
    });
    expect(model.links).toHaveLength(1);
    expect(model.links[0].target).toBe("b.ts");
  });
});
