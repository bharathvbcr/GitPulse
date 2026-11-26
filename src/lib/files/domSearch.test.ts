import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { applyDomHighlights, clearDomHighlights, setActiveDomHighlight } from "./domSearch";

class FakeClassList {
  private readonly set = new Set<string>();

  syncFromString(value: string) {
    this.set.clear();
    for (const part of value.split(/\s+/)) {
      if (part) this.set.add(part);
    }
  }

  add(...tokens: string[]) {
    for (const t of tokens) if (t) this.set.add(t);
  }

  remove(...tokens: string[]) {
    for (const t of tokens) this.set.delete(t);
  }

  contains(token: string) {
    return this.set.has(token);
  }

  toString() {
    return [...this.set].join(" ");
  }
}

class FakeNode {
  parentNode: FakeNode | null = null;
  childNodes: FakeNode[] = [];
  isConnected = true;

  get parentElement(): FakeElement | null {
    return this.parentNode instanceof FakeElement ? this.parentNode : null;
  }

  get firstChild(): FakeNode | null {
    return this.childNodes[0] ?? null;
  }

  contains(target: FakeNode | null): boolean {
    if (!target) return false;
    if (target === this) return true;
    return this.childNodes.some((child) => child.contains(target));
  }

  appendChild<T extends FakeNode>(child: T): T {
    return this.insertBefore(child, null);
  }

  insertBefore<T extends FakeNode>(child: T, ref: FakeNode | null): T {
    if (child instanceof FakeDocumentFragment) {
      const items = [...child.childNodes];
      for (const item of items) {
        this.insertBefore(item, ref);
      }
      child.childNodes = [];
      return child;
    }
    if (child.parentNode) {
      child.parentNode.removeChild(child);
    }
    child.parentNode = this;
    if (!ref) {
      this.childNodes.push(child);
    } else {
      const idx = this.childNodes.indexOf(ref);
      if (idx === -1) this.childNodes.push(child);
      else this.childNodes.splice(idx, 0, child);
    }
    return child;
  }

  removeChild<T extends FakeNode>(child: T): T {
    const idx = this.childNodes.indexOf(child);
    if (idx !== -1) {
      this.childNodes.splice(idx, 1);
      child.parentNode = null;
    }
    return child;
  }

  replaceChild(newChild: FakeNode, oldChild: FakeNode): FakeNode {
    this.insertBefore(newChild, oldChild);
    this.removeChild(oldChild);
    return oldChild;
  }

  normalize() {
    const merged: FakeNode[] = [];
    for (const child of this.childNodes) {
      if (child instanceof FakeText) {
        if (!child.nodeValue) {
          child.parentNode = null;
          continue;
        }
        const prev = merged[merged.length - 1];
        if (prev instanceof FakeText) {
          prev.nodeValue += child.nodeValue;
          child.parentNode = null;
          continue;
        }
      }
      merged.push(child);
    }
    this.childNodes = merged;
  }
}

class FakeText extends FakeNode {
  constructor(public nodeValue: string) {
    super();
  }
}

class FakeDocumentFragment extends FakeNode {}

class FakeElement extends FakeNode {
  readonly tagName: string;
  readonly attributes = new Map<string, string>();
  readonly classList = new FakeClassList();
  scrollCalls: unknown[] = [];

  constructor(tagName: string, readonly ownerDocument: FakeDocument) {
    super();
    this.tagName = tagName.toUpperCase();
  }

  get className(): string {
    return this.classList.toString();
  }

  set className(value: string) {
    this.classList.syncFromString(value);
  }

  set textContent(value: string) {
    this.childNodes = value ? [Object.assign(new FakeText(value), { parentNode: this })] : [];
  }

  get textContent(): string {
    return this.childNodes
      .map((child) => (child instanceof FakeText ? child.nodeValue : (child as FakeElement).textContent))
      .join("");
  }

  setAttribute(name: string, value: string) {
    this.attributes.set(name, value);
  }

  removeAttribute(name: string) {
    this.attributes.delete(name);
  }

  getAttribute(name: string): string | null {
    return this.attributes.get(name) ?? null;
  }

