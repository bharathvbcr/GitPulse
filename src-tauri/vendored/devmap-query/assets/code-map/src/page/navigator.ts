/**
 * The repo-map Navigator: an area tree with a filter and WAI-ARIA tree keys,
 * and a detail pane for the selected subsystem.
 *
 * Port of GitPulse's `RepoMapPanel` Navigator over the same pure modules
 * (`areaTree`, `areaTreeNav`). The page is read-only, so a file in the detail
 * pane is a "copy path" button where the app opens its editor. Capped samples
 * are labelled as samples: the payload carries no per-role totals, so the tests
 * section says so instead of printing an "N of M" it cannot back.
 */

import { buildAreaTree, summaryWithoutArea, type AreaTreeRow } from "../areaTree";
import { NO_KEYS, neighborRowKey, subsystemNodeId, treeKeyOutcome, visibleAreaRows } from "../areaTreeNav";
import { button, fmt, h, replaceChildren, type Child } from "./dom";
import { testsSampleText, type LanguageStyle, type PreviewModel, type PreviewSubsystem } from "./previewModel";

export interface NavigatorOptions {
  model: PreviewModel;
  styleFor: (language: string) => LanguageStyle;
  /** True when the map has a node for this subsystem id, so "Show on map" can land on it. */
  hasMapNode: (id: string) => boolean;
  onShowOnMap: (id: string) => void;
  onCopyPath: (path: string) => void;
  /** Called after a subsystem row is selected (by id). */
  onSelect?: (id: string) => void;
}

export interface NavigatorHandle {
  element: HTMLElement;
  /** Select the subsystem whose graph node id (area) is `id`, expanding its ancestors. */
  selectSubsystem(id: string): boolean;
}

function pct(n: number, total: number): string {
  if (total <= 0 || n <= 0) return "0%";
  const p = (n * 100) / total;
  return `${p < 1 ? p.toFixed(1) : p.toFixed(p < 10 ? 1 : 0)}%`;
}

function languageStrip(model: PreviewModel, styleFor: NavigatorOptions["styleFor"], pairs: Array<[string, number]>, total: number): HTMLElement {
  const strip = h("div", { class: "nav-strip", attrs: { role: "img", "aria-label": pairs.slice(0, 6).map(([lang, n]) => `${styleFor(lang).label} ${pct(n, total)}`).join(", ") } });
  for (const [lang, n] of pairs) {
    const seg = h("span", { title: `${styleFor(lang).label}: ${fmt(n)} files (${pct(n, total)})` });
    seg.style.width = `${(n * 100) / Math.max(1, total)}%`;
    seg.style.background = styleFor(lang).color || model.neutralColor;
    strip.appendChild(seg);
  }
  return strip;
}

