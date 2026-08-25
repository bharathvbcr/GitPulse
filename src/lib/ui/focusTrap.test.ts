import { afterEach, describe, expect, it } from "vitest";
import {
  FOCUSABLE_SELECTOR,
  cycleFocus,
  enumerateFocusables,
} from "./focusTrap";

/** Minimal attribute-backed element standing in for HTMLElement. */
class FakeFocusable {
  attributes = new Map<string, string>();
  focusCalls = 0;
  constructor(attributes: Record<string, string> = {}) {
    for (const [name, value] of Object.entries(attributes)) {
      this.attributes.set(name, value);
    }
  }
  getAttribute(name: string): string | null {
    return this.attributes.has(name) ? (this.attributes.get(name) as string) : null;
  }
  hasAttribute(name: string): boolean {
    return this.attributes.has(name);
  }
  focus(): void {
    this.focusCalls += 1;
    const doc = (globalThis as Record<string, unknown>).document as {
      activeElement: unknown;
    };
    // Refuse focus when marked inert: simulates display:none candidates
    // that match the selector but cannot actually take focus.
    if (!this.attributes.has("data-inert")) doc.activeElement = this;
  }
}

function installFakeDocument() {
  (globalThis as Record<string, unknown>).HTMLElement = FakeFocusable;
  (globalThis as Record<string, unknown>).document = { activeElement: null };
}

afterEach(() => {
  delete (globalThis as Record<string, unknown>).document;
  delete (globalThis as Record<string, unknown>).HTMLElement;
});

function containerOf(children: FakeFocusable[]) {
  return {
    children,
    querySelectorAll() {
      // Fixtures pre-simulate selector matching; enumeration adds ordering
      // and tab-visibility filtering on top.
      return children;
    },
  };
}

describe("FOCUSABLE_SELECTOR", () => {
  it("covers interactive controls and excludes disabled/negative tabindex", () => {
    expect(FOCUSABLE_SELECTOR).toContain("button:not([disabled])");
    expect(FOCUSABLE_SELECTOR).toContain("input:not([disabled])");
    expect(FOCUSABLE_SELECTOR).toContain("a[href]");
    expect(FOCUSABLE_SELECTOR).toContain("[tabindex]:not([tabindex='-1'])");
    expect(FOCUSABLE_SELECTOR).toContain("[contenteditable='true']");
  });
});

describe("enumerateFocusables", () => {
  it("returns candidates in DOM order", () => {
    installFakeDocument();
    const input = new FakeFocusable({ type: "text" });
    const save = new FakeFocusable({});
    const cancel = new FakeFocusable({});
    const list = containerOf([input, save, cancel]);

    expect(enumerateFocusables(list)).toEqual([input, save, cancel]);
  });

  it("skips [hidden] and aria-hidden='true' candidates", () => {
    installFakeDocument();
    const visible = new FakeFocusable({});
    const hidden = new FakeFocusable({ hidden: "" });
    const veiled = new FakeFocusable({ "aria-hidden": "true" });
    const after = new FakeFocusable({});
    const list = containerOf([hidden, visible, veiled, after]);

    expect(enumerateFocusables(list)).toEqual([visible, after]);
  });

  it("returns an empty list for a bare dialog", () => {
    installFakeDocument();
    expect(enumerateFocusables(containerOf([]))).toEqual([]);
  });
});

describe("cycleFocus", () => {
  function currentTarget(): unknown {
    const doc = (globalThis as Record<string, unknown>).document as {
      activeElement: unknown;
    };
    return doc.activeElement;
  }

  it("enters at the front when focus starts outside", () => {
    installFakeDocument();
    const first = new FakeFocusable({});
    const second = new FakeFocusable({});
    const dialog = containerOf([first, second]);

    cycleFocus(dialog as unknown as HTMLElement, true);
    expect(currentTarget()).toBe(first);

    cycleFocus(dialog as unknown as HTMLElement, true);
    expect(currentTarget()).toBe(second);
  });

  it("wraps forward past the last item back to the first", () => {
    installFakeDocument();
    const items = [new FakeFocusable({}), new FakeFocusable({})];
    const dialog = containerOf(items);
    (globalThis as Record<string, unknown>).document = {
      activeElement: items[1],
    };

    cycleFocus(dialog as unknown as HTMLElement, true);
    expect(currentTarget()).toBe(items[0]);
  });

  it("enters at the back when moving backward from outside", () => {
    installFakeDocument();
    const items = [new FakeFocusable({}), new FakeFocusable({})];
    const dialog = containerOf(items);

    cycleFocus(dialog as unknown as HTMLElement, false);
    expect(currentTarget()).toBe(items[1]);
  });

  it("wraps backward past the first item to the last", () => {
    installFakeDocument();
    const items = [new FakeFocusable({}), new FakeFocusable({})];
    const dialog = containerOf(items);
    (globalThis as Record<string, unknown>).document = {
      activeElement: items[0],
    };

    cycleFocus(dialog as unknown as HTMLElement, false);
    expect(currentTarget()).toBe(items[1]);
  });

  it("skips a candidate whose focus() silently fails and parks nowhere bad", () => {
    installFakeDocument();
    const stuck = new FakeFocusable({ "data-inert": "true" });
    const reachable = new FakeFocusable({});
    const dialog = containerOf([stuck, reachable]);

    cycleFocus(dialog as unknown as HTMLElement, true);
    expect(currentTarget()).toBe(reachable);
    expect(reachable.focusCalls).toBe(1);
  });
});

