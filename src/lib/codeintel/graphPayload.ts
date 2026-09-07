/**
 * Map a viz / map-preview payload into a render model and truncation legend.
 *
 * Never paper over `counts.nodes_truncated`: the legend always states shown
 * vs total when the sample is capped.
 */

import type { GraphVizCounts, GraphVizLink, GraphVizNode, GraphVizPayload } from "./types";
import { DOC_GRAPH_EXTENT } from "../docs/docGraphPayload";

export interface LaidOutNode extends GraphVizNode {
  x: number;
  y: number;
  radius: number;
  colorIndex: number;
}

export interface CodeGraphModel {
  nodes: LaidOutNode[];
  links: Array<GraphVizLink & { sourceIndex: number; targetIndex: number }>;
  communities: Array<{ name: string; count: number; colorIndex: number }>;
  counts: GraphVizCounts | null;
  level: string | null;
  generationId: string | null;
}

export interface TruncationLegend {
  /** Short status for the legend strip, e.g. "42 of 1200 nodes". */
  nodesLabel: string;
  linksLabel: string | null;
  /** True when the picture is a ranked sample, not the whole graph. */
  nodesTruncated: boolean;
  maxNodes: number | null;
  /** Human-readable honesty line when truncated. */
  honesty: string | null;
  coverageLabel: string | null;
}

function asFinite(n: unknown, fallback = 0): number {
  return typeof n === "number" && Number.isFinite(n) ? n : fallback;
}

export function normalizeCounts(raw: GraphVizCounts | null | undefined): GraphVizCounts | null {
  if (!raw || typeof raw !== "object") return null;
  const nodes_shown = asFinite(raw.nodes_shown);
  const nodes_total = asFinite(raw.nodes_total, nodes_shown);
  const nodes_truncated = Boolean(raw.nodes_truncated) || nodes_shown < nodes_total;
  return {
    nodes_shown,
    nodes_total,
    nodes_truncated,
    links_shown: raw.links_shown != null ? asFinite(raw.links_shown) : undefined,
    links_total: raw.links_total != null ? asFinite(raw.links_total) : undefined,
    max_nodes: raw.max_nodes != null ? asFinite(raw.max_nodes) : undefined,
  };
}

/**
 * Legend copy. Truncated samples always get an honesty line that names the
 * cap — never a bare "N nodes".
 */
export function truncationLegend(
  counts: GraphVizCounts | null,
  coverage?: { indexed_total?: number; in_subsystems?: number } | null,
): TruncationLegend {
  if (!counts) {
    const indexed = coverage?.indexed_total;
    const inSubs = coverage?.in_subsystems;
    const coverageLabel =
      indexed != null && inSubs != null
        ? `${inSubs} of ${indexed} indexed files in subsystems`
        : null;
    return {
      nodesLabel: "—",
      linksLabel: null,
      nodesTruncated: false,
      maxNodes: null,
      honesty: null,
      coverageLabel,
    };
  }

  const nodesTruncated = counts.nodes_truncated || counts.nodes_shown < counts.nodes_total;
  const nodesLabel = nodesTruncated
    ? `${counts.nodes_shown} of ${counts.nodes_total} nodes`
    : `${counts.nodes_total} nodes`;

  let linksLabel: string | null = null;
  if (counts.links_shown != null && counts.links_total != null) {
    linksLabel =
      counts.links_shown < counts.links_total
        ? `${counts.links_shown} of ${counts.links_total} links`
        : `${counts.links_total} links`;
  } else if (counts.links_shown != null) {
    linksLabel = `${counts.links_shown} links`;
  }

  const maxNodes = counts.max_nodes ?? null;
  const honesty = nodesTruncated
    ? maxNodes != null
      ? `Showing the ${counts.nodes_shown} most-connected of ${counts.nodes_total} (cap ${maxNodes}). This is not the whole graph.`
      : `Showing ${counts.nodes_shown} of ${counts.nodes_total}. This is not the whole graph.`
    : null;

  const indexed = coverage?.indexed_total;
  const inSubs = coverage?.in_subsystems;
  const coverageLabel =
    indexed != null && inSubs != null
      ? `${inSubs} of ${indexed} indexed files in subsystems`
      : null;

  return {
    nodesLabel,
    linksLabel,
    nodesTruncated,
    maxNodes,
    honesty,
    coverageLabel,
  };
}

/**
 * Deterministic 2D layout: community clusters on a ring, nodes on local spirals.
 * Honours payload `x`/`y` when both are finite. Doc-graph coords (`level` doc)
 * arrive in a 1000×1000 extent and are scaled into the canvas box here.
 */
