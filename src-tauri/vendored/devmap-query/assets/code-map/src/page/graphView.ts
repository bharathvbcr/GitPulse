/**
 * The code-graph page's reading of `viz::render_html`'s `view` block.
 *
 * Pure: no DOM. `DATA.view` is page chrome the renderer attaches to the
 * payload (title, level, which node fields the inspector lists, which flag
 * filters to offer); `build_payload` itself carries none of it.
 */

import { asRecord } from "../record";
import { readUnresolvedHandoffs, unplacedText } from "./previewModel";

export interface GraphView {
  title: string;
  level: string;
  suffix: string;
  /** What to do about a capped view ("raise --max-nodes"); empty when nothing was cut. */
  capHint: string;
  /** Note summarising unplaced handoffs (e.g. "150 handoffs unplaced (100 of 150 listed)"); empty when none. */
  unplacedNote: string;
  unresolvedHandoffs: Array<{ from: string; text: string }>;
  unresolvedTotal: number;
  unresolvedBySubsystem: Record<string, { total: number; listed: number }>;
  /** `[label, node key]` pairs the inspector lists for the selected node. */
  detailFields: Array<[string, string]>;
  /** `[flag, label]` pairs offered as "only nodes carrying this flag" filters. */
  flagFilters: Array<[string, string]>;
}

function labelled(value: unknown): Array<[string, string]> {
  const out: Array<[string, string]> = [];
  if (!Array.isArray(value)) return out;
  for (const row of value) {
    if (Array.isArray(row) && typeof row[0] === "string" && typeof row[1] === "string") out.push([row[0], row[1]]);
  }
  return out;
}

export function readView(DATA: Record<string, unknown>): GraphView {
  const VIEW = asRecord(DATA.view) ?? {};
  const handoffs = readUnresolvedHandoffs(
    DATA.unresolved_handoffs,
    DATA.unresolved_handoffs_total,
    DATA.unresolved_handoffs_by_subsystem,
  );
  const viewUnplaced = typeof VIEW.unplaced_note === "string" ? VIEW.unplaced_note : "";
  return {
    title: typeof VIEW.title === "string" && VIEW.title ? VIEW.title : "devmap",
    level: typeof VIEW.level === "string" ? VIEW.level : "",
    suffix: typeof VIEW.suffix === "string" && VIEW.suffix ? VIEW.suffix : "code graph",
    capHint: typeof VIEW.cap_hint === "string" ? VIEW.cap_hint : "",
    unplacedNote: viewUnplaced || unplacedText(handoffs.unresolvedTotal, handoffs.unresolved.length),
    unresolvedHandoffs: handoffs.unresolved,
    unresolvedTotal: handoffs.unresolvedTotal,
    unresolvedBySubsystem: handoffs.unresolvedBySubsystem,
    detailFields: labelled(VIEW.detail_fields),
    flagFilters: labelled(VIEW.flag_filters),
  };
}
