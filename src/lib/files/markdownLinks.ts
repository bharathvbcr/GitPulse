/**
 * What rendered Markdown may do when it is clicked, and the DOM pass that
 * makes a rendered body safe to embed in the app's own document.
 *
 * Rendered Markdown is untrusted repository content placed in the app shell.
 * Two things follow. A plain `<a href>` must never be followed by the
 * webview — it would navigate GitPulse itself away (an `https:` link) or into
 * a dead route (`./docs/a.md`). And the ids the renderer gives headings and
 * footnotes share the document with the app's own ids: a README heading
 * "GitPulse fleet filter" slugs to an id the fleet view looks up.
 *
 * So every click is judged here — the URL rules are the terminal's
 * ({@link isOpenableUrl}, {@link resolveRepoFile}) rather than a second set —
 * and {@link prepareRenderedMarkdown} namespaces the ids before insertion.
 */

import { isOpenableUrl, MAX_URL_LENGTH, resolveRepoFile } from "../terminal/links";

/** Prefix every rendered id carries in the app's document. */
export const MARKDOWN_ID_PREFIX = "gp-md-";

export type MarkdownLinkAction =
  | { kind: "fragment"; id: string }
  | { kind: "external"; url: string }
  | { kind: "file"; path: string; fragment: string | null }
  | { kind: "refused"; reason: string };

/** Where the rendered note lives; `null` for bodies that are not files. */
export interface MarkdownNote {
  /** Absolute repository root. */
  repoPath: string;
  /** Repository-relative POSIX path of the note. */
  path: string;
}

const SCHEME = /^[A-Za-z][A-Za-z0-9+.-]*:/;

const REFUSED_SCHEME =
  "GitPulse opens http, https and mailto links, and files inside this repository.";

/**
 * Decides what activating `href` in a rendered note may do.
 *
 * `#` alone is what the renderer leaves where it refused a destination
 * (`javascript:`, `data:`, an unknown scheme), so it reads as refused rather
 * than as a jump to the top. Relative paths resolve against the note's own
 * folder, `/path` against the repository root — GitHub's reading of both —
 * and either is refused if it climbs out of the repository.
 */
export function resolveMarkdownLink(href: string, note: MarkdownNote | null): MarkdownLinkAction {
  const raw = href.trim();
  if (!raw || raw === "#") return { kind: "refused", reason: "This link has no destination GitPulse can open." };
  if (raw.length > MAX_URL_LENGTH) return { kind: "refused", reason: "This link is too long to open." };
  if (raw.startsWith("#")) {
    const id = decode(raw.slice(1));
    return id ? { kind: "fragment", id } : { kind: "refused", reason: "This link names no section." };
  }
  if (isOpenableUrl(raw) || isMailto(raw)) return { kind: "external", url: raw };
  if (SCHEME.test(raw) || raw.startsWith("//") || raw.startsWith("\\")) {
    return { kind: "refused", reason: REFUSED_SCHEME };
  }
  if (!note) return { kind: "refused", reason: "A relative link has nothing to resolve against here." };

  const hash = raw.indexOf("#");
  const beforeHash = hash < 0 ? raw : raw.slice(0, hash);
  const fragment = hash < 0 ? null : decode(raw.slice(hash + 1)) || null;
  const pathPart = beforeHash.split("?")[0];
  const decoded = decode(pathPart);
  if (decoded === null) return { kind: "refused", reason: "This link is not a valid path." };
  if (!decoded) {
    // `?query` or `?q#frag` on the note itself.
    return fragment ? { kind: "fragment", id: fragment } : { kind: "refused", reason: "This link names no file." };
  }
  if (decoded.endsWith("/")) return { kind: "refused", reason: `${decoded} is a folder; GitPulse opens files.` };

  const folder = note.path.includes("/") ? note.path.slice(0, note.path.lastIndexOf("/")) : "";
  const target = decoded.startsWith("/") ? decoded.slice(1) : folder ? `${folder}/${decoded}` : decoded;
  const resolved = resolveRepoFile(note.repoPath, { path: target, line: null, column: null });
  if (!resolved) {
    return { kind: "refused", reason: `${decoded} is outside this repository — GitPulse only opens files inside it.` };
  }
  return { kind: "file", path: resolved.path, fragment };
}

function isMailto(url: string): boolean {
  try {
    return new URL(url).protocol === "mailto:";
  } catch {
    return false;
  }
}

/** Percent-decodes, or `null` for a malformed sequence or a NUL. */
function decode(value: string): string | null {
  let decoded: string;
  try {
    decoded = decodeURIComponent(value);
  } catch {
    return null;
  }
  return decoded.includes("\0") ? null : decoded;
}

/** The element a fragment names inside `container`, by its unprefixed id. */
export function findFragmentTarget(container: ParentNode, id: string): Element | null {
  const wanted = `${MARKDOWN_ID_PREFIX}${id}`;
  // A scan rather than `querySelector("#…")`: an id may start with a digit
  // (footnote `1`) or hold characters a selector would have to escape.
  for (const element of container.querySelectorAll("[id]")) {
    if (element.id === wanted) return element;
  }
  return null;
}

export interface MarkdownClickContext {
  container: ParentNode;
  note: MarkdownNote | null;
  openExternal: (url: string) => Promise<void>;
  openFile: (path: string, fragment: string | null) => void;
  copy: (text: string) => Promise<boolean>;
  /** Says why a click did nothing — silence reads as a click that missed. */
  report: (message: string) => void;
}

