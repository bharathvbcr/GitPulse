/**
 * The repo-map page: sidebar (headline, languages, liveness, Navigator) beside
 * the subsystem map.
 *
 * Reads `map_preview::build_preview_payload` through `readPreview`; the map
 * itself is the same `createMapView` the code-graph page uses.
 */

import { defaultLanguageColor, type LanguageColorFor } from "../languages";
import { subsystemNodeId } from "../areaTreeNav";
import { copyToClipboard } from "./clipboard";
import { append, fmt, h, type Child } from "./dom";
import { createMapView } from "./mapView";
import { createNavigator } from "./navigator";
import { capText, readPreview, unplacedText, type LanguageStyle, type PreviewModel } from "./previewModel";

function pctText(n: number, total: number): string {
  if (total <= 0 || n <= 0) return "0%";
  const p = (n * 100) / total;
  return `${p < 1 ? p.toFixed(1) : p.toFixed(p < 10 ? 1 : 0)}%`;
}

function languageBlock(model: PreviewModel, styleFor: (language: string) => LanguageStyle): HTMLElement {
  const total = model.languageTotals.reduce((sum, [, n]) => sum + n, 0);
  const bar = h("div", { class: "langbar", attrs: { role: "img", "aria-label": "Share of indexed files by language" } });
  for (const [lang, n] of model.languageTotals) {
    const seg = h("span", { title: `${styleFor(lang).label}: ${fmt(n)} files (${pctText(n, total)})` });
    seg.style.width = `${(n * 100) / Math.max(1, total)}%`;
    seg.style.background = styleFor(lang).color;
    bar.appendChild(seg);
  }
  const legend = h(
    "ul",
    { class: "langlegend" },
    ...model.languageTotals.slice(0, 14).map(([lang, n]) => {
      const style = styleFor(lang);
      const swatch = h("span", { class: `dot-swatch${style.official ? "" : " unofficial"}`, attrs: { "aria-hidden": "true" } });
      swatch.style.background = style.color;
      return h(
        "li",
        { class: "langchip", title: style.official ? style.label : `${style.label}: no Linguist colour, shown neutral` },
        swatch,
        h("span", { text: style.label }),
        h("span", { class: "pct", text: pctText(n, total) }),
      );
    }),
  );
  const coverage = model.indexedTotal
    ? h(
        "p",
        { class: "note" },
        "Subsystems account for ",
        h("strong", { text: fmt(model.inSubsystems) }),
        " of ",
        h("strong", { text: fmt(model.indexedTotal) }),
        ` indexed files (${pctText(model.inSubsystems, model.indexedTotal)}). The bar covers every indexed file; per-subsystem strips cover only the ${pctText(model.inSubsystems, model.indexedTotal)} inside a subsystem.`,
      )
    : h("p", { class: "note", text: "This map carries no file inventory, so language shares are unknown." });
  return h(
    "section",
    { class: "side-block" },
    h("h2", { text: total ? `Languages · ${fmt(total)} files · GitHub Linguist ${model.linguistVersion}` : "Languages · no indexed files" }),
    bar,
    legend,
    coverage,
  );
}

function livenessBlock(model: PreviewModel): HTMLElement {
  const live = model.liveness;
  const badge = (label: string, text: string, tip: string): HTMLElement =>
    h("span", { class: "badge", title: tip }, `${label} `, h("b", { text }));
  const parts: Child[] = [
    badge("entry roots", capText(live.entryRoots), "Files treated as starting points (main, CLI, routes, tests). Reachability is measured from these."),
    badge("unwired", capText(live.unwiredCandidates), "Files with no resolved inbound reference: nothing in the index imports or calls them."),
    badge("dead symbols", capText(live.deadSymbolCandidates), "Symbols with no resolved inbound caller. Ambiguous names and unresolved receivers weaken the signal."),
    live.unreachableUnreliable
      ? h("span", { class: "badge warn", title: "Reachability from entry roots was not trustworthy for this repository, so the unreachable count is withheld." }, "unreachable unreliable")
      : badge("unreachable", capText(live.unreachableFiles), "Files that no path from an entry root reaches."),
  ];
  return h(
    "section",
    { class: "side-block" },
    h("h2", { text: "Liveness signals" }),
    h("div", { class: "badges" }, ...parts),
    h("p", { class: "note", text: "These are candidates, not verdicts. Runtime entry points, registries, generated code, reflection and outside consumers can all make live code look unused, so check before deleting anything." }),
  );
}