  hasAttribute(name: string): boolean {
    return this.attributes.has(name);
  }

  scrollIntoView(options?: unknown) {
    this.scrollCalls.push(options ?? null);
  }

  querySelectorAll<T extends FakeElement = FakeElement>(selector: string): T[] {
    const attrMatch = /^mark\[([^\]]+)\]$/.exec(selector);
    const out: T[] = [];
    const walk = (node: FakeNode) => {
      for (const child of node.childNodes) {
        if (child instanceof FakeElement) {
          if (attrMatch && child.tagName === "MARK" && child.hasAttribute(attrMatch[1])) {
            out.push(child as T);
          }
          walk(child);
        }
      }
    };
    walk(this);
    return out;
  }
}

class FakeDocument {
  createElement(tag: string) {
    return new FakeElement(tag, this);
  }

  createTextNode(text: string) {
    return new FakeText(text);
  }

  createDocumentFragment() {
    return new FakeDocumentFragment();
  }

  createTreeWalker(
    root: FakeNode,
    _whatToShow: number,
    filter: { acceptNode(node: FakeNode): number },
  ) {
    const list: FakeNode[] = [];
    const visit = (node: FakeNode) => {
      for (const child of node.childNodes) {
        if (child instanceof FakeText) {
          if (filter.acceptNode(child) === 1) {
            list.push(child);
          }
        } else {
          if (child instanceof FakeElement && ["SCRIPT", "STYLE", "NOSCRIPT"].includes(child.tagName)) {
            const probe = new FakeText("x");
            probe.parentNode = child;
            if (filter.acceptNode(probe) === 2) continue;
          }
          visit(child);
        }
      }
    };
    visit(root);
    let idx = 0;
    return {
      nextNode() {
        return idx < list.length ? list[idx++] : null;
      },
    };
  }
}

const originalNodeFilter = (globalThis as { NodeFilter?: unknown }).NodeFilter;

beforeEach(() => {
  (globalThis as { NodeFilter: unknown }).NodeFilter = {
    SHOW_TEXT: 4,
    FILTER_ACCEPT: 1,
    FILTER_REJECT: 2,
    FILTER_SKIP: 3,
  };
});

afterEach(() => {
  vi.restoreAllMocks();
  if (originalNodeFilter === undefined) {
    delete (globalThis as { NodeFilter?: unknown }).NodeFilter;
  } else {
    (globalThis as { NodeFilter: unknown }).NodeFilter = originalNodeFilter;
  }
});

