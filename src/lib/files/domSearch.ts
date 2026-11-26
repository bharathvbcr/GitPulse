import {
  DEFAULT_MAX_MATCHES,
  DEFAULT_MAX_MILLIS,
  buildMatcher,
  hasUnboundedNesting,
  matchesInLine,
  type LineMatch,
  type SearchOptions,
  type SearchResult,
} from "../text/lineSearch";

const MARK_ATTR = "data-gp-find-hit";
const ACTIVE_ATTR = "data-gp-find-active";
const SKIP_TAGS = new Set(["SCRIPT", "STYLE", "NOSCRIPT"]);

/**
 * Removes rendered find highlight `<mark>` wrappers under `root` and merges
 * adjacent text nodes back together without touching structural elements.
 */
export function clearDomHighlights(root: HTMLElement | null | undefined): void {
  if (!root) return;
  const marks = root.querySelectorAll<HTMLElement>(`mark[${MARK_ATTR}]`);
  if (marks.length === 0) return;
  const parents = new Set<Node>();
  for (const mark of marks) {
    const parent = mark.parentNode;
    if (!parent) continue;
    parents.add(parent);
    while (mark.firstChild) {
      parent.insertBefore(mark.firstChild, mark);
    }
    parent.removeChild(mark);
  }
  for (const parent of parents) {
    if (parent.isConnected || root.contains(parent)) {
      parent.normalize();
    }
  }
}

/**
 * Highlights matches of `query` in visible text nodes beneath `root`, reusing
 * `text/lineSearch` bounds and catastrophic-backtracking guards.
 */
export function applyDomHighlights(
  root: HTMLElement | null | undefined,
  query: string,
  options: SearchOptions = {},
): { result: SearchResult; marks: HTMLElement[] } {
  clearDomHighlights(root);
  if (!root) {
    return { result: { matches: [], truncated: false, invalid: false }, marks: [] };
  }

  const matcher = buildMatcher(query, options);
  if (!matcher) {
    const asked = !!options.regex && query.trim().length > 0;
    if (!asked) {
      return { result: { matches: [], truncated: false, invalid: false }, marks: [] };
    }
    return {
      result: {
        matches: [],
        truncated: false,
        invalid: true,
        reason: hasUnboundedNesting(query) ? "unbounded" : "syntax",
      },
      marks: [],
    };
  }

  const doc = root.ownerDocument;
  const walker = doc.createTreeWalker(root, NodeFilter.SHOW_TEXT, {
    acceptNode(node) {
      const parent = node.parentElement;
      if (!parent || SKIP_TAGS.has(parent.tagName)) return NodeFilter.FILTER_REJECT;
      return node.nodeValue ? NodeFilter.FILTER_ACCEPT : NodeFilter.FILTER_SKIP;
    },
  });

  const textNodes: Text[] = [];
  let current = walker.nextNode();
  while (current) {
    textNodes.push(current as Text);
    current = walker.nextNode();
  }

  const cap = Math.max(0, options.maxMatches ?? DEFAULT_MAX_MATCHES);
  const deadline = Date.now() + Math.max(1, options.maxMillis ?? DEFAULT_MAX_MILLIS);
  const matches: LineMatch[] = [];
  const marks: HTMLElement[] = [];
  let truncated = false;

  for (let i = 0; i < textNodes.length; i += 1) {
    if (matches.length >= cap) {
      truncated = true;
      break;
    }
    if ((i & 0xff) === 0 && i > 0 && Date.now() > deadline) {
      truncated = true;
      break;
    }
    const node = textNodes[i];
    const text = node.nodeValue ?? "";
    if (!text) continue;
    const nodeMatches: LineMatch[] = [];
    matchesInLine(text, matcher, i, nodeMatches, cap - matches.length);
    if (nodeMatches.length === 0) continue;

    const fragment = doc.createDocumentFragment();
    let cursor = 0;
    for (const hit of nodeMatches) {
      if (hit.colStart > cursor) {
        fragment.appendChild(doc.createTextNode(text.slice(cursor, hit.colStart)));
      }
      const mark = doc.createElement("mark");
      mark.setAttribute(MARK_ATTR, "");
      mark.className = "rounded-sm bg-warn/30 text-inherit";
      mark.textContent = text.slice(hit.colStart, hit.colStart + hit.length);
      fragment.appendChild(mark);
      marks.push(mark);
      matches.push(hit);
      cursor = hit.colStart + hit.length;
    }
    if (cursor < text.length) {
      fragment.appendChild(doc.createTextNode(text.slice(cursor)));
    }
    node.parentNode?.replaceChild(fragment, node);
  }

  truncated = truncated || (matches.length >= cap && cap > 0);
  return {
    result: { matches, truncated, invalid: false },
    marks,
  };
}

/**
 * Updates the active match styling across rendered `<mark>` hits and optionally
 * scrolls the active hit into view.
 */
export function setActiveDomHighlight(
  marks: readonly HTMLElement[],
  activeIdx: number,
  scroll = false,
): void {
  for (let i = 0; i < marks.length; i += 1) {
    const mark = marks[i];
    if (i === activeIdx) {
      mark.classList.remove("bg-warn/30", "text-inherit");
      mark.classList.add("bg-warn", "text-onWarn");
      mark.setAttribute(ACTIVE_ATTR, "true");
    } else {
      mark.classList.remove("bg-warn", "text-onWarn");
      mark.classList.add("bg-warn/30", "text-inherit");
      mark.removeAttribute(ACTIVE_ATTR);
    }
  }
  if (scroll && activeIdx >= 0 && activeIdx < marks.length) {
    marks[activeIdx]?.scrollIntoView?.({ block: "center" });
  }
}
