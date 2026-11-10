/**
 * Map a viz / map-preview payload into a render model and truncation legend.
 *
 * Never paper over `counts.nodes_truncated`: the legend always states shown
 * vs total when the sample is capped.
 */

import type { GraphVizCounts, GraphVizLink, GraphVizNode } from "./types";
import { DOC_GRAPH_EXTENT } from "../docs/docGraphPayload";
import { getLanguageDisplayName, resolveLanguageIconKey, type LanguageIconKey } from "../language/languageLogos";
import { nodeFilePath } from "./nodeLabel";
import { disambiguateSuffixes, splitSegments } from "./labelDisambiguation";
import { asRecord } from "../record";

/**
 * Metadata wins; older payloads can still identify a language by source path.
 *
 * The `::` prefix is consulted only when it actually names a file. Taking it
 * unconditionally handed bare namespaces to the language resolver, so a symbol
 * like `Rust::new` or `Swift::make` was labelled by its namespace rather than
 * by its file.
 */
export function nodeLanguageKey(node: GraphVizNode): LanguageIconKey {
  for (const candidate of [node.language, node.lang, node.path, nodeFilePath(node.id), node.kind === "file" ? node.name : ""]) {
    const key = resolveLanguageIconKey(candidate ?? "");
    if (key !== "file") return key;
  }
  return "file";
}

/** Counts describe the rendered sample, including nodes of unknown language. */
export function graphLanguageLegend(nodes: GraphVizNode[]): Array<{ key: LanguageIconKey; name: string; count: number }> {
  const counts = new Map<LanguageIconKey, number>();
  for (const node of nodes) {
    const key = nodeLanguageKey(node);
    counts.set(key, (counts.get(key) ?? 0) + 1);
  }
  return [...counts].map(([key, count]) => ({
    key, name: key === "file" ? "Other / unknown" : getLanguageDisplayName(key), count,
  })).sort((a, b) => b.count - a.count || a.name.localeCompare(b.name));
}

export interface LaidOutNode extends GraphVizNode {
  /**
   * What the canvas shows: the shortest path suffix that is unique among the
   * model's nodes of the same kind. Display only: search, ids and open-in-editor
   * keep using `id`, `name` and `path`.
   */
  label: string;
  x: number;
  y: number;
  radius: number;
  colorIndex: number;
}

export interface GraphCommunity {
  name: string;
  label: string;
  count: number;
  shown: number;
  colorIndex: number;
  bounds: { x: number; y: number; radius: number } | null;
}

export function nodeCommunity(node: GraphVizNode): string {
  return (node.community || node.area || "").trim() || "_";
}

