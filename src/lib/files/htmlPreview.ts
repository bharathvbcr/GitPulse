import { get, writable } from "svelte/store";
import { logicalLineAt, type LinkBuffer } from "../terminal/links";
import {
  resolveRelativeRepoAsset,
  resolveTerminalFileCandidate,
  utf8ByteLength,
  type TerminalFileTarget,
} from "./previewPath";
import { extractSvgBlobText, svgBlobToDataUrl } from "./svgPreview";
import type { FileBlob } from "./types";

export { resolveRelativeRepoAsset, utf8ByteLength };

export type HtmlViewMode = "preview" | "split" | "code";
export type HtmlViewportPreset = "responsive" | "desktop" | "tablet" | "mobile";
export type HtmlCanvasSurface = "light" | "dark" | "grid";

export interface HtmlViewportSpec {
  readonly id: HtmlViewportPreset;
  readonly label: string;
  readonly width: number | null;
  readonly height: number | null;
}

export const HTML_VIEWPORT_PRESETS: readonly HtmlViewportSpec[] = Object.freeze([
  { id: "responsive", label: "Fit", width: null, height: null },
  { id: "desktop", label: "Desktop", width: 1280, height: 800 },
  { id: "tablet", label: "Tablet", width: 768, height: 1024 },
  { id: "mobile", label: "Mobile", width: 375, height: 667 },
]);

export const HTML_ZOOM_MIN = 50;
export const HTML_ZOOM_MAX = 200;
export const HTML_ZOOM_STEP = 25;

/** Maximum HTML document size rendered in live preview (512 KiB). */
export const MAX_HTML_PREVIEW_BYTES = 512 * 1024;
/** Maximum number of local stylesheets inlined per preview render. */
export const MAX_INLINED_STYLESHEETS = 8;
/** Maximum byte length of a single inlined local stylesheet (128 KiB). */
export const MAX_INLINED_CSS_BYTES = 128 * 1024;
/** Maximum number of local images inlined per preview render. */
export const MAX_INLINED_IMAGES = 24;
/** Maximum decoded byte length of a single inlined local image (256 KiB). */
export const MAX_INLINED_IMAGE_BYTES = 256 * 1024;

/** Maximum path length accepted for a mode-override navigation request. */
export const MAX_HTML_MODE_PATH = 1024;
/** How long a one-shot HTML mode request remains collectable. */
export const HTML_MODE_TTL_MS = 30_000;

/**
 * Defense-in-depth CSP injected inside the sandboxed `srcdoc` document.
 * Scripts, plugins, frames, form submissions, and arbitrary network fetches
 * are blocked; inline/inlined styles and local/HTTPS images remain permitted.
 */
export const HTML_PREVIEW_SANDBOX_CSP =
  "default-src 'none'; style-src 'unsafe-inline' data:; img-src data: blob: https:; font-src data:; media-src data: blob:; form-action 'none'; base-uri 'none'; frame-src 'none'; object-src 'none';";

/** Container width threshold for side-by-side vs stacked Split mode. */
export const HTML_SPLIT_HORIZONTAL_MIN_WIDTH = 620;
/** Container width threshold for inline vs stacked toolbar switcher. */
export const HTML_TOOLBAR_INLINE_MIN_WIDTH = 560;

export type HtmlSplitDirection = "horizontal" | "vertical";
export type HtmlToolbarLayout = "inline" | "stacked";
export type HtmlLayoutMode = "unmeasured" | "wide" | "compact";

export interface HtmlPreviewLayout {
  readonly mode: HtmlLayoutMode;
  readonly splitDirection: HtmlSplitDirection;
  readonly toolbar: HtmlToolbarLayout;
}

/** Resolves toolbar placement and split orientation from measured viewer width. */
export function resolveHtmlPreviewLayout(width: number): HtmlPreviewLayout {
  if (!Number.isFinite(width) || width <= 0) {
    return {
      mode: "unmeasured",
      splitDirection: "horizontal",
      toolbar: "inline",
    };
  }
  const splitDirection: HtmlSplitDirection =
    width >= HTML_SPLIT_HORIZONTAL_MIN_WIDTH ? "horizontal" : "vertical";
  const toolbar: HtmlToolbarLayout =
    width >= HTML_TOOLBAR_INLINE_MIN_WIDTH ? "inline" : "stacked";
  return {
    mode: splitDirection === "horizontal" && toolbar === "inline" ? "wide" : "compact",
    splitDirection,
    toolbar,
  };
}

export interface HtmlSplitLayoutChoice {
  readonly direction: HtmlSplitDirection;
  readonly canToggle: boolean;
}

/**
 * Resolves the effective Split mode orientation from the measured viewer width
 * and the user's preferred direction. Narrow panes always stack vertically;
 * wide panes allow toggling between side-by-side (`horizontal`) and stacked
 * (`vertical`) so full-width HTML layouts can be previewed cleanly while editing.
 */
export function resolveHtmlSplitDirection(
  width: number,
  preferred: HtmlSplitDirection = "horizontal",
): HtmlSplitLayoutChoice {
  const canToggle = resolveHtmlPreviewLayout(width).splitDirection === "horizontal";
  return {
    direction: canToggle ? preferred : "vertical",
    canToggle,
  };
}

