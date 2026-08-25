import { afterEach, describe, expect, it } from "vitest";
import { portal, resolvePortalTarget } from "./portal";

class FakeNode {
  parentNode: FakeNode | null;
  parentElement: FakeNode | null;
  children: FakeNode[] = [];
  constructor(parent: FakeNode | null = null) {
    this.parentNode = parent;
    this.parentElement = parent;
  }
  appendChild(child: FakeNode): FakeNode {
    if (child.parentElement && child.parentElement !== this) {
      const old = child.parentElement.children;
      const at = old.indexOf(child);
      if (at >= 0) old.splice(at, 1);
    }
    this.children.push(child);
    child.parentElement = this;
    child.parentNode = this;
    return child;
  }
  contains(candidate: FakeNode | null): boolean {
    return candidate !== null && this.children.includes(candidate);
  }
}

function holds(list: FakeNode[], node: FakeNode): boolean {
  return list.indexOf(node) >= 0;
}

function installFakeDocument(host: FakeNode) {
  (globalThis as Record<string, unknown>).HTMLElement = FakeNode;
  (globalThis as Record<string, unknown>).document = {
    querySelector: (selector: string) =>
      selector === "#host" ? (host as unknown as HTMLElement) : null,
  };
}

afterEach(() => {
  delete (globalThis as Record<string, unknown>).document;
  delete (globalThis as Record<string, unknown>).HTMLElement;
});

describe("resolvePortalTarget", () => {
  it("returns null without a DOM instead of crashing (SSR / node)", () => {
    expect(resolvePortalTarget("body")).toBeNull();
    const el = new FakeNode();
    expect(resolvePortalTarget(el as unknown as HTMLElement)).toBeNull();
  });

  it("resolves selectors to elements and rejects misses", () => {
    const host = new FakeNode();
    installFakeDocument(host);

    expect(resolvePortalTarget("#host")).toBe(host as unknown as HTMLElement);
    expect(resolvePortalTarget("#missing")).toBeNull();
    expect(
      resolvePortalTarget({} as unknown as HTMLElement),
    ).toBeNull();
  });
});

describe("portal action", () => {
  it("moves the node to the target and restores its home on destroy", () => {
    const home = new FakeNode();
    const host = new FakeNode();
    installFakeDocument(host);
    const popover = home.appendChild(new FakeNode());

    const action = portal(popover as unknown as HTMLElement, "#host");
    expect(holds(host.children, popover)).toBe(true);

    action.destroy();
    expect(holds(home.children, popover)).toBe(true);
    expect(holds(host.children, popover)).toBe(false);
  });

  it("update() re-parents when the target changes", () => {
    const home = new FakeNode();
    const first = new FakeNode();
    const second = new FakeNode();
    installFakeDocument(first);
    const popover = home.appendChild(new FakeNode());

    const action = portal(popover as unknown as HTMLElement, "#host");
    installFakeDocument(second);
    action.update("#host");
    expect(holds(first.children, popover)).toBe(false);
    expect(holds(second.children, popover)).toBe(true);

    action.destroy();
    expect(holds(home.children, popover)).toBe(true);
  });

  it("keeps the node in place when there is no DOM", () => {
    const home = new FakeNode();
    const popover = home.appendChild(new FakeNode());

    const action = portal(popover as unknown as HTMLElement, "body");
    expect(holds(home.children, popover)).toBe(true);

    action.update("body");
    action.destroy();
    expect(holds(home.children, popover)).toBe(true);
  });
});