export interface CodeGraphModel {
  nodes: LaidOutNode[];
  links: Array<GraphVizLink & { sourceIndex: number; targetIndex: number }>;
  communities: GraphCommunity[];
  counts: GraphVizCounts | null;
  level: string | null;
  generationId: string | null;
  warnings: string[];
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

function count(value: unknown): number {
  return Math.min(Number.MAX_SAFE_INTEGER, Math.max(0, Math.floor(asFinite(value))));
}

function textValue(value: unknown): string | undefined {
  return typeof value === "string" && value.length <= 16_384 ? value : undefined;
}

export function normalizeCounts(raw: unknown): GraphVizCounts | null {
  const value = asRecord(raw);
  if (!value) return null;
  const nodes_shown = count(value.nodes_shown);
  const nodes_total = Math.max(nodes_shown, count(value.nodes_total));
  return {
    nodes_shown, nodes_total,
    nodes_truncated: value.nodes_truncated === true || nodes_shown < nodes_total,
    links_shown: value.links_shown != null ? count(value.links_shown) : undefined,
    links_total: value.links_total != null ? Math.max(count(value.links_shown), count(value.links_total)) : undefined,
    max_nodes: value.max_nodes != null ? count(value.max_nodes) : undefined,
  };
}

function graphCoverageLabel(raw: unknown): string | null {
  const value = asRecord(raw);
  const indexed = value?.indexed_total, inSubs = value?.in_subsystems;
  if (typeof indexed !== "number" || typeof inSubs !== "number" ||
      !Number.isSafeInteger(indexed) || !Number.isSafeInteger(inSubs) ||
      indexed < 0 || inSubs < 0 || inSubs > indexed) return null;
  return `${inSubs} of ${indexed} indexed files in subsystems`;
}

/**
 * Legend copy. Truncated samples always get an honesty line that names the
 * cap — never a bare "N nodes".
 */
export function truncationLegend(
  counts: GraphVizCounts | null,
  coverage?: unknown,
): TruncationLegend {
  const coverageLabel = graphCoverageLabel(coverage);
  if (!counts) {
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
  let honesty = nodesTruncated
    ? maxNodes != null
      ? `Showing ${counts.nodes_shown} of ${counts.nodes_total} nodes (cap ${maxNodes}). This is not the whole graph.`
      : `Showing ${counts.nodes_shown} of ${counts.nodes_total}. This is not the whole graph.`
    : null;

  if (counts.links_shown != null && counts.links_total != null && counts.links_shown < counts.links_total) {
    honesty = [honesty, `Showing ${counts.links_shown} of ${counts.links_total} links; some relationships are outside this view.`].filter(Boolean).join(" ");
  }

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
 * Pack size-aware communities into shelves, then fit the whole arrangement.
 * A golden-angle disk distributes members through each group's interior;
 * high-degree nodes start near its center. Dependency-based slot refinement
 * shortens edges without introducing collisions or a continuously moving map.
 * Supplied coordinates (including the document graph) remain authoritative.
 */
export function layoutNodes(
  nodes: GraphVizNode[],
  width: number,
  height: number,
  options?: { scaleDocExtent?: boolean; links?: readonly GraphVizLink[] },
): LaidOutNode[] {
  width = Math.max(160, asFinite(width, 800));
  height = Math.max(160, asFinite(height, 560));
  const names = [...new Set(nodes.map(nodeCommunity))].sort();
  const colors = new Map(names.map((name, index) => [name, index]));
  const byCommunity = new Map<string, LaidOutNode[]>();
  const result = nodes.map((node): LaidOutNode => {
    const degree = Math.max(1, asFinite(node.degree, asFinite(node.val, 1)));
    const laidOut = {
      ...node, label: node.name || node.id, x: 0, y: 0,
      radius: Math.min(9, 4 + Math.sqrt(degree)),
      colorIndex: colors.get(nodeCommunity(node)) ?? 0,
    };
    if (typeof node.x === "number" && Number.isFinite(node.x) &&
        typeof node.y === "number" && Number.isFinite(node.y)) {
      laidOut.x = options?.scaleDocExtent ? 24 + node.x * (width - 48) / DOC_GRAPH_EXTENT : node.x;
      laidOut.y = options?.scaleDocExtent ? 24 + node.y * (height - 48) / DOC_GRAPH_EXTENT : node.y;
    } else {
      const name = nodeCommunity(node);
      const members = byCommunity.get(name) ?? [];
      members.push(laidOut);
      byCommunity.set(name, members);
    }
    return laidOut;
  });
  const groups = [...byCommunity].map(([name, members]) => ({
    name, members, radius: 24 * Math.sqrt(members.length) + 34, x: 0, y: 0,
  })).sort((a, b) => b.members.length - a.members.length || a.name.localeCompare(b.name));
  const area = groups.reduce((sum, group) => sum + (group.radius * 2) ** 2, 0);
  const shelfWidth = Math.max(groups[0]?.radius * 2 || 0, Math.sqrt(area * width / height));
  let x = 0, y = 0, rowHeight = 0, usedWidth = 0;
  for (const group of groups) {
    const size = group.radius * 2;
    if (x > 0 && x + size > shelfWidth) { y += rowHeight; x = 0; rowHeight = 0; }
    group.x = x + group.radius;
    group.y = y + group.radius;
    x += size;
    rowHeight = Math.max(rowHeight, size);
    usedWidth = Math.max(usedWidth, x);
  }
  const usedHeight = y + rowHeight;
  const fit = Math.min((width - 48) / Math.max(1, usedWidth), (height - 64) / Math.max(1, usedHeight));
  const offsetX = (width - usedWidth * fit) / 2;
  const offsetY = (height - usedHeight * fit) / 2;
  for (const group of groups) {
    group.members.sort((a, b) => b.radius - a.radius || a.id.localeCompare(b.id));
    group.members.forEach((node, index) => {
      const angle = index * Math.PI * (3 - Math.sqrt(5));
      const distance = 24 * Math.sqrt(index);
      node.x = offsetX + (group.x + Math.cos(angle) * distance) * fit;
      node.y = offsetY + (group.y + Math.sin(angle) * distance) * fit;
      node.radius *= Math.min(1.6, fit);
    });
  }
  refineDependencyLayout(result, groups.map(group => group.members), options?.links ?? []);
  return result;
}

/**
 * Swap occupied slots only when total squared edge length decreases. Spacing
 * and group separation survive by construction; supplied coordinates never
 * move. Stable node/neighbor order and an operation budget make refreshes
 * deterministic, including when a dense graph exhausts the refinement budget.
 */
function refineDependencyLayout(nodes: LaidOutNode[], groups: LaidOutNode[][], links: readonly GraphVizLink[]): void {
  if (!links.length) return;
  const byId = new Map(nodes.map(node => [node.id, node]));
  const neighbors = new Map<string, LaidOutNode[]>();
  for (const link of links) {
    const a = byId.get(link.source), b = byId.get(link.target);
    if (!a || !b || a === b) continue;
    const out = neighbors.get(a.id) ?? [], into = neighbors.get(b.id) ?? [];
    out.push(b); into.push(a);
    neighbors.set(a.id, out); neighbors.set(b.id, into);
  }
  for (const rows of neighbors.values()) rows.sort((a,b) => a.id.localeCompare(b.id));
  let visits = 0;
  for (let pass = 0; pass < 8; pass++) {
    for (const members of groups) {
      for (let i = 0; i < members.length; i++) {
        const a = members[i];
        for (let trial = 0; trial < 3; trial++) {
          // Three reproducible candidates per pass; no random or wall-clock seed.
          const b = members[(i + 1 + ((i * 31 + pass * 131 + trial * 47) % members.length)) % members.length];
          if (a === b) continue;
          const from = neighbors.get(a.id) ?? [], to = neighbors.get(b.id) ?? [];
          visits += from.length + to.length;
          if (visits > 2_000_000) return;
          let delta = 0;
          for (const n of from) {
            if (n === b) continue;
            delta += (b.x-n.x)**2 + (b.y-n.y)**2 - (a.x-n.x)**2 - (a.y-n.y)**2;
          }
          for (const n of to) {
            if (n === a) continue;
            delta += (a.x-n.x)**2 + (a.y-n.y)**2 - (b.x-n.x)**2 - (b.y-n.y)**2;
          }
          if (delta < -1e-8) {
            [a.x,b.x] = [b.x,a.x];
            [a.y,b.y] = [b.y,a.y];
          }
        }
      }
    }
  }
}

/** Naming convention only: used consistently by captions and the test filter. */
export function isGraphTestPath(path: string): boolean {
  const parts = path.replace(/\\/g, "/").split("/");
  const stem = (parts.pop() || "").replace(/\.[^.]+$/, "");
  return parts.some(part => /^(?:__)?(?:tests?|specs?)(?:__)?$/i.test(part)) ||
    /(?:^test[_.-]|[._-](?:tests?|specs?)$|(?:Tests?|Specs?)$)/.test(stem);
}

/** The directory a node lives in, or its whole area for a subsystem. */
function nodePathSegments(node: GraphVizNode): string[] {
  const path = node.path?.trim() || node.area?.trim() || nodeFilePath(node.id) || node.id;
  const segments = splitSegments(path);
  const name = node.name || node.id;
  // A symbol lives inside its file: `a/b.rs` + `new` reads `b.rs/new`.
  if (segments.length === 0 || segments[segments.length - 1] !== name) segments.push(name);
  return segments;
}

/**
 * Give every node the shortest path suffix that is unique among nodes of its
 * kind (`src/mod.rs` vs `framework/muda/src`). Computed once over the whole
 * model, so a label that is unique here is unique in any filtered subset.
 * Nodes that no suffix can separate (same path, same name) keep their name.
 */
function assignNodeLabels(nodes: LaidOutNode[]): void {
  const byKind = new Map<string, LaidOutNode[]>();
  for (const node of nodes) {
    const bucket = byKind.get(node.kind ?? "") ?? [];
    bucket.push(node);
    byKind.set(node.kind ?? "", bucket);
  }
  for (const group of byKind.values()) {
    const byId = new Map(group.map(node => [node.id, node]));
    const segments = new Map(group.map(node => [node.id, nodePathSegments(node)]));
    const labels = disambiguateSuffixes(
      group.map(node => node.id),
      id => segments.get(id) ?? [],
      id => byId.get(id)?.name || id,
    );
    for (const node of group) node.label = labels.get(node.id) ?? (node.name || node.id);
  }
}

interface CommunityCaption {
  label: string;
  /** Dominant source directory, whole; empty when the caption is not path-derived. */
  directory: string;
}

function communityLabel(name: string, nodes: LaidOutNode[]): CommunityCaption {
  if (name !== "_" && !/^community[-_]\d+$/.test(name)) return { label: name, directory: "" };
  // Test suites often outnumber the source files in a community. Prefer its
  // source area for the caption, without excluding tests from the graph.
  // Each file gets one vote even in a graph with many symbols per file.
  const paths = [...new Set(nodes.map(node => (node.path || "").replace(/\\/g, "/")).filter(Boolean))];
  const sourcePaths = paths.filter(path => !isGraphTestPath(path));
  const directories = new Map<string, number>();
  const whole = new Map<string, string>();
  for (const path of sourcePaths.length ? sourcePaths : paths) {
    const parts = path.split("/");
    parts.pop();
    const directory = parts.slice(-2).join("/");
    if (directory) {
      directories.set(directory, (directories.get(directory) ?? 0) + 1);
      if (!whole.has(directory)) whole.set(directory, parts.join("/"));
    }
  }
  const top = [...directories].sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]))[0]?.[0];
  if (top) return { label: top, directory: whole.get(top) ?? top };
  return { label: name === "_" ? "Ungrouped" : name.replace(/^community[-_]/, "Group "), directory: "" };
}

