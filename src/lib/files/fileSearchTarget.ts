/**
 * Scoped registry linking visible inline `FindField` controls inside a file
 * pane (`FileViewer` or `TerminalFilePane`) so keyboard chords (`⌘F`, `⌘G`)
 * reach the active viewer without prop-drilling through `MediaViewer` /
 * `LazyMount` / `LazyView`.
 */
export interface FileSearchController {
  openFind(seed?: string): void;
  openLine(): void;
  readonly canGoToLine: boolean;
}

export const MAX_SELECTION_SEED = 200;

/**
 * Extracts a single-line non-empty text selection from either a focused
 * textarea or a DOM selection contained in `scopeRoot`, for seeding `⌘F`.
 */
export function extractSelectionSeed(
  textarea?: HTMLTextAreaElement | null,
  scopeRoot?: Node | null,
): string | undefined {
  let raw = "";
  if (
    textarea &&
    typeof document !== "undefined" &&
    document.activeElement === textarea
  ) {
    const start = textarea.selectionStart ?? 0;
    const end = textarea.selectionEnd ?? 0;
    if (end > start) {
      raw = textarea.value.slice(start, end);
    }
  } else if (typeof window !== "undefined" && typeof window.getSelection === "function") {
    const sel = window.getSelection();
    if (sel && !sel.isCollapsed) {
      const anchor = sel.anchorNode;
      if (!scopeRoot || (anchor && scopeRoot.contains(anchor))) {
        raw = sel.toString();
      }
    }
  }
  if (!raw || raw.length > MAX_SELECTION_SEED || /[\r\n]/.test(raw) || !raw.trim()) {
    return undefined;
  }
  return raw;
}

const controllers = new WeakMap<HTMLElement, FileSearchController>();

/** Registers a viewer's inline find/go-to-line controller on its DOM node. */
export function fileSearchControl(
  node: HTMLElement,
  controller: FileSearchController | null,
) {
  function sync(next: FileSearchController | null) {
    if (next) {
      node.dataset.fileSearchControl = "";
      controllers.set(node, next);
    } else {
      delete node.dataset.fileSearchControl;
      controllers.delete(node);
    }
  }
  sync(controller);
  return {
    update(next: FileSearchController | null) {
      sync(next);
    },
    destroy() {
      delete node.dataset.fileSearchControl;
      controllers.delete(node);
    },
  };
}

/**
 * Triggers the first visible, non-inert inline search/go-to-line control inside
 * `scopeRoot`, returning false when none applies so chords can fall through.
 */
export function triggerFileSearch(
  scopeRoot: ParentNode | null | undefined,
  kind: "find" | "line",
  seed?: string,
): boolean {
  if (!scopeRoot) return false;
  const candidates = scopeRoot.querySelectorAll<HTMLElement>("[data-file-search-control]");
  for (const el of candidates) {
    if (el.closest("[inert], [hidden], .hidden")) continue;
    const controller = controllers.get(el);
    if (!controller) continue;
    if (kind === "line" && !controller.canGoToLine) continue;
    if (kind === "line") controller.openLine();
    else controller.openFind(seed);
    return true;
  }
  return false;
}