export function createNavigator(options: NavigatorOptions): NavigatorHandle {
  const { model } = options;
  const rows: AreaTreeRow[] = buildAreaTree(model.subsystems);
  let collapsed: ReadonlySet<string> = new Set(rows.filter((r) => r.hasChildren && r.depth >= 1).map((r) => r.key));
  let filter = "";
  let selectedKey: string | null = rows.find((r) => r.subsystemIndex !== null)?.key ?? null;
  let focusKey: string | null = selectedKey ?? rows[0]?.key ?? null;

  const filterInput = h("input", {
    class: "nav-filter",
    attrs: { type: "search", "aria-label": "Filter areas", placeholder: "Filter areas…", autocomplete: "off", spellcheck: "false" },
    on: { input: () => { filter = filterInput.value; renderTree(); } },
  });
  const tree = h("div", { class: "nav-tree", attrs: { role: "tree", "aria-label": "Areas" } });
  const detail = h("div", { class: "nav-detail", attrs: { "aria-live": "polite" } });
  const element = h("div", { class: "nav" }, h("nav", { class: "nav-side", attrs: { "aria-label": "Subsystem areas" } }, filterInput, tree), detail);

  const visible = (): AreaTreeRow[] => visibleAreaRows(rows, collapsed, filter);
  const filtering = (): boolean => filter.trim() !== "";
  const effectiveCollapsed = (): ReadonlySet<string> => (filtering() ? NO_KEYS : collapsed);

  function focusRow(key: string): void {
    focusKey = key;
    renderTree();
    const row = tree.querySelector(`[data-row-key="${CSS.escape(key)}"]`);
    if (row instanceof HTMLElement) row.focus();
  }

  function selectKey(key: string, notify = true): void {
    selectedKey = key;
    const row = rows.find((r) => r.key === key);
    focusKey = key;
    renderTree();
    renderDetail();
    if (notify && row && row.subsystemIndex !== null) options.onSelect?.(model.subsystems[row.subsystemIndex].id);
  }

  function toggle(key: string): void {
    if (filtering()) return;
    const next = new Set(collapsed);
    if (!next.delete(key)) next.add(key);
    collapsed = next;
    renderTree();
  }

  function renderTree(): void {
    const shown = visible();
    const open = effectiveCollapsed();
    const stop = shown.some((r) => r.key === focusKey) ? focusKey : shown[0]?.key ?? null;
    const items: Child[] = shown.map((row) => {
      const isOpen = row.hasChildren && !open.has(row.key);
      const sub = row.subsystemIndex === null ? null : model.subsystems[row.subsystemIndex];
      const selected = row.key === selectedKey;
      const item = h(
        "div",
        {
          class: `nav-row${selected ? " is-on" : ""}`,
          title: row.area,
          attrs: {
            role: "treeitem",
            "data-row-key": row.key,
            tabindex: row.key === stop ? 0 : -1,
            "aria-level": row.depth + 1,
            "aria-expanded": row.hasChildren ? String(isOpen) : undefined,
            "aria-selected": sub ? String(selected) : undefined,
          },
          on: {
            click: () => {
              focusKey = row.key;
              if (row.subsystemIndex === null) toggle(row.key);
              else selectKey(row.key);
            },
            keydown: (e) => {
              if (e.target !== e.currentTarget || e.altKey || e.ctrlKey || e.metaKey) return;
              const outcome = treeKeyOutcome(e.key, shown, row.key, open);
              if (!outcome) return;
              e.preventDefault();
              if (outcome.collapsed && !filtering()) collapsed = outcome.collapsed;
              if (outcome.select) selectKey(outcome.select);
              else focusRow(outcome.focusKey);
              if (outcome.select) focusRow(outcome.focusKey);
            },
          },
        },
        h("span", { class: "nav-chevron", attrs: { "aria-hidden": "true" }, text: row.hasChildren ? (isOpen ? "▾" : "▸") : "" }),
        h(
          "span",
          { class: "nav-row-copy" },
          h("span", { class: "nav-row-line" }, h("span", { class: "nav-row-label", text: row.label }), row.duplicateCount > 1 ? h("span", { class: "nav-badge", title: `${row.duplicateCount} subsystems share this area`, text: `×${row.duplicateCount}` }) : null),
          sub && sub.summary ? h("span", { class: "nav-row-summary", text: summaryWithoutArea(sub), title: sub.summary }) : null,
        ),
      );
      item.style.paddingInlineStart = `${0.5 + row.depth * 0.9}rem`;
      return item;
    });
    if (items.length === 0) items.push(h("p", { class: "nav-empty", attrs: { role: "status" }, text: `No areas match "${filter.trim()}".` }));
    replaceChildren(tree, ...items);
  }

  function pathList(paths: readonly string[]): HTMLElement {
    return h("ul", { class: "nav-list" }, ...paths.map((path) => h("li", {}, button(path, () => options.onCopyPath(path), { class: "nav-path", title: `Copy ${path}` }))));
  }

  function section(title: string, ...body: Child[]): HTMLElement {
    const id = `nav-sec-${title.toLowerCase().replace(/[^a-z]+/g, "-")}`;
    return h("section", { attrs: { "aria-labelledby": id } }, h("h3", { class: "nav-h", text: title, attrs: { id } }), ...body);
  }

  function renderDetail(): void {
    const row = rows.find((r) => r.key === selectedKey);
    const sub: PreviewSubsystem | null = row && row.subsystemIndex !== null ? model.subsystems[row.subsystemIndex] : null;
    if (!sub || !row) {
      replaceChildren(detail, h("p", { class: "cm-muted", text: "Select an area to see its entry points, critical files, tests and neighbors." }));
      return;
    }
    const summary = summaryWithoutArea(sub);
    const tests = sub.roleFiles.tests ?? [];
    const otherRoles = Object.keys(sub.roleFiles).filter((role) => role !== "tests" && (sub.roleFiles[role]?.length ?? 0) > 0);
    const placed = options.hasMapNode(subsystemNodeId(sub.area));
    const unplaced = model.unresolvedBySubsystem[sub.id];
    replaceChildren(
      detail,
      h(
        "header",
        { class: "nav-detail-head" },
        h("div", { class: "nav-detail-title" }, h("h2", { text: sub.area }), summary ? h("p", { class: "cm-muted", text: summary }) : null, row.duplicateCount > 1 ? h("p", { class: "cm-muted", text: `${row.duplicateCount} subsystems share this area; showing the first.` }) : null),
        placed ? button("Show on map", () => options.onShowOnMap(subsystemNodeId(sub.area)), { class: "cm-btn", title: "Select this subsystem on the map" }) : null,
      ),
      sub.fileCount > 0
        ? h("div", {}, languageStrip(model, options.styleFor, sub.languages, sub.fileCount), h("p", { class: "cm-muted", text: `${sub.languages.slice(0, 6).map(([lang, n]) => `${options.styleFor(lang).label} ${pct(n, sub.fileCount)}`).join(" · ")} · ${fmt(sub.fileCount)} file${sub.fileCount === 1 ? "" : "s"}` }))
        : h("p", { class: "cm-muted", text: "No indexed files attributed to this subsystem." }),
      section("Entry points", sub.entryPoints.length ? pathList(sub.entryPoints) : h("p", { class: "cm-muted", text: "None listed." })),
      section("Critical files", sub.criticalFiles.length ? pathList(sub.criticalFiles) : h("p", { class: "cm-muted", text: "None listed." })),
      section(
        "Tests",
        tests.length
          ? h("div", {}, pathList(tests), h("p", { class: "cm-muted", text: testsSampleText(tests.length) }))
          : h("p", { class: "cm-muted", text: model.roleFilesComputed ? testsSampleText(0) : "Role files were not computed for this map." }),
      ),
      sub.neighbors.length
        ? section(
            "Neighbors",
            h(
              "ul",
              { class: "nav-chips" },
              ...sub.neighbors.map((neighbor) => {
                const key = neighborRowKey(rows, neighbor);
                return h("li", {}, key ? button(neighbor, () => selectKey(key), { class: "nav-path", title: `Select ${neighbor} in the tree` }) : h("span", { class: "nav-chip-off", title: `${neighbor} is not in this map's subsystem list`, text: neighbor }));
              }),
            ),
          )
        : null,
      section(
        "Handoffs",
        sub.handoffPaths.length
          ? h("ul", { class: "nav-plain" }, ...sub.handoffPaths.slice(0, 8).map((text) => h("li", { text })), sub.handoffPaths.length > 8 ? h("li", { class: "cm-muted", text: `… ${sub.handoffPaths.length - 8} more` }) : null)
          : h("p", { class: "cm-muted", text: model.handoffPathsComputed ? "None." : "Not computed for this map." }),
      ),
      otherRoles.length
        ? section("Roles", h("ul", { class: "nav-plain" }, ...otherRoles.map((role) => h("li", {}, h("span", { class: "cm-muted", text: `${role}: ` }), (sub.roleFiles[role] ?? []).slice(0, 6).join(", ")))))
        : null,
      unplaced && unplaced.total
        ? section(
            `Unplaced handoffs${unplaced.listed < unplaced.total ? ` (${fmt(unplaced.listed)} of ${fmt(unplaced.total)} listed)` : ""}`,
            h("ul", { class: "nav-plain" }, ...model.unresolved.filter((u) => u.from === sub.id).slice(0, 8).map((u) => h("li", { text: u.text }))),
            h("p", { class: "cm-muted", text: "These handoffs name a target that no drawn subsystem contains, so they have no edge on the map." }),
          )
        : null,
    );
  }

  renderTree();
  renderDetail();

  return {
    element,
    selectSubsystem(id) {
      const target = `area:${subsystemNodeId(id)}`;
      const row = rows.find((r) => r.subsystemIndex !== null && (r.key === target || model.subsystems[r.subsystemIndex].id === id));
      if (!row) return false;
      const open = new Set(collapsed);
      for (let parent = row.parentKey; parent !== null; parent = rows.find((r) => r.key === parent)?.parentKey ?? null) open.delete(parent);
      collapsed = open;
      filter = "";
      filterInput.value = "";
      selectKey(row.key, false);
      return true;
    },
  };
}