/**
 * Chips whose captions collide are widened with path segments until they
 * differ. Two groups that share the same dominant directory cannot be told
 * apart by path, so the next discriminator is their most connected member.
 */
function disambiguateCommunities(communities: GraphCommunity[], directories: ReadonlyMap<string, string>, hubs: ReadonlyMap<string, string>): void {
  const counts = new Map<string, number>();
  for (const group of communities) counts.set(group.label, (counts.get(group.label) ?? 0) + 1);
  const colliding = communities.filter(group => (counts.get(group.label) ?? 0) > 1 && group.label !== group.name);
  if (!colliding.length) return;
  const byName = new Map(colliding.map(group => [group.name, group]));
  const widened = disambiguateSuffixes(
    colliding.map(group => group.name),
    name => splitSegments(directories.get(name) ?? ""),
    name => byName.get(name)?.label ?? name,
  );
  for (const group of colliding) {
    const label = widened.get(group.name);
    if (label && directories.get(group.name)) group.label = label;
    else if (hubs.get(group.name)) group.label += ` · ${hubs.get(group.name)}`;
  }
}

/**
 * The content space a graph is laid out in. Fixed, so the layout depends on
 * the payload alone and a viewport resize never recomputes it.
 */
export const CODE_GRAPH_LAYOUT_WIDTH = 800;
export const CODE_GRAPH_LAYOUT_HEIGHT = 560;

