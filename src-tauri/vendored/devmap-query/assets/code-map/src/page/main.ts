/**
 * Entry point of the standalone code-map pages.
 *
 * `devmap-query` emits one HTML shell per page (`viz::render_html`,
 * `map_preview::render_map_preview_html`): a `<div id="app">`, the payload as
 * escaped JSON in `<script type="application/json" id="devmap-payload">`, and
 * this bundle. The payload says which page it is: a `view` block is the code
 * graph, a `subsystems` list is the repo map.
 */

import { asRecord } from "../record";
import { h, replaceChildren } from "./dom";
import { mountCodeGraphPage } from "./codeGraphPage";
import { mountRepoMapPage } from "./repoMapPage";
import { PAGE_CSS } from "./styles";

export const PAYLOAD_ELEMENT_ID = "devmap-payload";
/** Stamped on <html> so a test can tell this renderer from any other page. */
export const PAGE_RENDERER = "devmap-code-map";

function showError(root: HTMLElement, title: string, detail: string): void {
  replaceChildren(root, h("div", { class: "page-error", attrs: { role: "alert" } }, h("strong", { text: title }), h("span", { text: detail })));
}

export function mountPage(doc: Document): void {
  doc.documentElement.setAttribute("data-renderer", PAGE_RENDERER);
  const style = doc.createElement("style");
  style.textContent = PAGE_CSS;
  doc.head.appendChild(style);
  const root = doc.getElementById("app");
  if (!root) return;
  const carrier = doc.getElementById(PAYLOAD_ELEMENT_ID);
  let DATA: Record<string, unknown> | null = null;
  try {
    DATA = asRecord(JSON.parse(carrier?.textContent ?? "null"));
  } catch {
    DATA = null;
  }
  if (!DATA) {
    showError(root, "This page has no readable payload", "The embedded map data is missing or is not valid JSON, so the artifact was probably truncated. Regenerate it.");
    return;
  }
  if (asRecord(DATA.view)) mountCodeGraphPage(root, DATA);
  else mountRepoMapPage(root, DATA);
}