export function mountRepoMapPage(root: HTMLElement, DATA: Record<string, unknown>): void {
  const model = readPreview(DATA);
  const styleFor = (language: string): LanguageStyle =>
    model.palette[language] ?? { color: model.neutralColor, label: language || "unknown", official: false };
  const colorFor: LanguageColorFor = (key, theme) => model.palette[key]?.color ?? defaultLanguageColor(key, theme);

  const announce = h("div", { class: "cm-live", attrs: { role: "status", "aria-live": "polite" } });
  const copyPath = (path: string): void => {
    void copyToClipboard(path).then((ok) => {
      announce.textContent = ok ? `Copied ${path}` : `Could not copy ${path}`;
    });
  };

  const map = createMapView({
    payload: DATA,
    title: "Subsystem map",
    caption: "Repository map",
    colorFor,
    emptyHint: "This repo map has no subsystems to draw.",
    onSelect: (id) => { if (id) navigator.selectSubsystem(id); },
  });
  const navigator = createNavigator({
    model,
    styleFor,
    hasMapNode: (id) => map.hasNode(id),
    onShowOnMap: (id) => { map.focusNode(id); },
    onCopyPath: copyPath,
    onSelect: (id) => { map.focusNode(subsystemNodeId(id)); },
  });

  const unplaced = unplacedText(model.unresolvedTotal);
  const head = model.generatedHead ? `HEAD ${model.generatedHead.slice(0, 10)}` : "working tree (no HEAD recorded)";
  // Beside the repo map the code graph is a sibling file; over HTTP it is a sibling route.
  const graphHref = location.protocol === "http:" || location.protocol === "https:" ? "code_graph.html" : model.graphHtml;
  const stats: Array<[number, string, string]> = [
    [model.nodeCount, "subsystems", "Subsystems drawn on this map: a selected set of areas, not every directory."],
    [model.edgeCount, "edges", "Connections between drawn subsystems: neighbour links and handoff paths, each counted once."],
    [model.indexedTotal, "files", "Source and document files in the DevMap index, including those outside any drawn subsystem."],
    [model.languageTotals.length, "languages", "Distinct languages found across the indexed files."],
  ];

  const side = h(
    "aside",
    { class: "page-side", attrs: { "aria-label": "Repository overview" } },
    h(
      "header",
      { class: "side-head" },
      h("h1", { text: "DevCouncil repo map" }),
      h("p", { class: "cm-muted", text: `${head}${unplaced ? ` · ${unplaced}` : ""}` }),
      h("p", { class: "cm-muted", text: "A static, offline picture of how this repository is organised: its main subsystems, how they connect, which languages they are written in, and where code looks unused. Built from the DevMap index, with no model call." }),
      h("div", { class: "stats" }, ...stats.map(([n, label, tip]) => h("div", { class: "stat", title: tip }, h("b", { text: fmt(n) }), h("span", { text: label })))),
    ),
    h(
      "div",
      { class: "side-scroll" },
      languageBlock(model, styleFor),
      h("section", { class: "side-block" }, h("h2", { text: "Navigator" }), navigator.element),
      livenessBlock(model),
      h(
        "details",
        { class: "about" },
        h("summary", { text: "How to read this map" }),
        h("p", { text: "Each dot is a subsystem: an area of the repository that DevMap picked out as a unit. The map shows a selected set, not every directory, so the coverage note under Languages says how much of the repository falls inside it." }),
        h("p", { text: "Colour is the subsystem's dominant language. Size is roughly how much it connects to. A green ring marks entry points. Arrows run from the depending area to the one it uses; plain lines join neighbours. Capped lists are samples, and the page says so when it is showing one." }),
      ),
    ),
    h("footer", { class: "side-foot" }, h("span", { text: "Select a dot or a row for detail" }), h("a", { attrs: { href: graphHref }, text: "Code graph →" })),
  );
  append(root, [h("div", { class: "page" }, side, h("main", { class: "page-main" }, map.element)), announce]);
}