describe("domSearch", () => {
  it("handles null roots and empty queries cleanly", () => {
    clearDomHighlights(null);
    expect(applyDomHighlights(null, "alpha")).toEqual({
      result: { matches: [], truncated: false, invalid: false },
      marks: [],
    });

    const doc = new FakeDocument();
    const root = doc.createElement("div");
    root.appendChild(doc.createTextNode("alpha beta"));
    expect(applyDomHighlights(root as unknown as HTMLElement, "   ")).toEqual({
      result: { matches: [], truncated: false, invalid: false },
      marks: [],
    });
  });

  it("reports invalid regex syntax and unbounded repetition without touching the DOM", () => {
    const doc = new FakeDocument();
    const root = doc.createElement("div");
    root.appendChild(doc.createTextNode("aaa"));

    const syntaxErr = applyDomHighlights(root as unknown as HTMLElement, "(unclosed", { regex: true });
    expect(syntaxErr.marks).toEqual([]);
    expect(syntaxErr.result).toEqual({
      matches: [],
      truncated: false,
      invalid: true,
      reason: "syntax",
    });

    const nestedErr = applyDomHighlights(root as unknown as HTMLElement, "(a+)+", { regex: true });
    expect(nestedErr.marks).toEqual([]);
    expect(nestedErr.result).toEqual({
      matches: [],
      truncated: false,
      invalid: true,
      reason: "unbounded",
    });
  });

  it("highlights text nodes, skips script/style/noscript/empty nodes, and restores clean text on clear", () => {
    const doc = new FakeDocument();
    const root = doc.createElement("div");
    const p = doc.createElement("p");
    p.appendChild(doc.createTextNode("Find foo and another Foo here"));
    p.appendChild(doc.createTextNode(""));
    const script = doc.createElement("script");
    script.appendChild(doc.createTextNode("foo in script"));
    const style = doc.createElement("style");
    style.appendChild(doc.createTextNode("foo in style"));
    root.appendChild(p);
    root.appendChild(script);
    root.appendChild(style);

    const { result, marks } = applyDomHighlights(root as unknown as HTMLElement, "foo");
    expect(result.invalid).toBe(false);
    expect(result.truncated).toBe(false);
    expect(result.matches).toHaveLength(2);
    expect(marks).toHaveLength(2);
    expect(marks.map((m) => m.textContent)).toEqual(["foo", "Foo"]);
    expect(p.textContent).toBe("Find foo and another Foo here");

    // Re-running with case sensitivity narrows to the single lowercase match and cleans up old marks first.
    const sensitive = applyDomHighlights(root as unknown as HTMLElement, "foo", { caseSensitive: true });
    expect(sensitive.marks).toHaveLength(1);
    expect(sensitive.marks[0].textContent).toBe("foo");

    clearDomHighlights(root as unknown as HTMLElement);
    expect(root.querySelectorAll("mark[data-gp-find-hit]")).toHaveLength(0);
    expect(p.childNodes).toHaveLength(1);
    expect(p.childNodes[0]).toBeInstanceOf(FakeText);
    expect((p.childNodes[0] as FakeText).nodeValue).toBe("Find foo and another Foo here");
  });

  it("updates active highlight classes and scrolls the active mark into view", () => {
    const doc = new FakeDocument();
    const root = doc.createElement("div");
    root.appendChild(doc.createTextNode("hit one hit two"));

    const { marks } = applyDomHighlights(root as unknown as HTMLElement, "hit");
    expect(marks).toHaveLength(2);

    setActiveDomHighlight(marks, 1, true);
    const m0 = marks[0] as unknown as FakeElement;
    const m1 = marks[1] as unknown as FakeElement;
    expect(m0.classList.contains("bg-warn/30")).toBe(true);
    expect(m0.hasAttribute("data-gp-find-active")).toBe(false);
    expect(m1.classList.contains("bg-warn")).toBe(true);
    expect(m1.classList.contains("text-onWarn")).toBe(true);
    expect(m1.getAttribute("data-gp-find-active")).toBe("true");
    expect(m1.scrollCalls).toEqual([{ block: "center" }]);

    // Switching active index resets the prior active mark and respects scroll = false.
    setActiveDomHighlight(marks, 0, false);
    expect(m1.classList.contains("bg-warn/30")).toBe(true);
    expect(m1.hasAttribute("data-gp-find-active")).toBe(false);
    expect(m0.classList.contains("bg-warn")).toBe(true);
    expect(m0.scrollCalls).toEqual([]);
  });

  it("enforces match caps and time budgets safely", () => {
    const doc = new FakeDocument();
    const root = doc.createElement("div");
    root.appendChild(doc.createTextNode("a a a a a"));

    const capped = applyDomHighlights(root as unknown as HTMLElement, "a", { maxMatches: 2 });
    expect(capped.result.matches).toHaveLength(2);
    expect(capped.result.truncated).toBe(true);

    // Build 260 separate paragraphs to cross the deadline check interval (i & 0xff) === 0.
    const bigRoot = doc.createElement("div");
    for (let i = 0; i < 260; i += 1) {
      const span = doc.createElement("span");
      span.appendChild(doc.createTextNode("x"));
      bigRoot.appendChild(span);
    }
    let now = 1000;
    vi.spyOn(Date, "now").mockImplementation(() => {
      now += 10;
      return now;
    });
    const timedOut = applyDomHighlights(bigRoot as unknown as HTMLElement, "x", { maxMillis: 5 });
    expect(timedOut.result.truncated).toBe(true);
    expect(timedOut.result.matches.length).toBeLessThanOrEqual(256);
  });
});