/** Duck-typed so it also runs where `Element` is not a global (unit tests). */
function closest(target: EventTarget | null, selector: string): Element | null {
  const candidate = target as { closest?: (s: string) => Element | null } | null;
  return typeof candidate?.closest === "function" ? candidate.closest(selector) : null;
}

/**
 * Handles a click (or middle click) inside rendered Markdown. Every link is
 * claimed — the webview never follows one — and resolved by
 * {@link resolveMarkdownLink}.
 */
export function handleMarkdownClick(event: MouseEvent, context: MarkdownClickContext): void {
  const copyButton = closest(event.target, `.${COPY_CLASS}`);
  if (copyButton) {
    event.preventDefault();
    void copyCode(copyButton, context);
    return;
  }
  const anchor = closest(event.target, "a[href]");
  if (!anchor) return;
  event.preventDefault();
  const action = resolveMarkdownLink(anchor.getAttribute("href") ?? "", context.note);
  switch (action.kind) {
    case "fragment": {
      const target = findFragmentTarget(context.container, action.id);
      if (target) target.scrollIntoView({ behavior: "smooth", block: "start" });
      else context.report(`This document has no section "${action.id}".`);
      return;
    }
    case "external":
      void context.openExternal(action.url).catch((error: unknown) => {
        context.report(`Could not open ${action.url}: ${error instanceof Error ? error.message : String(error)}`);
      });
      return;
    case "file":
      context.openFile(action.path, action.fragment);
      return;
    case "refused":
      context.report(action.reason);
  }
}

const COPY_CLASS = "gp-md-copy";
const CODE_FRAME_CLASS = "gp-md-code";
const COPY_RESET_MS = 1800;

async function copyCode(button: Element, context: MarkdownClickContext): Promise<void> {
  const block = button.parentElement?.querySelector("pre");
  const text = block?.textContent ?? "";
  if (!text) return;
  if (!(await context.copy(text))) {
    context.report("Could not copy to clipboard");
    return;
  }
  if (!button.isConnected) return;
  button.textContent = "Copied";
  button.setAttribute("data-copied", "");
  setTimeout(() => {
    button.textContent = "Copy";
    button.removeAttribute("data-copied");
  }, COPY_RESET_MS);
}

/**
 * Prepares rendered Markdown HTML for the app's document, before it is
 * inserted — so an unprefixed id never exists there, not even for a frame:
 *  - namespaces every id with {@link MARKDOWN_ID_PREFIX};
 *  - gives each code block a copy button (the renderer emits plain `<pre>`);
 *  - replaces each picture that was neither embedded nor https with its alt
 *    text, so no path ever resolves against the app's own origin;
 *  - titles each link with where it goes, since the status bar a browser
 *    would show does not exist here.
 *
 * The work happens in an inert `<template>`, where nothing loads or runs.
 * Idempotent: preparing prepared HTML changes nothing.
 */
export function prepareRenderedMarkdown(html: string, note: MarkdownNote | null, doc: Document = document): string {
  if (!html) return "";
  const template = doc.createElement("template");
  template.innerHTML = html;
  const root = template.content;
  const owner = root.ownerDocument;
  for (const element of root.querySelectorAll("[id]")) {
    if (!element.id.startsWith(MARKDOWN_ID_PREFIX)) element.id = `${MARKDOWN_ID_PREFIX}${element.id}`;
  }
  for (const block of root.querySelectorAll("pre")) {
    const parent = block.parentNode;
    if (!parent || block.parentElement?.classList.contains(CODE_FRAME_CLASS)) continue;
    const frame = owner.createElement("div");
    frame.className = CODE_FRAME_CLASS;
    const language = /\blanguage-([\w+#.-]{1,32})\b/.exec(block.querySelector("code")?.className ?? "")?.[1];
    if (language) frame.setAttribute("data-language", language);
    const button = owner.createElement("button");
    button.setAttribute("type", "button");
    button.className = COPY_CLASS;
    button.textContent = "Copy";
    button.setAttribute("aria-label", "Copy code");
    parent.insertBefore(frame, block);
    frame.append(button, block);
  }
  for (const picture of root.querySelectorAll("img")) {
    const src = picture.getAttribute("src") ?? "";
    if (/^(?:data:image\/|https:)/i.test(src)) continue;
    // The renderer leaves a picture it did not embed with its source path.
    // In the webview that path resolves against the APP's origin — a broken
    // icon at best, one of GitPulse's own assets at worst — so it is replaced
    // by its alt text and the reasons it may not have loaded. Which reason
    // applied is not reported by the renderer, so none is singled out.
    const stand = owner.createElement("span");
    stand.className = "gp-md-missing-picture";
    stand.textContent = picture.getAttribute("alt") || "Picture";
    stand.setAttribute(
      "title",
      /^http:/i.test(src)
        ? `Not loaded: ${src} is served over plain http.`
        : note
          ? `Not shown: ${src} is not a readable picture inside this repository, or is over the size limit.`
          : `Not shown: ${src} is a local picture, and this text is not a file in the repository.`,
    );
    picture.replaceWith(stand);
  }
  for (const anchor of root.querySelectorAll("a[href]")) {
    if (anchor.hasAttribute("title")) continue;
    const action = resolveMarkdownLink(anchor.getAttribute("href") ?? "", note);
    const title =
      action.kind === "external"
        ? `Open ${action.url} in your browser`
        : action.kind === "file"
          ? `Open ${action.path}`
          : action.kind === "refused"
            ? action.reason
            : "";
    if (title) anchor.setAttribute("title", title);
  }
  return template.innerHTML;
}