/** True when `path` names an HTML document (`.html`, `.htm`, or `.xhtml`). */
export function isHtmlFile(path: string | null | undefined): boolean {
  if (!path) return false;
  const trimmed = path.trim();
  if (!trimmed) return false;
  const clean = trimmed.split(/[?#]/, 1)[0];
  const lower = clean.toLowerCase();
  const base = lower.slice(Math.max(lower.lastIndexOf("/"), lower.lastIndexOf("\\")) + 1);
  return (
    (base.endsWith(".html") && base.length > 5) ||
    (base.endsWith(".htm") && base.length > 4) ||
    (base.endsWith(".xhtml") && base.length > 6)
  );
}

export interface HtmlViewModeRequest {
  readonly repo: string | null;
  readonly path: string;
  readonly fallbackPath?: string;
  readonly mode: HtmlViewMode;
  readonly at: number;
  readonly nonce: number;
}

const pendingMode = writable<HtmlViewModeRequest | null>(null);
let nextModeNonce = 1;

/** Subscribe-only view so mounted viewers react to right-click open requests. */
export const htmlViewModeRequest = { subscribe: pendingMode.subscribe };

function normalizeRepoKey(repo: string | null | undefined): string | null {
  if (!repo) return null;
  const normalized = repo.trim().replace(/[\\/]+$/, "");
  return normalized || null;
}

function normalizeRelativePath(path: string): string {
  return path.trim().replace(/\\/g, "/").replace(/^\.\//, "");
}

/**
 * Records a one-shot request to open `path` in `mode` (`preview`, `split`, or `code`).
 * Carries a monotonic nonce so repeated right-clicks on an already-open file still apply.
 */
export function requestHtmlViewMode(
  repo: string | null | undefined,
  path: string,
  mode: HtmlViewMode,
  now: number = Date.now(),
  fallbackPath?: string,
): boolean {
  const cleanPath = normalizeRelativePath(path);
  if (!cleanPath || cleanPath.length > MAX_HTML_MODE_PATH || !isHtmlFile(cleanPath)) return false;
  const cleanFallback = fallbackPath ? normalizeRelativePath(fallbackPath) : undefined;
  pendingMode.set({
    repo: normalizeRepoKey(repo),
    path: cleanPath,
    ...(cleanFallback && cleanFallback !== cleanPath && isHtmlFile(cleanFallback)
      ? { fallbackPath: cleanFallback }
      : {}),
    mode,
    at: now,
    nonce: nextModeNonce++,
  });
  return true;
}

/** True when `request` is unexpired and names `(repo, path)`. */
export function peekHtmlViewModeMatch(
  request: HtmlViewModeRequest | null,
  repo: string | null | undefined,
  path: string | null | undefined,
  now: number = Date.now(),
): boolean {
  if (!request || !path) return false;
  if (now - request.at > HTML_MODE_TTL_MS) return false;
  const cleanPath = normalizeRelativePath(path);
  if (request.path !== cleanPath && request.fallbackPath !== cleanPath) return false;
  const cleanRepo = normalizeRepoKey(repo);
  if (request.repo && cleanRepo && request.repo !== cleanRepo) return false;
  return true;
}

/**
 * Non-destructively matches a mode request for `(repo, path)` and returns its
 * `{ mode, nonce }`. Mounted viewers track their own `lastHandledNonce` so
 * concurrently mounted hosts (`FileViewer` + `TerminalFilePane`) never steal
 * a right-click request from one another while one is still loading its blob.
 */
export function matchHtmlViewMode(
  request: HtmlViewModeRequest | null,
  repo: string | null | undefined,
  path: string | null | undefined,
  now: number = Date.now(),
): { readonly mode: HtmlViewMode; readonly nonce: number } | null {
  if (!request) return null;
  if (now - request.at > HTML_MODE_TTL_MS) return null;
  if (!peekHtmlViewModeMatch(request, repo, path, now)) return null;
  return { mode: request.mode, nonce: request.nonce };
}

/** Consumes the pending mode request if it matches `(repo, path)` and has not expired. */
export function consumeHtmlViewMode(
  repo: string | null | undefined,
  path: string,
  now: number = Date.now(),
): HtmlViewMode | null {
  const current = get(pendingMode);
  if (!current) return null;
  if (now - current.at > HTML_MODE_TTL_MS) {
    pendingMode.set(null);
    return null;
  }
  if (!peekHtmlViewModeMatch(current, repo, path, now)) return null;
  pendingMode.set(null);
  return current.mode;
}

/**
 * Clears the pending mode request when it targets `(repo, path)`.
 * Called when the user explicitly selects a mode button or a line-reveal
 * overrides preview so a superseded right-click request cannot replay later.
 */
export function clearHtmlViewModeIfMatches(
  repo: string | null | undefined,
  path: string | null | undefined,
  now: number = Date.now(),
): void {
  const current = get(pendingMode);
  if (!current) return;
  if (now - current.at > HTML_MODE_TTL_MS || peekHtmlViewModeMatch(current, repo, path, now)) {
    pendingMode.set(null);
  }
}

/** Clears any pending HTML mode request. For teardown and tests. */
export function clearHtmlViewMode(): void {
  pendingMode.set(null);
}

const MAX_REMEMBERED_MODES = 64;
const rememberedModes = new Map<string, HtmlViewMode>();

function rememberedModeKey(repo: string | null | undefined, path: string): string {
  return `${normalizeRepoKey(repo) ?? ""}\0${normalizeRelativePath(path)}`;
}

/** Remembers the user's chosen view mode for `(repo, path)` within this session. */
export function rememberHtmlViewMode(
  repo: string | null | undefined,
  path: string,
  mode: HtmlViewMode,
): void {
  if (!path) return;
  const key = rememberedModeKey(repo, path);
  if (rememberedModes.has(key)) rememberedModes.delete(key);
  rememberedModes.set(key, mode);
  if (rememberedModes.size > MAX_REMEMBERED_MODES) {
    const oldest = rememberedModes.keys().next().value;
    if (oldest !== undefined) rememberedModes.delete(oldest);
  }
}

/** Returns the last remembered view mode for `(repo, path)`, if any. */
export function recalledHtmlViewMode(
  repo: string | null | undefined,
  path: string,
): HtmlViewMode | null {
  if (!path) return null;
  return rememberedModes.get(rememberedModeKey(repo, path)) ?? null;
}

/** Clears remembered HTML view modes. For tests. */
export function clearRememberedHtmlViewModes(): void {
  rememberedModes.clear();
}

export interface HtmlDocumentStats {
  readonly title: string | null;
  readonly byteCount: number;
  readonly lineCount: number;
  readonly hasDoctype: boolean;
  readonly scriptCount: number;
  readonly inlineStyleCount: number;
  readonly localStylesheetRefs: readonly string[];
  readonly localImageRefs: readonly string[];
  readonly externalStylesheetCount: number;
  readonly externalScriptCount: number;
}

export interface PreparedHtmlDocument {
  readonly srcdoc: string;
  readonly stats: HtmlDocumentStats;
  readonly tooLarge: boolean;
  readonly omittedBytes: number;
  readonly inlinedStylesheetCount: number;
  readonly inlinedImageCount: number;
}

export interface PrepareHtmlOptions {
  readonly canvas?: HtmlCanvasSurface;
  /** Map from raw HTML `href` attribute value to stylesheet CSS text. */
  readonly inlinedStylesheets?: Readonly<Record<string, string>>;
  /** Map from raw HTML `src` or inline CSS `url(...)` value to `data:` URI. */
  readonly inlinedImages?: Readonly<Record<string, string>>;
}

export interface InlinedAssetResult {
  readonly stylesheets: Record<string, string>;
  readonly images: Record<string, string>;
  readonly inlinedStylesheetCount: number;
  readonly inlinedImageCount: number;
  readonly missingAssets: readonly string[];
  readonly skippedCapCount: number;
}

const SCHEME_RE = /^[A-Za-z][A-Za-z0-9+.-]*:/;
const EXTERNAL_URL_RE = /^(?:https?:|\/\/)/i;

function decodeBasicEntities(text: string): string {
  return text
    .replace(/&amp;/gi, "&")
    .replace(/&lt;/gi, "<")
    .replace(/&gt;/gi, ">")
    .replace(/&quot;/gi, '"')
    .replace(/&#39;|&apos;/gi, "'")
    .replace(/&nbsp;/gi, " ");
}

function escapeAttrValue(value: string): string {
  return value
    .replace(/&/g, "&amp;")
    .replace(/"/g, "&quot;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;");
}

function sanitizeInlinedCss(css: string): string {
  return css.replace(/<\/(style)/gi, "<\\/$1");
}

function isLocalRef(raw: string): boolean {
  const value = raw.trim();
  if (!value || value.startsWith("#")) return false;
  if (EXTERNAL_URL_RE.test(value)) return false;
  if (SCHEME_RE.test(value) || value.startsWith("\\")) return false;
  return true;
}

// ---------------------------------------------------------------------------
// Linear, comment- and quote-aware HTML & CSS scanners (zero regex backtracking)
// ---------------------------------------------------------------------------

interface ParsedAttr {
  readonly name: string;
  readonly value: string;
  /** Start of attribute name inside the HTML string. */
  readonly nameStart: number;
  /** Start of attribute value token (including quotes, if any), or -1 if boolean attr. */
  readonly valueStart: number;
  /** Exclusive end of attribute value token (including closing quote), or name end. */
  readonly valueEnd: number;
}

type HtmlToken =
  | { readonly kind: "comment"; readonly start: number; readonly end: number }
  | { readonly kind: "doctype"; readonly start: number; readonly end: number }
  | {
      readonly kind: "script";
      readonly start: number;
      readonly end: number;
      readonly attrs: readonly ParsedAttr[];
    }
  | {
      readonly kind: "style";
      readonly start: number;
      readonly end: number;
      readonly contentStart: number;
      readonly contentEnd: number;
      readonly attrs: readonly ParsedAttr[];
    }
  | {
      readonly kind: "rcdata";
      readonly name: "title" | "textarea";
      readonly start: number;
      readonly end: number;
      readonly contentStart: number;
      readonly contentEnd: number;
      readonly attrs: readonly ParsedAttr[];
    }
  | {
      readonly kind: "tag";
      readonly start: number;
      readonly end: number;
      readonly name: string;
      readonly isClosing: boolean;
      readonly selfClosing: boolean;
      readonly attrs: readonly ParsedAttr[];
    };

function isAsciiSpace(code: number): boolean {
  return code === 0x20 || code === 0x09 || code === 0x0a || code === 0x0d || code === 0x0c;
}

function isTagNameStart(code: number): boolean {
  return (code >= 65 && code <= 90) || (code >= 97 && code <= 122);
}

function isTagNameChar(code: number): boolean {
  return (
    isTagNameStart(code) ||
    (code >= 48 && code <= 57) ||
    code === 45 || // -
    code === 58 // :
  );
}

function isAttrNameStop(code: number): boolean {
  return (
    isAsciiSpace(code) ||
    code === 61 || // =
    code === 62 || // >
    code === 47 || // /
    code === 34 || // "
    code === 39 // '
  );
}

/**
 * Finds the closing `>` of an opening tag starting at `from`, skipping over
 * double- and single-quoted attribute values so `alt="a > b"` never terminates
 * the tag prematurely.
 */
function findTagEnd(html: string, from: number): number {
  let quote = 0;
  for (let i = from; i < html.length; i++) {
    const code = html.charCodeAt(i);
    if (quote !== 0) {
      if (code === quote) quote = 0;
      continue;
    }
    if (code === 34 || code === 39) {
      quote = code;
    } else if (code === 62) {
      return i;
    }
  }
  return -1;
}

/**
 * Parses attributes between `start` (right after tag name) and `end` (index of `>`).
 * Exact attribute names are extracted from whitespace/separator boundaries so
 * `data-src` is never confused with `src`.
 */
function parseTagAttributes(html: string, start: number, end: number): ParsedAttr[] {
  const attrs: ParsedAttr[] = [];
  let i = start;
  while (i < end) {
    while (i < end && (isAsciiSpace(html.charCodeAt(i)) || html.charCodeAt(i) === 47)) i++;
    if (i >= end) break;
    const nameStart = i;
    while (i < end && !isAttrNameStop(html.charCodeAt(i))) i++;
    if (i === nameStart) {
      i++;
      continue;
    }
    const name = html.slice(nameStart, i).toLowerCase();
    let j = i;
    while (j < end && isAsciiSpace(html.charCodeAt(j))) j++;
    if (j < end && html.charCodeAt(j) === 61) {
      j++;
      while (j < end && isAsciiSpace(html.charCodeAt(j))) j++;
      if (j < end && (html.charCodeAt(j) === 34 || html.charCodeAt(j) === 39)) {
        const quote = html.charCodeAt(j);
        const valueStart = j;
        j++;
        const innerStart = j;
        while (j < end && html.charCodeAt(j) !== quote) j++;
        const innerEnd = j;
        if (j < end && html.charCodeAt(j) === quote) j++;
        attrs.push({
          name,
          value: decodeBasicEntities(html.slice(innerStart, innerEnd)).trim(),
          nameStart,
          valueStart,
          valueEnd: j,
        });
        i = j;
      } else {
        const valueStart = j;
        while (j < end && !isAsciiSpace(html.charCodeAt(j))) j++;
        // Strip trailing `/` on unquoted self-closing attribute like `<img src=a.png/>`.
        let valueEnd = j;
        if (valueEnd === end && valueEnd > valueStart && html.charCodeAt(valueEnd - 1) === 47) {
          valueEnd -= 1;
        }
        attrs.push({
          name,
          value: decodeBasicEntities(html.slice(valueStart, valueEnd)).trim(),
          nameStart,
          valueStart,
          valueEnd,
        });
        i = j;
      }
    } else {
      attrs.push({
        name,
        value: "",
        nameStart,
        valueStart: -1,
        valueEnd: i,
      });
      i = j;
    }
  }
  return attrs;
}

function getAttr(attrs: readonly ParsedAttr[], name: string): ParsedAttr | undefined {
  return attrs.find((attr) => attr.name === name);
}

function findRawTextClose(
  html: string,
  lower: string,
  tagName: string,
  from: number,
): { readonly contentEnd: number; readonly blockEnd: number } {
  const len = html.length;
  const marker = `</${tagName}`;
  const markerLen = marker.length;
  let searchFrom = from;
  while (searchFrom < len) {
    const closeIdx = lower.indexOf(marker, searchFrom);
    if (closeIdx < 0) {
      return { contentEnd: len, blockEnd: len };
    }
    const afterClose = lower[closeIdx + markerLen];
    if (
      afterClose === undefined ||
      isAsciiSpace(afterClose.charCodeAt(0)) ||
      afterClose === ">" ||
      afterClose === "/"
    ) {
      const closeGt = html.indexOf(">", closeIdx + markerLen);
      return { contentEnd: closeIdx, blockEnd: closeGt < 0 ? len : closeGt + 1 };
    }
    searchFrom = closeIdx + markerLen;
  }
  return { contentEnd: len, blockEnd: len };
}

/**
 * Single-pass linear HTML scanner that skips comments and respects quoted
 * attributes, producing tokens for comments, doctype, `<script>`, `<style>`,
 * RCDATA (`<title>`/`<textarea>` outside SVG/MathML foreign content), and
 * standard tags without regex backtracking.
 */
function scanHtmlTokens(html: string): HtmlToken[] {
  const tokens: HtmlToken[] = [];
  const lower = html.toLowerCase();
  const len = html.length;
  let foreignDepth = 0;
  let i = 0;

  while (i < len) {
    const lt = html.indexOf("<", i);
    if (lt < 0) break;

    // HTML comment: <!-- ... -->
    if (html.startsWith("<!--", lt)) {
      const close = html.indexOf("-->", lt + 4);
      const end = close < 0 ? len : close + 3;
      tokens.push({ kind: "comment", start: lt, end });
      i = end;
      continue;
    }

    // DOCTYPE declaration: <!doctype html ...>
    if (lower.startsWith("<!doctype", lt)) {
      const next = lower[lt + 9];
      if (next === undefined || isAsciiSpace(next.charCodeAt(0)) || next === ">") {
        const gt = html.indexOf(">", lt + 9);
        const end = gt < 0 ? len : gt + 1;
        tokens.push({ kind: "doctype", start: lt, end });
        i = end;
        continue;
      }
    }

    // Closing tag: </name ...>
    if (lt + 1 < len && html.charCodeAt(lt + 1) === 47) {
      let nameIdx = lt + 2;
      if (nameIdx < len && isTagNameStart(html.charCodeAt(nameIdx))) {
        let nameEnd = nameIdx + 1;
        while (nameEnd < len && isTagNameChar(html.charCodeAt(nameEnd))) nameEnd++;
        const gt = html.indexOf(">", nameEnd);
        const end = gt < 0 ? len : gt + 1;
        const name = lower.slice(nameIdx, nameEnd);
        if ((name === "svg" || name === "math") && foreignDepth > 0) {
          foreignDepth -= 1;
        }
        tokens.push({
          kind: "tag",
          start: lt,
          end,
          name,
          isClosing: true,
          selfClosing: false,
          attrs: [],
        });
        i = end;
        continue;
      }
      i = lt + 2;
      continue;
    }

    // Opening tag: <name ...>
    if (lt + 1 < len && isTagNameStart(html.charCodeAt(lt + 1))) {
      let nameEnd = lt + 2;
      while (nameEnd < len && isTagNameChar(html.charCodeAt(nameEnd))) nameEnd++;
      const name = lower.slice(lt + 1, nameEnd);
      const afterName = html.charCodeAt(nameEnd);
      if (
        nameEnd < len &&
        !isAsciiSpace(afterName) &&
        afterName !== 62 &&
        afterName !== 47
      ) {
        i = nameEnd;
        continue;
      }
      const gt = findTagEnd(html, nameEnd);
      if (gt < 0) {
        if (name === "script") {
          tokens.push({ kind: "script", start: lt, end: len, attrs: [] });
        }
        break;
      }
      let prevNonSpace = gt - 1;
      while (prevNonSpace >= nameEnd && isAsciiSpace(html.charCodeAt(prevNonSpace))) {
        prevNonSpace -= 1;
      }
      const selfClosing = prevNonSpace >= nameEnd && html.charCodeAt(prevNonSpace) === 47;
      // Exclude the self-closing `/` from attribute parsing so an unquoted value
      // followed by `/ >` never swallows `/` past the insertion point.
      const attrEnd = selfClosing ? prevNonSpace : gt;
      const attrs = parseTagAttributes(html, nameEnd, attrEnd);

      if (name === "script") {
        if (selfClosing) {
          tokens.push({ kind: "script", start: lt, end: gt + 1, attrs });
          i = gt + 1;
        } else {
          const { blockEnd } = findRawTextClose(html, lower, "script", gt + 1);
          tokens.push({ kind: "script", start: lt, end: blockEnd, attrs });
          i = blockEnd;
        }
        continue;
      }

      if (name === "style" && !selfClosing) {
        const { contentEnd, blockEnd } = findRawTextClose(html, lower, "style", gt + 1);
        tokens.push({
          kind: "style",
          start: lt,
          end: blockEnd,
          contentStart: gt + 1,
          contentEnd,
          attrs,
        });
        i = blockEnd;
        continue;
      }

      if ((name === "title" || name === "textarea") && !selfClosing && foreignDepth === 0) {
        const { contentEnd, blockEnd } = findRawTextClose(html, lower, name, gt + 1);
        tokens.push({
          kind: "rcdata",
          name,
          start: lt,
          end: blockEnd,
          contentStart: gt + 1,
          contentEnd,
          attrs,
        });
        i = blockEnd;
        continue;
      }

      if ((name === "svg" || name === "math") && !selfClosing) {
        foreignDepth += 1;
      }

      tokens.push({
        kind: "tag",
        start: lt,
        end: gt + 1,
        name,
        isClosing: false,
        selfClosing,
        attrs,
      });
      i = gt + 1;
      continue;
    }

    i = lt + 1;
  }

  return tokens;
}

interface CssUrlSpan {
  /** Start of `url(` in `css`. */
  readonly start: number;
  /** Exclusive end after `)`. */
  readonly end: number;
  /** Unquoted, trimmed URL string. */
  readonly rawRef: string;
}

/**
 * Linearly scans CSS for `url(...)` references outside CSS comments `/* ... *\/`.
 * Only local refs (relative or repo-root paths) are returned.
 */
export function scanLocalCssUrlSpans(css: string): CssUrlSpan[] {
  const spans: CssUrlSpan[] = [];
  const lower = css.toLowerCase();
  const len = css.length;
  let i = 0;

  while (i < len) {
    // Skip CSS block comments `/* ... */`
    if (css.charCodeAt(i) === 47 && css.charCodeAt(i + 1) === 42) {
      const close = css.indexOf("*/", i + 2);
      i = close < 0 ? len : close + 2;
      continue;
    }
    // Skip quoted strings so `content: "url(foo.png)"` is not rewritten
    const ch = css.charCodeAt(i);
    if (ch === 34 || ch === 39) {
      i++;
      while (i < len) {
        const c = css.charCodeAt(i);
        if (c === 92) {
          i += 2;
          continue;
        }
        if (c === ch) {
          i++;
          break;
        }
        i++;
      }
      continue;
    }
    if (
      lower.startsWith("url(", i) &&
      (i === 0 || !/[a-z0-9_-]/i.test(css[i - 1]))
    ) {
      let j = i + 4;
      while (j < len && isAsciiSpace(css.charCodeAt(j))) j++;
      if (j >= len) break;
      let raw = "";
      let closeParen = -1;
      const q = css.charCodeAt(j);
      if (q === 34 || q === 39) {
        j++;
        const innerStart = j;
        while (j < len) {
          const c = css.charCodeAt(j);
          if (c === 92) {
            j += 2;
            continue;
          }
          if (c === q) break;
          j++;
        }
        raw = css.slice(innerStart, j).trim();
        if (j < len && css.charCodeAt(j) === q) j++;
        while (j < len && isAsciiSpace(css.charCodeAt(j))) j++;
        if (j < len && css.charCodeAt(j) === 41) closeParen = j;
      } else {
        const innerStart = j;
        while (j < len && css.charCodeAt(j) !== 41 && !isAsciiSpace(css.charCodeAt(j))) j++;
        raw = css.slice(innerStart, j).trim();
        while (j < len && isAsciiSpace(css.charCodeAt(j))) j++;
        if (j < len && css.charCodeAt(j) === 41) closeParen = j;
      }
      if (closeParen > i) {
        if (isLocalRef(raw)) {
          spans.push({ start: i, end: closeParen + 1, rawRef: raw });
        }
        i = closeParen + 1;
        continue;
      }
    }
    i++;
  }

  return spans;
}

function rewriteLocalCssUrls(
  css: string,
  urlMap: Readonly<Record<string, string>>,
): { readonly css: string; readonly count: number } {
  const spans = scanLocalCssUrlSpans(css);
  if (spans.length === 0) return { css, count: 0 };
  let out = "";
  let cursor = 0;
  let count = 0;
  for (const span of spans) {
    if (!Object.prototype.hasOwnProperty.call(urlMap, span.rawRef)) continue;
    const dataUri = urlMap[span.rawRef];
    out += css.slice(cursor, span.start);
    // Double-quote safely; escape any `"` or `\` or newlines just in case.
    const safeUri = dataUri.replace(/["\\\r\n]/g, (c) => `\\${c}`);
    out += `url("${safeUri}")`;
    cursor = span.end;
    count += 1;
  }
  if (count === 0) return { css, count: 0 };
  out += css.slice(cursor);
  return { css: out, count };
}

/** Inspects an HTML string for title, resource references, and sandbox-relevant counts. */
export function analyzeHtmlDocument(html: string): HtmlDocumentStats {
  const byteCount = utf8ByteLength(html);
  if (!html) {
    return {
      title: null,
      byteCount: 0,
      lineCount: 0,
      hasDoctype: false,
      scriptCount: 0,
      inlineStyleCount: 0,
      localStylesheetRefs: [],
      localImageRefs: [],
      externalStylesheetCount: 0,
      externalScriptCount: 0,
    };
  }

  const lineCount = html.split(/\r?\n/).length;
  const tokens = scanHtmlTokens(html);

  let hasDoctype = false;
  let title: string | null = null;
  let scriptCount = 0;
  let externalScriptCount = 0;
  let inlineStyleCount = 0;
  let externalStylesheetCount = 0;
  const localStylesheets = new Set<string>();
  const localImages = new Set<string>();

  for (let idx = 0; idx < tokens.length; idx++) {
    const tok = tokens[idx];
    if (tok.kind === "comment") continue;
    if (tok.kind === "doctype") {
      hasDoctype = true;
      continue;
    }
    if (tok.kind === "script") {
      scriptCount += 1;
      const src = getAttr(tok.attrs, "src")?.value;
      if (src && EXTERNAL_URL_RE.test(src)) externalScriptCount += 1;
      continue;
    }
    if (tok.kind === "style") {
      inlineStyleCount += 1;
      const css = html.slice(tok.contentStart, tok.contentEnd);
      for (const span of scanLocalCssUrlSpans(css)) {
        localImages.add(span.rawRef);
      }
      continue;
    }
    if (tok.kind === "rcdata") {
      if (tok.name === "title" && title === null) {
        const rawTitle = decodeBasicEntities(html.slice(tok.contentStart, tok.contentEnd))
          .replace(/\s+/g, " ")
          .trim();
        if (rawTitle) title = rawTitle.slice(0, 512);
      }
      const inlineStyleAttr = getAttr(tok.attrs, "style")?.value;
      if (inlineStyleAttr) {
        for (const span of scanLocalCssUrlSpans(inlineStyleAttr)) {
          localImages.add(span.rawRef);
        }
      }
      continue;
    }
    if (tok.kind === "tag" && !tok.isClosing) {
      if (tok.name === "link") {
        const rel = getAttr(tok.attrs, "rel")?.value.toLowerCase() ?? "";
        if (!rel.split(/\s+/).includes("stylesheet")) continue;
        const href = getAttr(tok.attrs, "href")?.value;
        if (!href) continue;
        if (EXTERNAL_URL_RE.test(href)) {
          externalStylesheetCount += 1;
        } else if (isLocalRef(href)) {
          localStylesheets.add(href);
        }
      } else if (tok.name === "img") {
        const src = getAttr(tok.attrs, "src")?.value;
        if (src && isLocalRef(src)) localImages.add(src);
      }
      const inlineStyleAttr = getAttr(tok.attrs, "style")?.value;
      if (inlineStyleAttr) {
        for (const span of scanLocalCssUrlSpans(inlineStyleAttr)) {
          localImages.add(span.rawRef);
        }
      }
    }
  }

  return {
    title,
    byteCount,
    lineCount,
    hasDoctype,
    scriptCount,
    inlineStyleCount,
    localStylesheetRefs: [...localStylesheets],
    localImageRefs: [...localImages],
    externalStylesheetCount,
    externalScriptCount,
  };
}

interface TextEdit {
  readonly start: number;
  readonly end: number;
  readonly replacement: string;
}

function applyTextEdits(source: string, edits: readonly TextEdit[]): string {
  if (edits.length === 0) return source;
  const sorted = [...edits].sort((a, b) => a.start - b.start);
  let out = "";
  let cursor = 0;
  for (const edit of sorted) {
    if (edit.start < cursor) continue;
    out += source.slice(cursor, edit.start);
    out += edit.replacement;
    cursor = edit.end;
  }
  out += source.slice(cursor);
  return out;
}

/**
 * Prepares untrusted repository HTML for rendering inside a strictly sandboxed
 * `<iframe srcdoc>` by enforcing a size cap, stripping scripts/refresh/base tags
 * outside HTML comments, substituting pre-loaded local stylesheets and images,
 * containing external `<a href>` navigations, and injecting a strict CSP into
 * the real `<head>` (never inside an HTML comment).
 */
export function prepareHtmlPreviewDocument(
  rawHtml: string,
  options: PrepareHtmlOptions = {},
): PreparedHtmlDocument {
  const stats = analyzeHtmlDocument(rawHtml);
  if (stats.byteCount > MAX_HTML_PREVIEW_BYTES) {
    return {
      srcdoc: "",
      stats,
      tooLarge: true,
      omittedBytes: stats.byteCount - MAX_HTML_PREVIEW_BYTES,
      inlinedStylesheetCount: 0,
      inlinedImageCount: 0,
    };
  }

  const stylesheets = options.inlinedStylesheets ?? {};
  const images = options.inlinedImages ?? {};
  const canvas = options.canvas ?? "light";

  const tokens = scanHtmlTokens(rawHtml);
  const edits: TextEdit[] = [];
  let inlinedStylesheetCount = 0;
  let inlinedImageCount = 0;
  let realHeadInsertAt: number | null = null;
  let realHtmlInsertAt: number | null = null;

  for (const tok of tokens) {
    if (tok.kind === "comment" || tok.kind === "doctype") continue;

    if (tok.kind === "script") {
      edits.push({ start: tok.start, end: tok.end, replacement: "" });
      continue;
    }

    if (tok.kind === "style") {
      const css = rawHtml.slice(tok.contentStart, tok.contentEnd);
      const rewritten = rewriteLocalCssUrls(css, images);
      if (rewritten.count > 0) {
        inlinedImageCount += rewritten.count;
        edits.push({
          start: tok.contentStart,
          end: tok.contentEnd,
          replacement: sanitizeInlinedCss(rewritten.css),
        });
      }
      continue;
    }

    if (tok.kind === "rcdata") {
      const styleAttr = getAttr(tok.attrs, "style");
      if (styleAttr && styleAttr.valueStart >= 0) {
        const rewritten = rewriteLocalCssUrls(styleAttr.value, images);
        if (rewritten.count > 0) {
          inlinedImageCount += rewritten.count;
          edits.push({
            start: styleAttr.valueStart,
            end: styleAttr.valueEnd,
            replacement: `"${escapeAttrValue(rewritten.css)}"`,
          });
        }
      }
      continue;
    }

    if (tok.kind === "tag" && !tok.isClosing) {
      if (tok.name === "head" && realHeadInsertAt === null) {
        realHeadInsertAt = tok.end;
      } else if (tok.name === "html" && realHtmlInsertAt === null) {
        realHtmlInsertAt = tok.end;
      } else if (tok.name === "base") {
        edits.push({ start: tok.start, end: tok.end, replacement: "" });
        continue;
      } else if (tok.name === "meta") {
        const httpEquiv = getAttr(tok.attrs, "http-equiv")?.value.toLowerCase();
        if (httpEquiv === "refresh") {
          edits.push({ start: tok.start, end: tok.end, replacement: "" });
          continue;
        }
      } else if (tok.name === "link") {
        const rel = getAttr(tok.attrs, "rel")?.value.toLowerCase() ?? "";
        if (rel.split(/\s+/).includes("stylesheet")) {
          const href = getAttr(tok.attrs, "href")?.value;
          if (href && Object.prototype.hasOwnProperty.call(stylesheets, href)) {
            const css = stylesheets[href];
            inlinedStylesheetCount += 1;
            edits.push({
              start: tok.start,
              end: tok.end,
              replacement: `<style data-gitpulse-inlined="${escapeAttrValue(href)}">\n${sanitizeInlinedCss(css)}\n</style>`,
            });
            continue;
          }
        }
      } else if (tok.name === "img") {
        const srcAttr = getAttr(tok.attrs, "src");
        if (
          srcAttr &&
          srcAttr.valueStart >= 0 &&
          Object.prototype.hasOwnProperty.call(images, srcAttr.value)
        ) {
          const dataUri = images[srcAttr.value];
          inlinedImageCount += 1;
          edits.push({
            start: srcAttr.valueStart,
            end: srcAttr.valueEnd,
            replacement: `"${escapeAttrValue(dataUri)}"`,
          });
        }
      } else if (tok.name === "a" || tok.name === "area") {
        // Keep in-page `#fragment` links scrolling inside the preview, while
        // routing external/relative/empty links (including `<area href>` and
        // SVG `<a xlink:href>`) to `target="_blank"` with
        // `rel="noopener noreferrer"` so the sandbox (no `allow-popups`)
        // blocks frame hijacking instead of navigating the preview iframe
        // away to an untrusted URL or blank `about:srcdoc` page.
        const hrefAttr =
          getAttr(tok.attrs, "href") ??
          (tok.name === "a" ? getAttr(tok.attrs, "xlink:href") : undefined);
        if (hrefAttr && !hrefAttr.value.startsWith("#")) {
          let appendAttrs = "";

          const targetAttrs = tok.attrs.filter((attr) => attr.name === "target");
          if (targetAttrs.length > 0) {
            const [firstTarget, ...extraTargets] = targetAttrs;
            edits.push({
              start: firstTarget.nameStart,
              end: firstTarget.valueEnd,
              replacement: 'target="_blank"',
            });
            for (const dup of extraTargets) {
              edits.push({ start: dup.nameStart, end: dup.valueEnd, replacement: "" });
            }
          } else {
            appendAttrs += ' target="_blank"';
          }

          const relAttrs = tok.attrs.filter((attr) => attr.name === "rel");
          if (relAttrs.length > 0) {
            const [firstRel, ...extraRels] = relAttrs;
            const existingTokens = firstRel.value
              ? firstRel.value.split(/\s+/).filter(Boolean)
              : [];
            const seenLower = new Set(existingTokens.map((token) => token.toLowerCase()));
            const mergedTokens = [...existingTokens];
            if (!seenLower.has("noopener")) mergedTokens.push("noopener");
            if (!seenLower.has("noreferrer")) mergedTokens.push("noreferrer");
            if (firstRel.valueStart < 0 || mergedTokens.length !== existingTokens.length) {
              edits.push({
                start: firstRel.nameStart,
                end: firstRel.valueEnd,
                replacement: `rel="${escapeAttrValue(mergedTokens.join(" "))}"`,
              });
            }
            for (const dup of extraRels) {
              edits.push({ start: dup.nameStart, end: dup.valueEnd, replacement: "" });
            }
          } else {
            appendAttrs += ' rel="noopener noreferrer"';
          }

          if (appendAttrs) {
            let insertAt = tok.end - 1;
            if (tok.selfClosing) {
              let slash = tok.end - 2;
              while (slash > tok.start && isAsciiSpace(rawHtml.charCodeAt(slash))) slash -= 1;
              insertAt = slash;
            }
            edits.push({
              start: insertAt,
              end: insertAt,
              replacement: appendAttrs,
            });
          }
        }
      }

      // Rewrite local `url(...)` inside inline `style="..."` attributes too.
      const styleAttr = getAttr(tok.attrs, "style");
      if (styleAttr && styleAttr.valueStart >= 0) {
        const rewritten = rewriteLocalCssUrls(styleAttr.value, images);
        if (rewritten.count > 0) {
          inlinedImageCount += rewritten.count;
          edits.push({
            start: styleAttr.valueStart,
            end: styleAttr.valueEnd,
            replacement: `"${escapeAttrValue(rewritten.css)}"`,
          });
        }
      }
    }
  }

  const cspTag = `<meta http-equiv="Content-Security-Policy" content="${HTML_PREVIEW_SANDBOX_CSP}">`;
  const canvasRule =
    canvas === "dark"
      ? `<style data-gitpulse-canvas>:root{color-scheme:dark;}</style>`
      : `<style data-gitpulse-canvas>:root{color-scheme:light;}</style>`;
  const injectedHeadPrefix = `${cspTag}${canvasRule}`;

  let doc: string;
  if (realHeadInsertAt !== null) {
    edits.push({
      start: realHeadInsertAt,
      end: realHeadInsertAt,
      replacement: `\n${injectedHeadPrefix}\n`,
    });
    doc = applyTextEdits(rawHtml, edits);
  } else if (realHtmlInsertAt !== null) {
    edits.push({
      start: realHtmlInsertAt,
      end: realHtmlInsertAt,
      replacement: `\n<head>${injectedHeadPrefix}</head>\n`,
    });
    doc = applyTextEdits(rawHtml, edits);
  } else {
    const body = applyTextEdits(rawHtml, edits);
    doc = `<!DOCTYPE html>\n<html><head><meta charset="utf-8">${injectedHeadPrefix}</head><body>\n${body}\n</body></html>`;
  }

  return {
    srcdoc: doc,
    stats,
    tooLarge: false,
    omittedBytes: 0,
    inlinedStylesheetCount,
    inlinedImageCount,
  };
}

type ImageReadOutcome =
  | { readonly kind: "ok"; readonly dataUri: string }
  | { readonly kind: "cap" }
  | { readonly kind: "missing" };

async function readImageDataUri(
  repoPath: string,
  resolvedPath: string,
  readBlob: (repoPath: string, filePath: string) => Promise<FileBlob | null>,
  cache: Map<string, Promise<ImageReadOutcome>>,
): Promise<ImageReadOutcome> {
  const existing = cache.get(resolvedPath);
  if (existing) return existing;
  const promise = (async (): Promise<ImageReadOutcome> => {
    try {
      const blob = await readBlob(repoPath, resolvedPath);
      if (!blob) return { kind: "missing" };
      const lower = resolvedPath.toLowerCase();
      if (lower.endsWith(".svg") || blob.mime === "image/svg+xml") {
        if (blob.base64 && Math.floor((blob.base64.length * 3) / 4) > MAX_INLINED_IMAGE_BYTES) {
          return { kind: "cap" };
        }
        const extracted = extractSvgBlobText(blob);
        if (extracted.hasNul || extracted.invalidBase64 || !extracted.text.trim()) {
          return { kind: "missing" };
        }
        if (utf8ByteLength(extracted.text) > MAX_INLINED_IMAGE_BYTES) return { kind: "cap" };
        const dataUri = svgBlobToDataUrl(blob);
        if (!dataUri) return { kind: "missing" };
        return { kind: "ok", dataUri };
      }
      if (blob.base64) {
        const approxBytes = Math.floor((blob.base64.length * 3) / 4);
        if (approxBytes > MAX_INLINED_IMAGE_BYTES) return { kind: "cap" };
        const mime =
          blob.mime && blob.mime.startsWith("image/")
            ? blob.mime
            : "application/octet-stream";
        if (!mime.startsWith("image/")) return { kind: "missing" };
        return { kind: "ok", dataUri: `data:${mime};base64,${blob.base64}` };
      }
      return { kind: "missing" };
    } catch {
      return { kind: "missing" };
    }
  })();
  cache.set(resolvedPath, promise);
  return promise;
}

/**
 * Reads bounded repository-local CSS and image assets referenced by `rawHtml`
 * (including `url(...)` images inside inlined stylesheets and `<style>` blocks)
 * so multi-file HTML pages render faithfully inside the sandboxed `srcdoc`.
 */
export async function inlineLocalHtmlAssets(
  rawHtml: string,
  location: { readonly repoPath: string; readonly filePath: string },
  readBlob: (repoPath: string, filePath: string) => Promise<FileBlob | null>,
): Promise<InlinedAssetResult> {
  const stats = analyzeHtmlDocument(rawHtml);
  const stylesheets: Record<string, string> = {};
  const images: Record<string, string> = {};
  const missingAssets: string[] = [];
  let skippedCapCount = 0;

  if (stats.byteCount > MAX_HTML_PREVIEW_BYTES || !location.repoPath || !location.filePath) {
    return {
      stylesheets,
      images,
      inlinedStylesheetCount: 0,
      inlinedImageCount: 0,
      missingAssets,
      skippedCapCount,
    };
  }

  const cssRefs = stats.localStylesheetRefs.slice(0, MAX_INLINED_STYLESHEETS);
  skippedCapCount += Math.max(0, stats.localStylesheetRefs.length - cssRefs.length);

  const imgRefs = stats.localImageRefs.slice(0, MAX_INLINED_IMAGES);
  skippedCapCount += Math.max(0, stats.localImageRefs.length - imgRefs.length);
  let remainingImageBudget = Math.max(0, MAX_INLINED_IMAGES - imgRefs.length);

  const imageCache = new Map<string, Promise<ImageReadOutcome>>();
  const loadedCss: Array<{ rawRef: string; resolvedPath: string; text: string }> = [];

  await Promise.all([
    ...cssRefs.map(async (rawRef) => {
      const resolved = resolveRelativeRepoAsset(location.repoPath, location.filePath, rawRef);
      if (!resolved) {
        missingAssets.push(rawRef);
        return;
      }
      try {
        const blob = await readBlob(location.repoPath, resolved);
        if (!blob || blob.is_binary || typeof blob.text !== "string") {
          missingAssets.push(rawRef);
          return;
        }
        if (utf8ByteLength(blob.text) > MAX_INLINED_CSS_BYTES) {
          skippedCapCount += 1;
          return;
        }
        loadedCss.push({ rawRef, resolvedPath: resolved, text: blob.text });
      } catch {
        missingAssets.push(rawRef);
      }
    }),
    ...imgRefs.map(async (rawRef) => {
      const resolved = resolveRelativeRepoAsset(location.repoPath, location.filePath, rawRef);
      if (!resolved) {
        missingAssets.push(rawRef);
        return;
      }
      const outcome = await readImageDataUri(location.repoPath, resolved, readBlob, imageCache);
      if (outcome.kind === "ok") {
        images[rawRef] = outcome.dataUri;
      } else if (outcome.kind === "cap") {
        skippedCapCount += 1;
      } else {
        missingAssets.push(rawRef);
      }
    }),
  ]);

  let cssInlinedImageCount = 0;
  for (const sheet of loadedCss) {
    const cssUrlRefs = [...new Set(scanLocalCssUrlSpans(sheet.text).map((s) => s.rawRef))];
    if (cssUrlRefs.length === 0) {
      stylesheets[sheet.rawRef] = sheet.text;
      continue;
    }
    const allowedRefs = cssUrlRefs.slice(0, remainingImageBudget);
    skippedCapCount += Math.max(0, cssUrlRefs.length - allowedRefs.length);
    remainingImageBudget = Math.max(0, remainingImageBudget - allowedRefs.length);

    const sheetUrlMap: Record<string, string> = {};
    await Promise.all(
      allowedRefs.map(async (cssUrlRef) => {
        const resolved = resolveRelativeRepoAsset(location.repoPath, sheet.resolvedPath, cssUrlRef);
        if (!resolved) {
          missingAssets.push(cssUrlRef);
          return;
        }
        const outcome = await readImageDataUri(location.repoPath, resolved, readBlob, imageCache);
        if (outcome.kind === "ok") {
          sheetUrlMap[cssUrlRef] = outcome.dataUri;
        } else if (outcome.kind === "cap") {
          skippedCapCount += 1;
        } else {
          missingAssets.push(cssUrlRef);
        }
      }),
    );

    const rewritten = rewriteLocalCssUrls(sheet.text, sheetUrlMap);
    cssInlinedImageCount += Object.keys(sheetUrlMap).length;
    stylesheets[sheet.rawRef] = rewritten.css;
  }

  return {
    stylesheets,
    images,
    inlinedStylesheetCount: Object.keys(stylesheets).length,
    inlinedImageCount: Object.keys(images).length + cssInlinedImageCount,
    missingAssets,
    skippedCapCount,
  };
}

export type TerminalHtmlTarget = TerminalFileTarget;

const TERMINAL_TOKEN_DELIMITERS = new Set([
  '"', "'", "`", "<", ">", "|", "(", ")", "[", "]", "{", "}", ",", ";", "*", "?", "=", "&",
]);

function isTerminalTokenChar(ch: string): boolean {
  const code = ch.charCodeAt(0);
  if (code <= 0x20 || code === 0x7f || (code >= 0x80 && code <= 0x9f)) return false;
  return !TERMINAL_TOKEN_DELIMITERS.has(ch);
}

/**
 * Extracts the path-like token spanning 0-based `colIndex` in a terminal line
 * (e.g. `index.html` or `./docs/page.html:12` in `ls` or build output) so
 * right-clicking directly on an HTML filename works without drag-selecting first.
 */
export function extractTerminalTokenAtColumn(
  lineText: string | null | undefined,
  colIndex: number,
): string | null {
  if (!lineText || !Number.isInteger(colIndex) || colIndex < 0 || colIndex >= lineText.length) {
    return null;
  }
  if (!isTerminalTokenChar(lineText[colIndex])) return null;
  let start = colIndex;
  while (start > 0 && isTerminalTokenChar(lineText[start - 1])) start -= 1;
  let end = colIndex + 1;
  while (end < lineText.length && isTerminalTokenChar(lineText[end])) end += 1;
  while (end > start) {
    const tail = lineText[end - 1];
    if (tail === "." || tail === ":" || tail === "!") {
      end -= 1;
      continue;
    }
    break;
  }
  if (end <= start || end - start > MAX_HTML_MODE_PATH) return null;
  return lineText.slice(start, end);
}

function isLikelyWideGlyph(glyph: string): boolean {
  const cp = glyph.codePointAt(0);
  if (cp === undefined || cp < 0x1100) return false;
  return (
    (cp >= 0x1100 && cp <= 0x115f) ||
    cp === 0x2329 ||
    cp === 0x232a ||
    (cp >= 0x2e80 && cp <= 0x303e) ||
    (cp >= 0x3040 && cp <= 0xa4cf) ||
    (cp >= 0xac00 && cp <= 0xd7a3) ||
    (cp >= 0xf900 && cp <= 0xfaff) ||
    (cp >= 0xfe10 && cp <= 0xfe19) ||
    (cp >= 0xfe30 && cp <= 0xfe6f) ||
    (cp >= 0xff00 && cp <= 0xff60) ||
    (cp >= 0xffe0 && cp <= 0xffe6) ||
    cp >= 0x1f300
  );
}

/**
 * Extracts the path-like token strictly under 1-based terminal cell `(row1, col1)`.
 * Respects wrapped lines and double-width glyphs, and returns `null` when `col1`
 * falls in blank space past the end of a shorter row rather than clamping to the
 * row's last character.
 */
export function extractTerminalTokenAtCell(
  buffer: LinkBuffer,
  row1: number,
  col1: number,
): string | null {
  if (!Number.isInteger(row1) || row1 < 1 || !Number.isInteger(col1) || col1 < 1) {
    return null;
  }
  const logical = logicalLineAt(buffer, row1);
  if (!logical.text || logical.cells.length === 0) return null;
  const rowWidths = buffer.row(row1)?.widths;

  let rowLocalIndex = 0;
  let charIndex = -1;
  let i = 0;
  while (i < logical.cells.length) {
    const cell = logical.cells[i];
    if (cell.y !== row1) {
      i += 1;
      continue;
    }
    const start = i;
    const startRowLocal = rowLocalIndex;
    while (
      i < logical.cells.length &&
      logical.cells[i].y === row1 &&
      logical.cells[i].x === cell.x
    ) {
      i += 1;
      rowLocalIndex += 1;
    }
    const nextSameRow =
      i < logical.cells.length && logical.cells[i].y === row1 ? logical.cells[i] : null;
    const explicitWidth = rowWidths?.[startRowLocal];
    let cellWidth: number;
    if (
      typeof explicitWidth === "number" &&
      Number.isInteger(explicitWidth) &&
      explicitWidth >= 1 &&
      explicitWidth <= 2
    ) {
      cellWidth = explicitWidth;
    } else if (nextSameRow && nextSameRow.x > cell.x && nextSameRow.x - cell.x <= 2) {
      cellWidth = nextSameRow.x - cell.x;
    } else {
      cellWidth = isLikelyWideGlyph(logical.text.slice(start, i)) ? 2 : 1;
    }
    if (col1 >= cell.x && col1 < cell.x + cellWidth) {
      charIndex = start;
      break;
    }
    if (cell.x > col1) break;
  }

  if (charIndex < 0) return null;
  return extractTerminalTokenAtColumn(logical.text, charIndex);
}

/**
 * True when 1-based terminal cell `(row1, col1)` lies within an inclusive
 * xterm `IBufferRange` (supporting single-row and wrapped multi-row spans).
 * Prevents a stale hovered-link reference from firing when the user right-clicks
 * elsewhere (such as blank space after buffer scroll).
 */
export function isCellInTerminalRange(
  range: { readonly start: { readonly x: number; readonly y: number }; readonly end: { readonly x: number; readonly y: number } } | null | undefined,
  row1: number,
  col1: number,
): boolean {
  if (!range || !Number.isInteger(row1) || !Number.isInteger(col1) || row1 < 1 || col1 < 1) {
    return false;
  }
  const { start, end } = range;
  if (row1 < start.y || row1 > end.y) return false;
  if (row1 === start.y && col1 < start.x) return false;
  if (row1 === end.y && col1 > end.x) return false;
  return true;
}

/**
 * Resolves a hovered link, cursor token, or selected terminal token to a
 * repository-contained HTML file target for the terminal viewport context menu.
 */
export function resolveTerminalHtmlCandidate(
  rawText: string | null | undefined,
  workPath: string,
  activeCwdSubdir?: string | null,
): TerminalHtmlTarget | null {
  return resolveTerminalFileCandidate(
    rawText,
    workPath,
    isHtmlFile,
    MAX_HTML_MODE_PATH,
    activeCwdSubdir,
  );
}
