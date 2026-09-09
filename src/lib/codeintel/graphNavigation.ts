import { isGraphTestPath, nodeCommunity, nodeLanguageKey, type CodeGraphModel, type LaidOutNode } from "./graphPayload";
import type { GraphVizLink, GraphVizNode } from "./types";
import type { LanguageIconKey } from "../language/languageLogos";

export type TraceDirection = "outgoing" | "incoming" | "both";

export function fitGraphView(nodes: readonly LaidOutNode[], width: number, height: number): {scale:number;panX:number;panY:number} {
  if (!nodes.length) return {scale:1,panX:0,panY:0};
  width = Number.isFinite(width) ? Math.max(160,width) : 640;
  height = Number.isFinite(height) ? Math.max(160,height) : 420;
  let minX=Infinity, maxX=-Infinity, minY=Infinity, maxY=-Infinity;
  for (const node of nodes) {
    minX=Math.min(minX,node.x-node.radius); maxX=Math.max(maxX,node.x+node.radius);
    minY=Math.min(minY,node.y-node.radius); maxY=Math.max(maxY,node.y+node.radius);
  }
  const scale=Math.min(5, (width-64)/Math.max(1,maxX-minX), (height-64)/Math.max(1,maxY-minY));
  return {scale,panX:width/2-(minX+maxX)/2*scale,panY:height/2-(minY+maxY)/2*scale};
}

export interface GraphIndex {
  nodes: ReadonlyMap<string, LaidOutNode>;
  outgoing: ReadonlyMap<string, number[]>;
  incoming: ReadonlyMap<string, number[]>;
  related: ReadonlyMap<string, number[]>;
  links: CodeGraphModel["links"];
}

/** The subsystem producer deduplicates neighbor pairs without direction. */
export function isUndirectedGraphLink(link: GraphVizLink): boolean {
  return link.kind === "neighbor";
}

/** Build once per payload/layout, not once per pointer move or traversal step. */
export function buildGraphIndex(model: CodeGraphModel): GraphIndex {
  const nodes = new Map(model.nodes.map(n => [n.id, n]));
  const outgoing = new Map<string, number[]>(), incoming = new Map<string, number[]>();
  const related = new Map<string, number[]>();
  for (const [i, link] of model.links.entries()) {
    if (isUndirectedGraphLink(link)) {
      for (const id of new Set([link.source,link.target])) {
        const rows = related.get(id) ?? [];
        rows.push(i); related.set(id,rows);
      }
      continue;
    }
    const out = outgoing.get(link.source) ?? [], into = incoming.get(link.target) ?? [];
    out.push(i); into.push(i);
    outgoing.set(link.source, out); incoming.set(link.target, into);
  }
  // Stable traversal even when producers return their links in another order.
  for (const [direction, rows] of [["target", outgoing], ["source", incoming]] as const) {
    for (const indices of rows.values()) indices.sort((a, b) =>
      model.links[a][direction].localeCompare(model.links[b][direction]) ||
      (model.links[a].kind ?? "").localeCompare(model.links[b].kind ?? ""));
  }
  for (const [id, rows] of related) rows.sort((a,b) => {
    const first = model.links[a], second = model.links[b];
    return (first.source === id ? first.target : first.source).localeCompare(second.source === id ? second.target : second.source);
  });
  return { nodes, outgoing, incoming, related, links: model.links };
}

export interface GraphFilters {
  query?: string;
  community?: string | null;
  language?: LanguageIconKey | null;
  hideTests?: boolean;
  hideGenerated?: boolean;
  hideNotes?: boolean;
  connectedOnly?: boolean;
}

export function filterGraphNodes(model: CodeGraphModel, index: GraphIndex, filters: GraphFilters): LaidOutNode[] {
  const query = (filters.query ?? "").trim().toLowerCase();
  return model.nodes.filter(node => {
    const path = (node.path || node.id.split("::")[0]).replace(/\\/g, "/");
    if (filters.community && nodeCommunity(node) !== filters.community) return false;
    if (filters.language && nodeLanguageKey(node) !== filters.language) return false;
    if (filters.hideTests && isGraphTestPath(path)) return false;
    if (filters.hideNotes && node.documentation === true) return false;
    if (filters.hideGenerated && (/(^|\/)(generated|__generated__|vendor|vendored|node_modules|dist|build|target)(\/|$)/i.test(path) || /(?:\.generated|\.g|\.min)\.[^/]+$/i.test(path))) return false;
    if (filters.connectedOnly && !index.incoming.has(node.id) && !index.outgoing.has(node.id) && !index.related.has(node.id)) return false;
    return !query || `${node.name} ${path} ${node.id}`.toLowerCase().includes(query);
  });
}

/** Trace a bounded neighborhood; a capped result explicitly remains incomplete. */
export function traceGraph(index: GraphIndex, root: string, direction: TraceDirection, depth = 1, allowed?: ReadonlySet<string>): {
  ids: ReadonlySet<string>; edges: ReadonlySet<number>; truncated: boolean; visitedEdges: number;
} {
  const ids = new Set<string>();
  const edges = new Set<number>();
  if (!index.nodes.has(root) || (allowed && !allowed.has(root))) return { ids, edges, truncated: false, visitedEdges: 0 };
  const hops = Number.isFinite(depth) ? Math.max(1, Math.min(3, Math.floor(depth))) : 1;
  const queue = [{ id: root, depth: 0 }];
  ids.add(root);
  let visitedEdges = 0;
  for (let cursor = 0; cursor < queue.length; cursor++) {
    const current = queue[cursor];
    if (current.depth >= hops) continue;
    const steps: Array<[ReadonlyMap<string, number[]>, "source" | "target" | "other"]> = [[index.related,"other"]];
    if (direction !== "incoming") steps.push([index.outgoing, "target"]);
    if (direction !== "outgoing") steps.push([index.incoming, "source"]);
    for (const [rows, end] of steps) {
      for (const edgeIndex of rows.get(current.id) ?? []) {
        if (visitedEdges >= 50_000) return {ids, edges, truncated:true, visitedEdges};
        visitedEdges++;
        const link = index.links[edgeIndex];
        const id = end === "other" ? (link.source === current.id ? link.target : link.source) : link[end];
        if (allowed && !allowed.has(id)) continue;
        if (ids.has(id)) { edges.add(edgeIndex); continue; }
        if (ids.size >= 1000) return {ids, edges, truncated:true, visitedEdges};
        edges.add(edgeIndex);
        ids.add(id); queue.push({id, depth:current.depth + 1});
      }
    }
  }
  return {ids, edges, truncated:false, visitedEdges};
}

/** File ownership is explicit; a language-extension whitelist loses valid files. */
export function graphNodeOpenPath(node: GraphVizNode): string | null {
  if (node.kind === "subsystem") return null;
  if (node.path?.trim()) return node.path;
  if (node.kind === "file" || node.kind === "doc") return node.id || null;
  const separator = node.id.indexOf("::");
  if (separator > 0) {
    const prefix = node.id.slice(0, separator);
    if (/[\\/]|\.[a-z0-9]+$/i.test(prefix)) return prefix;
  }
  return null;
}
