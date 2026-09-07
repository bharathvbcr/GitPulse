/**
 * Map a MarkDev doc-graph payload onto the Section C code-graph canvas model.
 *
 * Doc nodes carry force-directed `x`/`y` in a 1000×1000 extent. Coordinates
 * stay in that space here; `buildCodeGraphModel` scales them into the canvas
 * CSS box when `level === "doc"` so resize stays correct. Truncation honesty
 * (`truncated`, `total_notes`) always reaches `counts`.
 */

import type { GraphVizLoad, GraphVizPayload } from "../codeintel/types";
import type { DocGraph } from "./client";

/** MarkDev vault layout extent (see vault/graph.rs). */
export const DOC_GRAPH_EXTENT = 1000;

/** Cap documented beside the search IPC default. */
export const DOCS_SEARCH_DEFAULT_LIMIT = 50;

export function docGraphToVizPayload(graph: DocGraph): GraphVizPayload {
  const nodes = graph.nodes.map((node, i) => {
    const folder = node.path.includes("/")
      ? node.path.slice(0, node.path.lastIndexOf("/"))
      : "_root";
    return {
      id: node.path || `doc:${i}`,
      name: node.title || node.path || `doc:${i}`,
      kind: "doc",
      path: node.path,
      community: folder,
      degree: node.degree,
      // Keep the vault's 1000×1000 coords; layout scales into the canvas.
      x: node.x,
      y: node.y,
    };
  });

  const links = graph.edges.map((edge) => {
    const source = nodes[edge.source]?.id ?? String(edge.source);
    const target = nodes[edge.target]?.id ?? String(edge.target);
    return { source, target, kind: "doc_link" as const };
  });

  const shown = nodes.length;
  const total = Math.max(shown, graph.total_notes ?? shown);
  const truncated = Boolean(graph.truncated) || shown < total;

  return {
    level: "doc",
    nodes,
    links,
    counts: {
      nodes_shown: shown,
      nodes_total: total,
      nodes_truncated: truncated,
      links_shown: links.length,
      links_total: links.length,
      max_nodes: truncated ? shown : undefined,
    },
  };
}

export function docGraphToLoad(
  graph: DocGraph | null,
  reason: string | null,
): GraphVizLoad {
  if (!graph) {
    return {
      available: false,
      reason: reason ?? "Doc graph unavailable",
      kind: "doc_graph",
      payload: null,
    };
  }
  return {
    available: true,
    reason: null,
    kind: "doc_graph",
    payload: docGraphToVizPayload(graph),
  };
}
