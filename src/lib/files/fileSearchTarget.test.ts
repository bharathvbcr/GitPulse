import { afterEach, describe, expect, it, vi } from "vitest";
import {
  MAX_SELECTION_SEED,
  extractSelectionSeed,
  fileSearchControl,
  triggerFileSearch,
  type FileSearchController,
} from "./fileSearchTarget";

interface FakeControlNode {
  dataset: Record<string, string>;
  inertOrHidden: boolean;
  closest(selector: string): FakeControlNode | null;
}

function makeNode(inertOrHidden = false): FakeControlNode {
  return {
    dataset: {},
    inertOrHidden,
    closest(selector: string) {
      if (this.inertOrHidden && selector.includes("inert")) return this;
      return null;
    },
  };
}

function makeScope(nodes: FakeControlNode[]): ParentNode {
  return {
    querySelectorAll(selector: string) {
      if (selector !== "[data-file-search-control]") return [];
      return nodes.filter((n) => "fileSearchControl" in n.dataset);
    },
  } as unknown as ParentNode;
}

describe("fileSearchTarget", () => {
  it("registers, updates, and cleans up controllers without leaking stale entries", () => {
    const el = makeNode();
    const first: FileSearchController = {
      openFind: vi.fn(),
      openLine: vi.fn(),
      canGoToLine: true,
    };
    const second: FileSearchController = {
      openFind: vi.fn(),
      openLine: vi.fn(),
      canGoToLine: false,
    };

    const action = fileSearchControl(el as unknown as HTMLElement, first);
    expect(el.dataset.fileSearchControl).toBe("");
    const scope = makeScope([el]);

    expect(triggerFileSearch(scope, "find")).toBe(true);
    expect(first.openFind).toHaveBeenCalledTimes(1);

    action.update(second);
    expect(triggerFileSearch(scope, "line")).toBe(false);
    expect(second.openLine).not.toHaveBeenCalled();
    expect(triggerFileSearch(scope, "find")).toBe(true);
    expect(second.openFind).toHaveBeenCalledTimes(1);

    action.update(null);
    expect("fileSearchControl" in el.dataset).toBe(false);
    expect(triggerFileSearch(scope, "find")).toBe(false);

    action.update(first);
    expect("fileSearchControl" in el.dataset).toBe(true);
    action.destroy();
    expect("fileSearchControl" in el.dataset).toBe(false);
    expect(triggerFileSearch(scope, "find")).toBe(false);
  });

  it("skips inert/hidden controls and falls through to the first active controller supporting the chord", () => {
    const hiddenEl = makeNode(true);
    const unregisteredEl = makeNode(false);
    unregisteredEl.dataset.fileSearchControl = "";
    const noLineEl = makeNode(false);
    const lineEl = makeNode(false);

    const hiddenCtrl: FileSearchController = {
      openFind: vi.fn(),
      openLine: vi.fn(),
      canGoToLine: true,
    };
    const noLineCtrl: FileSearchController = {
      openFind: vi.fn(),
      openLine: vi.fn(),
      canGoToLine: false,
    };
    const lineCtrl: FileSearchController = {
      openFind: vi.fn(),
      openLine: vi.fn(),
      canGoToLine: true,
    };

    fileSearchControl(hiddenEl as unknown as HTMLElement, hiddenCtrl);
    fileSearchControl(noLineEl as unknown as HTMLElement, noLineCtrl);
    fileSearchControl(lineEl as unknown as HTMLElement, lineCtrl);

    const scope = makeScope([hiddenEl, unregisteredEl, noLineEl, lineEl]);

    expect(triggerFileSearch(null, "find")).toBe(false);
    expect(triggerFileSearch(makeScope([]), "find")).toBe(false);

    // Find targets the first visible controller (noLineEl), skipping hiddenEl and unregisteredEl.
    expect(triggerFileSearch(scope, "find", "needle")).toBe(true);
    expect(hiddenCtrl.openFind).not.toHaveBeenCalled();
    expect(noLineCtrl.openFind).toHaveBeenCalledWith("needle");
    expect(lineCtrl.openFind).not.toHaveBeenCalled();

    // Line chord skips noLineEl because canGoToLine is false, hitting lineEl instead.
    expect(triggerFileSearch(scope, "line")).toBe(true);
    expect(hiddenCtrl.openLine).not.toHaveBeenCalled();
    expect(noLineCtrl.openLine).not.toHaveBeenCalled();
    expect(lineCtrl.openLine).toHaveBeenCalledTimes(1);
  });

  describe("extractSelectionSeed", () => {
    const origDocument = globalThis.document;
    const origWindow = globalThis.window;

    afterEach(() => {
      if (origDocument === undefined) delete (globalThis as { document?: unknown }).document;
      else (globalThis as { document: unknown }).document = origDocument;
      if (origWindow === undefined) delete (globalThis as { window?: unknown }).window;
      else (globalThis as { window: unknown }).window = origWindow;
    });

    it("reads single-line selection from a focused textarea and rejects multiline or oversized text", () => {
      const textarea = {
        value: "alpha beta\ngamma " + "x".repeat(MAX_SELECTION_SEED + 5),
        selectionStart: 6,
        selectionEnd: 10,
      } as unknown as HTMLTextAreaElement;
      (globalThis as { document: unknown }).document = { activeElement: textarea };

      expect(extractSelectionSeed(textarea, null)).toBe("beta");

      textarea.selectionStart = 6;
      textarea.selectionEnd = 14;
      expect(extractSelectionSeed(textarea, null)).toBeUndefined();

      textarea.selectionStart = 17;
      textarea.selectionEnd = textarea.value.length;
      expect(extractSelectionSeed(textarea, null)).toBeUndefined();
    });

    it("reads DOM selection only when anchored inside scopeRoot", () => {
      const anchor = {} as Node;
      const root = { contains: (n: Node | null) => n === anchor } as unknown as Node;
      const otherRoot = { contains: () => false } as unknown as Node;
      (globalThis as { document: unknown }).document = { activeElement: null };
      (globalThis as { window: unknown }).window = {
        getSelection: () => ({
          isCollapsed: false,
          anchorNode: anchor,
          toString: () => "symbolName",
        }),
      };

      expect(extractSelectionSeed(null, root)).toBe("symbolName");
      expect(extractSelectionSeed(null, otherRoot)).toBeUndefined();
    });
  });
});
