import { describe, expect, it } from "vitest";
import { EXPLORER_PAGE_GESTURE_EXCLUDED, opensExplorerPage } from "./explorerPageGesture";

/**
 * A small element model with a real `closest`: enough of the selector grammar
 * (`tag`, `[attr]`, `[attr="value"]`, `tag[attr="value"]`, comma lists) to run
 * the exclusion list the module actually ships, against the markup the docked
 * explorers actually render. Vitest runs these files without a DOM.
 */
interface FakeElement {
  tag: string;
  attrs: Record<string, string>;
  parent: FakeElement | null;
  closest(selector: string): FakeElement | null;
}

function simpleMatches(el: FakeElement, simple: string): boolean {
  const parsed = /^([a-z][\w-]*)?((?:\[[^\]]+\])*)$/.exec(simple.trim());
  if (!parsed) throw new Error(`selector outside the test grammar: ${simple}`);
  const [, tag, attrPart] = parsed;
  if (tag && el.tag !== tag) return false;
  for (const attr of attrPart.matchAll(/\[([\w-]+)(?:="([^"]*)")?\]/g)) {
    const [, name, value] = attr;
    if (!(name in el.attrs)) return false;
    if (value !== undefined && el.attrs[name] !== value) return false;
  }
  return true;
}

function el(tag: string, attrs: Record<string, string> = {}, parent: FakeElement | null = null): FakeElement {
  const node: FakeElement = {
    tag,
    attrs,
    parent,
    closest(selector) {
      const parts = selector.split(",");
      for (let at: FakeElement | null = node; at; at = at.parent) {
        if (parts.some((part) => simpleMatches(at as FakeElement, part))) return at;
      }
      return null;
    },
  };
  return node;
}

const dblclick = (target: unknown, extra: Partial<{ defaultPrevented: boolean; button: number }> = {}) => ({
  target,
  defaultPrevented: false,
  button: 0,
  ...extra,
});

// The docked explorer as it renders: the pane, its title strip, the tree
// viewport (role="tree") and a file row (role="treeitem") inside it.
const pane = el("div", { "data-testid": "sidebar-explorer-pane" });
const titleStrip = el("div", {}, pane);
const titleText = el("span", {}, titleStrip);
const tree = el("div", { role: "tree", tabindex: "0" }, pane);
const treeSpace = el("div", {}, tree);
const row = el("div", { role: "treeitem", draggable: "true", tabindex: "-1" }, treeSpace);
const rowName = el("span", {}, row);

describe("opensExplorerPage", () => {
  it("opens the page from empty space in the tree and from the pane's own chrome", () => {
    expect(opensExplorerPage(dblclick(treeSpace))).toBe(true);
    expect(opensExplorerPage(dblclick(tree))).toBe(true);
    expect(opensExplorerPage(dblclick(titleStrip))).toBe(true);
    expect(opensExplorerPage(dblclick(titleText))).toBe(true);
    expect(opensExplorerPage(dblclick(pane))).toBe(true);
  });

  it("leaves a row's double-click to the row: it keeps the file open where its click sent it", () => {
    expect(opensExplorerPage(dblclick(row))).toBe(false);
    expect(opensExplorerPage(dblclick(rowName))).toBe(false);
  });

  it("leaves controls, fields, links, menus and the resize sash alone", () => {
    const targets = [
      el("button", {}, titleStrip),
      el("svg", {}, el("button", {}, titleStrip)),
      el("input", { type: "text" }, titleStrip),
      el("textarea", {}, pane),
      el("select", {}, pane),
      el("label", {}, pane),
      el("a", { href: "#x" }, pane),
      el("div", { contenteditable: "true" }, pane),
      el("div", { role: "menuitem" }, el("div", { role: "menu" }, pane)),
      el("div", { role: "option" }, pane),
      el("div", { role: "separator" }, pane),
      el("div", { role: "button" }, pane),
      el("div", { role: "tab" }, pane),
      el("div", { role: "dialog" }, pane),
      el("div", { draggable: "true" }, pane),
    ];
    for (const target of targets) {
      expect(opensExplorerPage(dblclick(target)), `${target.tag} ${JSON.stringify(target.attrs)}`).toBe(false);
    }
  });

  it("does nothing for a handled event, a non-primary button, or a target that is not an element", () => {
    expect(opensExplorerPage(dblclick(treeSpace, { defaultPrevented: true }))).toBe(false);
    expect(opensExplorerPage(dblclick(treeSpace, { button: 2 }))).toBe(false);
    expect(opensExplorerPage(dblclick(null))).toBe(false);
    expect(opensExplorerPage(dblclick({ nodeType: 3 }))).toBe(false);
  });

  it("ships an exclusion list the browser can parse as one selector list", () => {
    // Every entry is a simple compound selector: no :not(), which an older
    // WebKit rejected inside closest() and would throw on every double-click.
    for (const part of EXPLORER_PAGE_GESTURE_EXCLUDED.split(",")) {
      expect(part.trim()).toMatch(/^([a-z][\w-]*)?(\[[\w-]+(="[^"]*")?\])*$/);
    }
  });
});
