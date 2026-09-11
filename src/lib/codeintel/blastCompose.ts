/**
 * Compose `impact_layered` answers over a changed-file set.
 *
 * Each seed is queried separately (`getImpactLayeredMany`); this merges the
 * hop bands so the UI can show node_count and nodes_omitted — not only the
 * 50-node sample — plus unmatched seeds and walk_incomplete honesty. Per-seed
 * walk_incomplete essays are folded (numeric ranges, one copy of corpus
 * coverage) so a 179-file change-set cannot dump 179 copies of the same
 * disclaimer into the diff pane.
 */

import type { CodeintelLayeredImpact } from "./types";
import { summarizeWalkIncomplete } from "./walkIncomplete";

/** Kernel / IPC reason when a walk is tripped by cancel or the 30s backstop. */
export const QUERY_CANCELLED_REASON = "query cancelled";

export function isCancelledReason(reason: string | null | undefined): boolean {
  return (reason ?? "").trim().toLowerCase() === QUERY_CANCELLED_REASON;
}

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
  /**
   * Seeds the walk never ran (cancel / deadline). Not the same as unmatched
   * (no indexed start) — those remaining hops are a partial answer.
   */
  cancelled_seeds: number;
}

function pushWalkIncomplete(
  parts: Array<string | null | undefined>,
  value: string | null | undefined,
): void {
  if (!value || !value.trim()) return;
  const last = parts[parts.length - 1];
  if (last === value) return;
  parts.push(value);
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
  let cancelled_seeds = 0;
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

    if (!result.available) {
      const rawReason = result.reason ?? "layered impact unavailable";
      const reason = summarizeWalkIncomplete([rawReason]) ?? rawReason;
      unavailable_seeds.push({ seed: seedHint, reason });
      reasons.push(rawReason);
      if (isCancelledReason(rawReason)) {
        cancelled_seeds += 1;
      } else {
        for (const u of result.blast_radius.unmatched_targets) unmatched.add(u);
      }
      pushWalkIncomplete(walkParts, result.blast_radius.layers.walk_incomplete);
      pushWalkIncomplete(walkParts, result.edges.walk_incomplete);
      continue;
    }

    for (const u of result.blast_radius.unmatched_targets) unmatched.add(u);

    anyAvailable = true;
    total_impacted = saturateAdd(
      total_impacted,
      finiteCount(result.blast_radius.total_impacted),
    );
    const layers = result.blast_radius.layers;
    layers_truncated = layers_truncated || layers.truncated;
    pushWalkIncomplete(walkParts, layers.walk_incomplete);
    pushWalkIncomplete(walkParts, result.edges.walk_incomplete);

    for (const layer of layers.items) {
      const prev = byDepth.get(layer.depth);
      const nodeCount = finiteCount(layer.node_count);
      const omitted = finiteCount(layer.nodes_omitted);
      const confidence = finiteConfidence(layer.lowest_confidence);
      if (!prev) {
        byDepth.set(layer.depth, {
          depth: layer.depth,
          nodes: [...layer.nodes],
          node_count: nodeCount,
          nodes_omitted: omitted,
          lowest_confidence: confidence,
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
      prev.node_count = saturateAdd(prev.node_count, nodeCount);
      prev.nodes_omitted = saturateAdd(prev.nodes_omitted, omitted);
      if (
        confidence != null &&
        (prev.lowest_confidence == null || confidence < prev.lowest_confidence)
      ) {
        prev.lowest_confidence = confidence;
      }
    }
  }

  const layers = [...byDepth.values()].sort((a, b) => a.depth - b.depth);

  return {
    available: anyAvailable,
    reason: anyAvailable
      ? null
      : summarizeWalkIncomplete(reasons) ??
        (results.length === 0 ? "no targets" : "layered impact unavailable for every seed"),
    seeds: [...new Set(seeds)],
    unmatched_targets: [...unmatched],
    total_impacted,
    overlap_possible: results.filter((r) => r.available).length > 1,
    layers,
    layers_truncated,
    walk_incomplete: summarizeWalkIncomplete(walkParts),
    unavailable_seeds,
    cancelled_seeds,
  };
}

function finiteCount(n: unknown): number {
  return typeof n === "number" && Number.isFinite(n) && n > 0 ? n : 0;
}

function finiteConfidence(n: unknown): number | null {
  return typeof n === "number" && Number.isFinite(n) ? n : null;
}

function saturateAdd(a: number, b: number): number {
  const next = a + b;
  if (!Number.isFinite(next) || next < 0) return a;
  return Math.min(Number.MAX_SAFE_INTEGER, next);
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
    cancelled_seeds: 0,
  };
}