export function layoutNodes(
  nodes: GraphVizNode[],
  width: number,
  height: number,
  options?: { scaleDocExtent?: boolean },
): LaidOutNode[] {
  const cx = width / 2;
  const cy = height / 2;
  const communityIndex = new Map<string, number>();
  let nextCommunity = 0;
  const pad = 24;
  const scaleDoc = Boolean(options?.scaleDocExtent);
  const docScaleX = (Math.max(160, width) - pad * 2) / DOC_GRAPH_EXTENT;
  const docScaleY = (Math.max(160, height) - pad * 2) / DOC_GRAPH_EXTENT;

  const withCoords = nodes.map((node) => {
    const community = (node.community || node.area || "").trim() || "_";
    if (!communityIndex.has(community)) {
      communityIndex.set(community, nextCommunity++);
    }
    const colorIndex = communityIndex.get(community) ?? 0;
    const degree = asFinite(node.degree, asFinite(node.val, 1));
    const radius = Math.min(14, 4 + Math.sqrt(Math.max(1, degree)));

    if (Number.isFinite(node.x) && Number.isFinite(node.y)) {
      let x = node.x as number;
      let y = node.y as number;
      if (scaleDoc) {
        x = pad + x * docScaleX;
        y = pad + y * docScaleY;
      }
      return {
        ...node,
        x,
        y,
        radius,
        colorIndex,
      };
    }
    return { ...node, x: 0, y: 0, radius, colorIndex, _community: community };
  });

  const byCommunity = new Map<string, number[]>();
  withCoords.forEach((node, i) => {
    if (Number.isFinite(nodes[i]?.x) && Number.isFinite(nodes[i]?.y)) return;
    const community = (node.community || node.area || "").trim() || "_";
    const list = byCommunity.get(community) ?? [];
    list.push(i);
    byCommunity.set(community, list);
  });

  const communities = [...byCommunity.keys()];
  const ringR = Math.min(width, height) * 0.32;
  communities.forEach((name, ci) => {
    const angle = (2 * Math.PI * ci) / Math.max(1, communities.length) - Math.PI / 2;
    const clusterX = cx + Math.cos(angle) * ringR;
    const clusterY = cy + Math.sin(angle) * ringR;
    const members = byCommunity.get(name) ?? [];
    members.forEach((idx, mi) => {
      const localAngle = (2 * Math.PI * mi) / Math.max(1, members.length);
      const localR = 18 + Math.min(80, members.length * 4);
      withCoords[idx].x = clusterX + Math.cos(localAngle) * localR;
      withCoords[idx].y = clusterY + Math.sin(localAngle) * localR;
    });
  });

  // Strip helper field
  return withCoords.map(({ ...rest }) => {
    const cleaned = { ...rest } as LaidOutNode & { _community?: string };
    delete cleaned._community;
    return cleaned;
  });
}

export function buildCodeGraphModel(
  payload: GraphVizPayload | null | undefined,
  width = 800,
  height = 560,
): CodeGraphModel {
  const nodesRaw = Array.isArray(payload?.nodes) ? payload!.nodes : [];
  const linksRaw = Array.isArray(payload?.links) ? payload!.links : [];
  const counts = normalizeCounts(payload?.counts ?? null);

  // For map-preview (no counts), synthesize shown==total from the node list so
  // the legend still has a number — but never invent a truncated=false claim
  // that hides coverage gaps; coverage rides separately.
  const effectiveCounts =
    counts ??
    (nodesRaw.length > 0
      ? {
          nodes_shown: nodesRaw.length,
          nodes_total: nodesRaw.length,
          nodes_truncated: false,
          links_shown: linksRaw.length,
          links_total: linksRaw.length,
        }
      : null);

  const nodes = layoutNodes(nodesRaw, width, height, {
    scaleDocExtent: payload?.level === "doc",
  });
  const indexById = new Map(nodes.map((n, i) => [n.id, i]));

  const links = linksRaw
    .map((link) => {
      const sourceIndex = indexById.get(link.source);
      const targetIndex = indexById.get(link.target);
      if (sourceIndex == null || targetIndex == null) return null;
      return { ...link, sourceIndex, targetIndex };
    })
    .filter((l): l is GraphVizLink & { sourceIndex: number; targetIndex: number } => l != null);

  const communityCounts = payload?.communities ?? {};
  const communities = Object.entries(communityCounts)
    .map(([name, count], colorIndex) => ({
      name,
      count: asFinite(count),
      colorIndex,
    }))
    .sort((a, b) => b.count - a.count || a.name.localeCompare(b.name));

  // When communities object is empty, derive from laid-out nodes.
  if (communities.length === 0) {
    const derived = new Map<string, { count: number; colorIndex: number }>();
    for (const node of nodes) {
      const name = (node.community || node.area || "").trim();
      if (!name) continue;
      const prev = derived.get(name);
      if (prev) prev.count += 1;
      else derived.set(name, { count: 1, colorIndex: node.colorIndex });
    }
    for (const [name, { count, colorIndex }] of derived) {
      communities.push({ name, count, colorIndex });
    }
    communities.sort((a, b) => b.count - a.count || a.name.localeCompare(b.name));
  }

  const gen = payload?.generation_id;
  return {
    nodes,
    links,
    communities,
    counts: effectiveCounts,
    level: payload?.level ?? null,
    generationId: gen == null ? null : String(gen),
  };
}
