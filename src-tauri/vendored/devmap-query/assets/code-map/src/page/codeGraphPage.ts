/**
 * The code-graph page: the file or symbol graph, full width.
 *
 * Reads `viz::build_payload` plus the `view` chrome `render_html` attaches.
 * The inspector lists the node fields `view.detail_fields` names, which include
 * the liveness verdict and its reason; those are not part of the layout model,
 * so they are read back off the raw rows by node id.
 */

import { asRecord } from "../record";
import { append, h } from "./dom";
import { readView } from "./graphView";
import { createMapView } from "./mapView";

export function mountCodeGraphPage(root: HTMLElement, DATA: Record<string, unknown>): void {
  const view = readView(DATA);
  const rows = new Map<string, Record<string, unknown>>();
  if (Array.isArray(DATA.nodes)) {
    for (const raw of DATA.nodes) {
      const row = asRecord(raw);
      if (row && typeof row.id === "string") rows.set(row.id, row);
    }
  }
  const map = createMapView({
    payload: DATA,
    title: `${view.title} — ${view.suffix}`,
    caption: view.level ? `Code graph · ${view.level}` : "Code graph",
    flagFilters: view.flagFilters,
    capHint: view.capHint,
    ringLegend: {
      rings: [["--cm-dead", "dead candidate"], ["--cm-unwired", "unwired"], ["--cm-entry", "entry point"]],
      // An isolated config or README has no ring and no edges, and beside a legend
      // that names "dead" and "unwired" a lone dot reads as one of them.
      note: "No ring: reached, or data that is not a liveness candidate. Select a node for its liveness verdict and why.",
    },
    emptyHint: "This generation has no nodes at this level to draw. Build the index first, or switch level.",
    detailFor: (node) => {
      const row = rows.get(node.id);
      if (!row) return [];
      return view.detailFields.flatMap(([label, key]): Array<[string, string]> => {
        const value = row[key];
        if (value === undefined || value === null || value === "") return [];
        // The inspector title already shows the path; the line number is what it lacks.
        if (key === "path") return typeof row.line === "number" && row.line > 0 ? [["Line", String(row.line)]] : [];
        return [[label, String(value)]];
      });
    },
  });
  append(root, [h("div", { class: "page" }, h("main", { class: "page-main" }, map.element))]);
}