describe("trapFocus action", () => {
  function currentTarget(): unknown {
    const doc = (globalThis as Record<string, unknown>).document as {
      activeElement: unknown;
    };
    return doc.activeElement;
  }

  class FakeNodeWithEvents extends FakeFocusable {
    isConnected = true;
    listeners = new Map<string, Array<(event: unknown) => void>>();
    children: FakeFocusable[] = [];

    querySelectorAll(_selector: string): FakeFocusable[] {
      return this.children;
    }

    addEventListener(event: string, fn: (event: unknown) => void): void {
      const list = this.listeners.get(event) ?? [];
      list.push(fn);
      this.listeners.set(event, list);
    }

    removeEventListener(event: string, fn: (event: unknown) => void): void {
      const list = this.listeners.get(event) ?? [];
      const index = list.indexOf(fn);
      if (index >= 0) list.splice(index, 1);
    }

    dispatchEvent(event: unknown & { type: string }): void {
      for (const fn of this.listeners.get(event.type) ?? []) {
        fn(event);
      }
    }
  }

  it("autofocuses first focusable by default when options are omitted", () => {
    installFakeDocument();
    (globalThis as Record<string, unknown>).HTMLElement = FakeNodeWithEvents;

    const dialog = new FakeNodeWithEvents();
    const btn1 = new FakeNodeWithEvents();
    const btn2 = new FakeNodeWithEvents();
    dialog.children = [btn1, btn2];

    const action = import("./focusTrap").then(({ trapFocus }) => {
      const res = trapFocus(dialog as unknown as HTMLElement);
      expect(currentTarget()).toBe(btn1);
      expect(btn1.focusCalls).toBe(1);
      res.destroy();
    });
    return action;
  });

  it("respects autofocus: false", async () => {
    installFakeDocument();
    (globalThis as Record<string, unknown>).HTMLElement = FakeNodeWithEvents;

    const dialog = new FakeNodeWithEvents();
    const btn1 = new FakeNodeWithEvents();
    dialog.children = [btn1];

    const { trapFocus } = await import("./focusTrap");
    const res = trapFocus(dialog as unknown as HTMLElement, { autofocus: false });
    expect(currentTarget()).toBeNull();
    expect(btn1.focusCalls).toBe(0);
    res.destroy();
  });

  it("prioritizes initial() when provided", async () => {
    installFakeDocument();
    (globalThis as Record<string, unknown>).HTMLElement = FakeNodeWithEvents;

    const dialog = new FakeNodeWithEvents();
    const btn1 = new FakeNodeWithEvents();
    const input = new FakeNodeWithEvents();
    dialog.children = [btn1, input];

    const { trapFocus } = await import("./focusTrap");
    const res = trapFocus(dialog as unknown as HTMLElement, {
      initial: () => input as unknown as HTMLElement,
    });
    expect(currentTarget()).toBe(input);
    expect(input.focusCalls).toBe(1);
    expect(btn1.focusCalls).toBe(0);
    res.destroy();
  });

  it("cycles focus on Tab key and restores previous element on destroy", async () => {
    installFakeDocument();
    (globalThis as Record<string, unknown>).HTMLElement = FakeNodeWithEvents;

    const outside = new FakeNodeWithEvents();
    (globalThis as Record<string, unknown>).document = { activeElement: outside };

    const dialog = new FakeNodeWithEvents();
    const btn1 = new FakeNodeWithEvents();
    const btn2 = new FakeNodeWithEvents();
    dialog.children = [btn1, btn2];

    const { trapFocus } = await import("./focusTrap");
    const action = trapFocus(dialog as unknown as HTMLElement);
    expect(currentTarget()).toBe(btn1);

    let defaultPrevented = false;
    const tabEvent = {
      type: "keydown",
      key: "Tab",
      shiftKey: false,
      altKey: false,
      ctrlKey: false,
      metaKey: false,
      preventDefault: () => {
        defaultPrevented = true;
      },
    };
    dialog.dispatchEvent(tabEvent);
    expect(defaultPrevented).toBe(true);
    expect(currentTarget()).toBe(btn2);

    action.destroy();
    expect(currentTarget()).toBe(outside);
  });
});
