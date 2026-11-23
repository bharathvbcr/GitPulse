import { describe, expect, it } from "vitest";
import { get } from "svelte/store";
import { VIEW_PANE_ID } from "../views/viewRegistry";
import { classifySurface, createLastSurface } from "./lastSurface";

/** A node tree without a DOM, as `panelDismiss.test.ts` builds one. */
interface FakeNode {
  tokens: string[];
  parent: FakeNode | null;
  closest(selector: string): FakeNode | null;
}

function node(tokens: string[], parent: FakeNode | null = null): FakeNode {
  const self: FakeNode = {
    tokens,
    parent,
    closest(selector) {
      const wanted = selector.split(",").map((part) => part.trim());
      for (let at: FakeNode | null = self; at; at = at.parent) {
        if (at.tokens.some((token) => wanted.includes(token))) return at;
      }
      return null;
    },
  };
  return self;
}

const body = node(["body"]);
const viewBar = node(["[data-viewbar]"], body);
const viewPane = node([`#${VIEW_PANE_ID}`], body);
const pageContent = node(["div"], node([".gp-view"], viewPane));
// The dock is inside the view pane's element, as in App.svelte.
const dock = node(["[data-terminal-dock]"], viewPane);
const terminalInput = node(["textarea"], node(["div"], dock));
const sidebar = node(["button"], node(["[data-sidebar-panel]"], body));
const portaledMenu = node([".gp-menu-item"], node([".gp-menu"], body));

describe("classifySurface", () => {
  it("names the terminal for anything inside the dock, even though the dock sits in the view pane", () => {
    expect(classifySurface(terminalInput)).toBe("terminal");
    expect(classifySurface(dock)).toBe("terminal");
  });

  it("names the page for the view bar and the view above the dock", () => {
    expect(classifySurface(viewBar)).toBe("page");
    expect(classifySurface(pageContent)).toBe("page");
  });

  it("is null for the sidebar, a portaled menu, the body and a missing target", () => {
    expect(classifySurface(sidebar)).toBeNull();
    expect(classifySurface(portaledMenu)).toBeNull();
    expect(classifySurface(body)).toBeNull();
    expect(classifySurface(null)).toBeNull();
    expect(classifySurface({})).toBeNull();
  });

  it("walks from a text node to its element", () => {
    expect(classifySurface({ parentElement: terminalInput })).toBe("terminal");
    expect(classifySurface({ parentElement: pageContent })).toBe("page");
  });
});

describe("createLastSurface", () => {
  function fakeHost() {
    const listeners = new Map<string, Set<(event: Event) => void>>();
    return {
      listeners,
      addEventListener(type: string, handler: (event: Event) => void, capture?: boolean) {
        expect(capture).toBe(true);
        if (!listeners.has(type)) listeners.set(type, new Set());
        listeners.get(type)!.add(handler);
      },
      removeEventListener(type: string, handler: (event: Event) => void, capture?: boolean) {
        expect(capture).toBe(true);
        listeners.get(type)?.delete(handler);
      },
      fire(type: string, target: unknown) {
        for (const handler of [...(listeners.get(type) ?? [])]) handler({ target } as unknown as Event);
      },
      count: () => [...listeners.values()].reduce((sum, set) => sum + set.size, 0),
    };
  }

  it("is null until a surface is used, then follows pointer presses and key releases", () => {
    const host = fakeHost();
    const surface = createLastSurface(host as unknown as Document);
    const stop = surface.subscribe(() => {});
    expect(get(surface)).toBeNull();
    host.fire("pointerdown", terminalInput);
    expect(get(surface)).toBe("terminal");
    // Tab out of the terminal: the key's release lands on the page element it focused.
    host.fire("keyup", pageContent);
    expect(get(surface)).toBe("page");
    host.fire("keyup", terminalInput);
    expect(get(surface)).toBe("terminal");
    stop();
  });

  it("focus moved by code is not the user arriving: a dock restored open at launch does not count", () => {
    const host = fakeHost();
    const surface = createLastSurface(host as unknown as Document);
    const stop = surface.subscribe(() => {});
    // The panel focuses its terminal on mount; no person did anything.
    host.fire("focusin", terminalInput);
    expect(get(surface)).toBeNull();
    host.fire("pointerdown", pageContent);
    host.fire("focusin", terminalInput);
    expect(get(surface)).toBe("page");
    stop();
  });

  it("a click in the sidebar never changes it", () => {
    const host = fakeHost();
    const surface = createLastSurface(host as unknown as Document);
    const stop = surface.subscribe(() => {});
    host.fire("pointerdown", terminalInput);
    host.fire("pointerdown", sidebar);
    host.fire("keyup", sidebar);
    host.fire("pointerdown", portaledMenu);
    expect(get(surface)).toBe("terminal");
    stop();
  });

  it("an interaction before a late subscriber arrives is still there when it does", () => {
    const host = fakeHost();
    const surface = createLastSurface(host as unknown as Document);
    const keepAlive = surface.subscribe(() => {});
    host.fire("keyup", terminalInput);
    // The lazily mounted sidebar explorer subscribes only now.
    expect(get(surface)).toBe("terminal");
    keepAlive();
  });

  it("listens only while subscribed and removes exactly what it added", () => {
    const host = fakeHost();
    const surface = createLastSurface(host as unknown as Document);
    expect(host.count()).toBe(0);
    const stopA = surface.subscribe(() => {});
    const stopB = surface.subscribe(() => {});
    expect(host.count()).toBe(2);
    stopA();
    expect(host.count()).toBe(2);
    stopB();
    expect(host.count()).toBe(0);
  });

  it("without a document it stays null and installs nothing", () => {
    const surface = createLastSurface(undefined);
    const stop = surface.subscribe(() => {});
    expect(get(surface)).toBeNull();
    stop();
  });
});
