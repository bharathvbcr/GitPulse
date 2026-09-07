/**
 * Compose `impact_layered` answers over a changed-file set.
 *
 * Each seed is queried separately (`getImpactLayeredMany`); this merges the
 * hop bands so the UI can show node_count and nodes_omitted — not only the
 * 50-node sample — plus unmatched seeds and walk_incomplete honesty.
 */

import type { CodeintelLayeredImpact } from "./types";

export interface ComposedBlastLayer {
  depth: number;
  /** Sample nodes (capped); never treat as the full band. */
  nodes: string[];
  node_count: number;
  nodes_omitted: number;
  lowest_confidence: number | null;
}

export interface ComposedBlastRadius {
  available: boolean;
  reason: string | null;
  seeds: string[];
  unmatched_targets: string[];
  total_impacted: number;
  /** True when totals may double-count symbols reached from multiple seeds. */
  overlap_possible: boolean;
  layers: ComposedBlastLayer[];
  layers_truncated: boolean;
  walk_incomplete: string | null;
  /** Per-seed refusals that did not contribute a usable radius. */
  unavailable_seeds: Array<{ seed: string; reason: string }>;
}

function mergeWalkIncomplete(parts: Array<string | null | undefined>): string | null {
  const uniq = [...new Set(parts.filter((p): p is string => Boolean(p && p.trim())))];
  return uniq.length === 0 ? null : uniq.join(" · ");
}

/** Merge layered impact results from a changed-file (or symbol) seed set. */
export function composeLayeredImpacts(
  results: CodeintelLayeredImpact[],
  seedLabels?: string[],
): ComposedBlastRadius {
  const seeds =
    seedLabels && seedLabels.length > 0
      ? [...seedLabels]
      : results.flatMap((r) => r.blast_radius.seeds);

  const unmatched = new Set<string>();
  const unavailable_seeds: Array<{ seed: string; reason: string }> = [];
  const byDepth = new Map<number, ComposedBlastLayer>();
  let total_impacted = 0;
  let layers_truncated = false;
  const walkParts: Array<string | null | undefined> = [];
  let anyAvailable = false;
  const reasons: string[] = [];

  for (let index = 0; index < results.length; index++) {
    const result = results[index]!;
    const seedHint =
      result.blast_radius.seeds[0] ??
      seedLabels?.[index] ??
      `seed[${index}]`;

    for (const u of result.blast_radius.unmatched_targets) unmatched.add(u);

    if (!result.available) {
      unavailable_seeds.push({
        seed: seedHint,
        reason: result.reason ?? "layered impact unavailable",
      });
      if (result.reason) reasons.push(result.reason);
      walkParts.push(result.blast_radius.layers.walk_incomplete);
      walkParts.push(result.edges.walk_incomplete);
      continue;
    }

    anyAvailable = true;
    total_impacted += result.blast_radius.total_impacted;
    const layers = result.blast_radius.layers;
    layers_truncated = layers_truncated || layers.truncated;
    walkParts.push(layers.walk_incomplete);
    walkParts.push(result.edges.walk_incomplete);

    for (const layer of layers.items) {
      const prev = byDepth.get(layer.depth);
      if (!prev) {
        byDepth.set(layer.depth, {
          depth: layer.depth,
          nodes: [...layer.nodes],
          node_count: layer.node_count,
          nodes_omitted: layer.nodes_omitted,
          lowest_confidence: layer.lowest_confidence ?? null,
        });
        continue;
      }
      const seen = new Set(prev.nodes);
      for (const n of layer.nodes) {
        if (!seen.has(n) && prev.nodes.length < 50) {
          prev.nodes.push(n);
          seen.add(n);
        }
      }
      prev.node_count += layer.node_count;
      prev.nodes_omitted += layer.nodes_omitted;
      if (
        layer.lowest_confidence != null &&
        (prev.lowest_confidence == null ||
          layer.lowest_confidence < prev.lowest_confidence)
      ) {
        prev.lowest_confidence = layer.lowest_confidence;
      }
    }
  }

  const layers = [...byDepth.values()].sort((a, b) => a.depth - b.depth);

  return {
    available: anyAvailable,
    reason: anyAvailable
      ? null
      : reasons[0] ??
        (results.length === 0 ? "no targets" : "layered impact unavailable for every seed"),
    seeds: [...new Set(seeds)],
    unmatched_targets: [...unmatched],
    total_impacted,
    overlap_possible: results.filter((r) => r.available).length > 1,
    layers,
    layers_truncated,
    walk_incomplete: mergeWalkIncomplete(walkParts),
    unavailable_seeds,
  };
}

/** Placeholder empty blast for idle UI. */
export function emptyComposedBlast(reason = "no changed files"): ComposedBlastRadius {
  return {
    available: false,
    reason,
    seeds: [],
    unmatched_targets: [],
    total_impacted: 0,
    overlap_possible: false,
    layers: [],
    layers_truncated: false,
    walk_incomplete: null,
    unavailable_seeds: [],
  };
}