export function buildCodeGraphModel(
  payload: unknown,
  width = CODE_GRAPH_LAYOUT_WIDTH,
  height = CODE_GRAPH_LAYOUT_HEIGHT,
): CodeGraphModel {
  const data = asRecord(payload) ?? {};
  const nodeRows: unknown[] = Array.isArray(data.nodes) ? data.nodes : [];
  const linkRows: unknown[] = Array.isArray(data.links) ? data.links : [];
  const counts = normalizeCounts(data.counts);
  const warnings: string[] = [];
  const coverage = asRecord(data.meta)?.coverage;
  if (coverage != null && !graphCoverageLabel(coverage)) warnings.push("Invalid subsystem coverage metadata; indexed-file coverage is unavailable.");
  if (payload != null && (!Array.isArray(data.nodes) || !Array.isArray(data.links))) {
    warnings.push("Invalid graph payload: nodes and links must be arrays.");
  }
  const nodeIds = new Map<string, GraphVizNode | null>();
  let invalidNodes = 0;
  const scanNodes = Math.min(nodeRows.length, 100_000);
  for (let i = 0; i < scanNodes; i++) {
    const row = asRecord(nodeRows[i]);
    const id = textValue(row?.id);
    if (!row || !id) { invalidNodes++; continue; }
    if (nodeIds.has(id)) {
      invalidNodes += nodeIds.get(id) === null ? 1 : 2;
      nodeIds.set(id, null);
      continue;
    }
    const flags = Array.isArray(row.flags) ? row.flags.slice(0, 50).flatMap((raw: unknown) => {
      const flag = asRecord(raw);
      const name = textValue(flag?.flag);
      return name ? [{ flag: name, confidence: textValue(flag?.confidence) }] : [];
    }) : [];
    nodeIds.set(id, {
      id, name: textValue(row.name) || id, path: textValue(row.path), kind: textValue(row.kind),
      community: textValue(row.community), area: textValue(row.area), language: textValue(row.language), lang: textValue(row.lang),
      degree: Math.max(0, asFinite(row.degree, asFinite(row.val, 1))),
      val: asFinite(row.val), file_count: count(row.file_count), summary: textValue(row.summary),
      line: count(row.line), entry: row.entry === true, flags,
      documentation: row.documentation === true,
      x: typeof row.x === "number" && Number.isFinite(row.x) && Math.abs(row.x) <= 1e7 ? row.x : undefined,
      y: typeof row.y === "number" && Number.isFinite(row.y) && Math.abs(row.y) <= 1e7 ? row.y : undefined,
    });
  }
  const nodesRaw = [...nodeIds.values()].filter((node): node is GraphVizNode => node !== null);
  if (nodesRaw.length > 5000) {
    nodesRaw.sort((a, b) => (b.degree ?? 0) - (a.degree ?? 0) || a.id.localeCompare(b.id));
    nodesRaw.length = 5000;
  }
  if (invalidNodes) warnings.push(`${invalidNodes} invalid or ambiguous node rows omitted.`);
  if (scanNodes < nodeRows.length) warnings.push(`Examined ${scanNodes} of ${nodeRows.length} node rows; input exceeds the processing limit.`);
  const projection = asRecord(asRecord(data.meta)?.projection);
  const omitted = count(projection?.invalid_nodes) + count(projection?.invalid_edges);
  if (omitted) warnings.push(`${omitted} invalid or unresolved graph records omitted by the index projection.`);
  const level = textValue(data.level) ?? null;
  const indexById = new Map(nodesRaw.map((n, i) => [n.id, i]));

  const links: CodeGraphModel["links"] = [];
  const linkKeys = new Set<string>();
  let invalidLinks = 0, duplicateLinks = 0;
  const scanLinks = Math.min(linkRows.length, 500_000);
  for (let i = 0; i < scanLinks; i++) {
    const row = asRecord(linkRows[i]);
    const source = textValue(row?.source), target = textValue(row?.target);
    if (!row || !source || !target || !nodeIds.get(source) || !nodeIds.get(target)) { invalidLinks++; continue; }
    const sourceIndex = indexById.get(source), targetIndex = indexById.get(target);
    if (sourceIndex == null || targetIndex == null) continue;
    const kind = textValue(row.kind);
    const key = JSON.stringify([source, target, kind ?? ""]);
    if (linkKeys.has(key)) { duplicateLinks++; continue; }
    linkKeys.add(key);
    if (links.length >= 50_000) continue;
    const confidence = typeof row.confidence === "number" && Number.isFinite(row.confidence) && row.confidence >= 0 && row.confidence <= 1 ? row.confidence : null;
    links.push({ source, target, sourceIndex, targetIndex, kind, confidence,
      resolution: textValue(row.resolution), label: textValue(row.label), evidence_count: Math.max(1, count(row.evidence_count)),
    });
  }
  if (invalidLinks) warnings.push(`${invalidLinks} invalid links or links with ambiguous endpoints omitted.`);
  if (duplicateLinks) warnings.push(`${duplicateLinks} duplicate links merged.`);
  if (scanLinks < linkRows.length) warnings.push(`Examined ${scanLinks} of ${linkRows.length} link rows; input exceeds the processing limit.`);
  const nodes = layoutNodes(nodesRaw, width, height, {scaleDocExtent:level === "doc", links});
  const nodesTotal = Math.max(nodeRows.length, counts?.nodes_total ?? 0);
  const linksTotal = Math.max(linkRows.length, counts?.links_total ?? 0);
  const effectiveCounts: GraphVizCounts | null = payload == null ? null : {
    nodes_shown: nodes.length, nodes_total: nodesTotal,
    nodes_truncated: Boolean(counts?.nodes_truncated) || nodes.length < nodesTotal,
    links_shown: links.length, links_total: linksTotal,
    max_nodes: nodeRows.length > 5000 ? 5000 : counts?.max_nodes,
  };
  const communityCounts = asRecord(data.communities);

  const grouped = new Map<string, LaidOutNode[]>();
  for (const node of nodes) {
    const name = nodeCommunity(node);
    const members = grouped.get(name) ?? [];
    members.push(node);
    grouped.set(name, members);
  }
  assignNodeLabels(nodes);
  const directories = new Map<string, string>();
  const hubs = new Map<string, string>();
  const communities: GraphCommunity[] = [...grouped].map(([name, members]) => {
    const minX = Math.min(...members.map(n => n.x - n.radius));
    const maxX = Math.max(...members.map(n => n.x + n.radius));
    const minY = Math.min(...members.map(n => n.y - n.radius));
    const maxY = Math.max(...members.map(n => n.y + n.radius));
    const x = (minX + maxX) / 2, y = (minY + maxY) / 2;
    const radius = Math.max(...members.map(n => Math.hypot(n.x - x, n.y - y) + n.radius));
    const hasCoordinates = members.some(n => {
      const raw = nodesRaw[indexById.get(n.id) ?? -1];
      return Number.isFinite(raw?.x) && Number.isFinite(raw?.y);
    });
    const caption = communityLabel(name, members);
    directories.set(name, caption.directory);
    const hub = members.reduce((best, n) => (n.degree ?? 0) > (best.degree ?? 0) || ((n.degree ?? 0) === (best.degree ?? 0) && n.id < best.id) ? n : best);
    hubs.set(name, hub.label);
    return {
      name, label: caption.label, shown: members.length,
      count: Math.max(members.length, count(communityCounts?.[name])),
      colorIndex: members[0].colorIndex,
      bounds: hasCoordinates ? null : { x, y, radius },
    };
  }).sort((a, b) => b.shown - a.shown || a.name.localeCompare(b.name));

  disambiguateCommunities(communities, directories, hubs);

  const gen = typeof data.generation_id === "number" && Number.isFinite(data.generation_id) ? data.generation_id : textValue(data.generation_id);
  return {
    nodes,
    links,
    communities,
    counts: effectiveCounts,
    level,
    warnings,
    generationId: gen == null ? null : String(gen),
  };
}
