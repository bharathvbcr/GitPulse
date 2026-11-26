import { get, writable } from "svelte/store";
import {
  EXTERNAL_RESOURCE_URL_RE,
  isLocalAssetRef,
  normalizeRepoRelativePath,
  resolveRelativeRepoAsset,
  resolveTerminalFileCandidate,
  utf8ByteLength,
  type TerminalFileTarget,
} from "./previewPath";
import type { FileBlob } from "./types";

export type SvgViewMode = "preview" | "split" | "code";
export type SvgCanvasSurface = "grid" | "dark-grid" | "light" | "dark";
export type SvgSplitDirection = "horizontal" | "vertical";
export type SvgToolbarLayout = "inline" | "stacked";
export type SvgLayoutMode = "unmeasured" | "wide" | "compact";
export type SvgDimensionSource = "attributes" | "viewBox" | "default";
export type SvgPreviewShortcutAction = "zoom-in" | "zoom-out" | "fit" | "actual" | "bounds";
export type TerminalSvgTarget = TerminalFileTarget;

export const SVG_ZOOM_STEPS: readonly number[] = Object.freeze([
  25, 50, 75, 100, 125, 150, 200, 300, 400, 600, 800, 1200, 1600,
]);
export const SVG_ZOOM_MIN = 25;
export const SVG_ZOOM_MAX = 1600;

/** Maximum SVG document size rendered in live preview (1 MiB). */
export const MAX_SVG_PREVIEW_BYTES = 1024 * 1024;
/** Maximum number of local raster/SVG images inlined inside an SVG preview. */
export const MAX_SVG_INLINED_IMAGES = 16;
/** Maximum decoded byte length of a single inlined local image (256 KiB). */
export const MAX_SVG_INLINED_IMAGE_BYTES = 256 * 1024;

/** Maximum path length accepted for a mode-override navigation request. */
export const MAX_SVG_MODE_PATH = 1024;
/** How long a one-shot SVG mode request remains collectable. */
export const SVG_MODE_TTL_MS = 30_000;

/** Container width threshold for side-by-side vs stacked Split mode. */
export const SVG_SPLIT_HORIZONTAL_MIN_WIDTH = 620;
/** Container width threshold for inline vs stacked toolbar switcher. */
export const SVG_TOOLBAR_INLINE_MIN_WIDTH = 560;

const DEFAULT_SVG_WIDTH = 300;
const DEFAULT_SVG_HEIGHT = 150;
const MAX_FIT_AUTO_SCALE = 16;
const LIGHT_CANVAS_INK = "#0f172a";
const DARK_CANVAS_INK = "#f1f5f9";

export interface SvgPreviewLayout {
  readonly mode: SvgLayoutMode;
  readonly splitDirection: SvgSplitDirection;
  readonly toolbar: SvgToolbarLayout;
}

/** Resolves toolbar placement and split orientation from measured viewer width. */
export function resolveSvgPreviewLayout(width: number): SvgPreviewLayout {
  if (!Number.isFinite(width) || width <= 0) {
    return {
      mode: "unmeasured",
      splitDirection: "horizontal",
      toolbar: "inline",
    };
  }
  const splitDirection: SvgSplitDirection =
    width >= SVG_SPLIT_HORIZONTAL_MIN_WIDTH ? "horizontal" : "vertical";
  const toolbar: SvgToolbarLayout =
    width >= SVG_TOOLBAR_INLINE_MIN_WIDTH ? "inline" : "stacked";
  return {
    mode: splitDirection === "horizontal" && toolbar === "inline" ? "wide" : "compact",
    splitDirection,
    toolbar,
  };
}

export interface SvgSplitLayoutChoice {
  readonly direction: SvgSplitDirection;
  readonly canToggle: boolean;
}

/**
 * Resolves the effective Split mode orientation from the measured viewer width
 * and the user's preferred direction. Narrow panes always stack vertically;
 * wide panes allow toggling between side-by-side (`horizontal`) and stacked
 * (`vertical`).
 */
export function resolveSvgSplitDirection(
  width: number,
  preferred: SvgSplitDirection = "horizontal",
): SvgSplitLayoutChoice {
  const canToggle = resolveSvgPreviewLayout(width).splitDirection === "horizontal";
  return {
    direction: canToggle ? preferred : "vertical",
    canToggle,
  };
}

/** True when `path` names an uncompressed SVG document (`.svg`). */
export function isSvgFile(path: string | null | undefined): boolean {
  if (!path) return false;
  const trimmed = path.trim();
  if (!trimmed) return false;
  const clean = trimmed.split(/[?#]/, 1)[0];
  const lower = clean.toLowerCase();
  const base = lower.slice(Math.max(lower.lastIndexOf("/"), lower.lastIndexOf("\\")) + 1);
  return base.endsWith(".svg") && base.length > 4;
}

export interface SvgDisplaySize {
  readonly width: number;
  readonly height: number;
  readonly effectivePercent: number;
}

/**
 * Computes rendered SVG pixel dimensions for either Fit-to-stage or manual zoom.
 * Auto-fit scales up tiny icons (capped at 1600%) and scales down large artboards
 * while preserving aspect ratio.
 */
export function computeSvgDisplaySize(params: {
  readonly intrinsicWidth: number;
  readonly intrinsicHeight: number;
  readonly stageWidth: number;
  readonly stageHeight: number;
  readonly fit: boolean;
  readonly zoom: number;
  /** Space reserved inside `stageWidth` / `stageHeight` for surface chrome (padding + border). */
  readonly inset?: number;
}): SvgDisplaySize {
  const baseW = Number.isFinite(params.intrinsicWidth) && params.intrinsicWidth > 0
    ? params.intrinsicWidth
    : DEFAULT_SVG_WIDTH;
  const baseH = Number.isFinite(params.intrinsicHeight) && params.intrinsicHeight > 0
    ? params.intrinsicHeight
    : DEFAULT_SVG_HEIGHT;

  if (params.fit) {
    if (
      !Number.isFinite(params.stageWidth) ||
      params.stageWidth <= 0 ||
      !Number.isFinite(params.stageHeight) ||
      params.stageHeight <= 0
    ) {
      return {
        width: Math.max(1, Math.round(baseW)),
        height: Math.max(1, Math.round(baseH)),
        effectivePercent: 100,
      };
    }
    const inset =
      params.inset !== undefined && Number.isFinite(params.inset) && params.inset >= 0
        ? params.inset
        : 48;
    const availW = Math.max(1, params.stageWidth - inset);
    const availH = Math.max(1, params.stageHeight - inset);
    const rawScale = Math.min(availW / baseW, availH / baseH);
    const scale = Math.min(MAX_FIT_AUTO_SCALE, rawScale);
    return {
      width: Math.max(1, Math.floor(baseW * scale + 1e-9)),
      height: Math.max(1, Math.floor(baseH * scale + 1e-9)),
      effectivePercent: Math.max(1, Math.round(scale * 100)),
    };
  }

  const clampedZoom = Number.isFinite(params.zoom)
    ? Math.max(SVG_ZOOM_MIN, Math.min(SVG_ZOOM_MAX, params.zoom))
    : 100;
  const scale = clampedZoom / 100;
  return {
    width: Math.max(1, Math.round(baseW * scale)),
    height: Math.max(1, Math.round(baseH * scale)),
    effectivePercent: Math.round(clampedZoom),
  };
}

/** Steps `currentPercent` to the next higher or lower zoom stop in `SVG_ZOOM_STEPS`. */
export function stepSvgZoom(currentPercent: number, delta: 1 | -1): number {
  const current = Number.isFinite(currentPercent)
    ? Math.max(SVG_ZOOM_MIN, Math.min(SVG_ZOOM_MAX, currentPercent))
    : 100;
  if (delta > 0) {
    for (const stop of SVG_ZOOM_STEPS) {
      if (stop > current) return stop;
    }
    return SVG_ZOOM_MAX;
  }
  for (let i = SVG_ZOOM_STEPS.length - 1; i >= 0; i--) {
    if (SVG_ZOOM_STEPS[i] < current) return SVG_ZOOM_STEPS[i];
  }
  return SVG_ZOOM_MIN;
}

/** Maps an unmodified preview-pane keystroke to an SVG stage action, if any. */
export function resolveSvgPreviewShortcut(event: {
  readonly key: string;
  readonly ctrlKey: boolean;
  readonly metaKey: boolean;
  readonly altKey: boolean;
}): SvgPreviewShortcutAction | null {
  if (event.ctrlKey || event.metaKey || event.altKey) return null;
  switch (event.key) {
    case "+":
    case "=":
      return "zoom-in";
    case "-":
    case "_":
      return "zoom-out";
    case "0":
      return "fit";
    case "1":
      return "actual";
    case "b":
    case "B":
      return "bounds";
    default:
      return null;
  }
}

// ---------------------------------------------------------------------------
// Persisted SVG preview preferences (canvas surface, bounds, split direction)
// ---------------------------------------------------------------------------

export interface SvgPreviewPreferences {
  readonly canvasSurface: SvgCanvasSurface;
  readonly showBounds: boolean;
  readonly preferredSplitDirection: SvgSplitDirection;
}

const SVG_PREFS_STORAGE_KEY = "gitpulse_svg_preview_prefs";
const DEFAULT_SVG_PREFS: SvgPreviewPreferences = Object.freeze({
  canvasSurface: "grid",
  showBounds: false,
  preferredSplitDirection: "horizontal",
});

function isValidCanvasSurface(value: unknown): value is SvgCanvasSurface {
  return value === "grid" || value === "dark-grid" || value === "light" || value === "dark";
}

function isValidSplitDirection(value: unknown): value is SvgSplitDirection {
  return value === "horizontal" || value === "vertical";
}

/** Loads persisted SVG preview preferences, falling back cleanly on SSR/storage errors. */
export function loadSvgPreviewPreferences(): SvgPreviewPreferences {
  if (typeof localStorage === "undefined") return { ...DEFAULT_SVG_PREFS };
  try {
    const raw = localStorage.getItem(SVG_PREFS_STORAGE_KEY);
    if (!raw) return { ...DEFAULT_SVG_PREFS };
    const parsed: unknown = JSON.parse(raw);
    if (!parsed || typeof parsed !== "object") return { ...DEFAULT_SVG_PREFS };
    const obj = parsed as Record<string, unknown>;
    return {
      canvasSurface: isValidCanvasSurface(obj.canvasSurface)
        ? obj.canvasSurface
        : DEFAULT_SVG_PREFS.canvasSurface,
      showBounds: typeof obj.showBounds === "boolean" ? obj.showBounds : DEFAULT_SVG_PREFS.showBounds,
      preferredSplitDirection: isValidSplitDirection(obj.preferredSplitDirection)
        ? obj.preferredSplitDirection
        : DEFAULT_SVG_PREFS.preferredSplitDirection,
    };
  } catch {
    return { ...DEFAULT_SVG_PREFS };
  }
}

/** Merges and persists user-chosen SVG preview preferences. */
export function saveSvgPreviewPreferences(patch: Partial<SvgPreviewPreferences>): void {
  if (typeof localStorage === "undefined") return;
  try {
    const current = loadSvgPreviewPreferences();
    const next: SvgPreviewPreferences = {
      canvasSurface: isValidCanvasSurface(patch.canvasSurface)
        ? patch.canvasSurface
        : current.canvasSurface,
      showBounds: typeof patch.showBounds === "boolean" ? patch.showBounds : current.showBounds,
      preferredSplitDirection: isValidSplitDirection(patch.preferredSplitDirection)
        ? patch.preferredSplitDirection
        : current.preferredSplitDirection,
    };
    localStorage.setItem(SVG_PREFS_STORAGE_KEY, JSON.stringify(next));
  } catch {
    /* ignore quota / private-mode failures */
  }
}

/** Clears persisted SVG preview preferences. For tests. */
export function clearSvgPreviewPreferences(): void {
  if (typeof localStorage === "undefined") return;
  try {
    localStorage.removeItem(SVG_PREFS_STORAGE_KEY);
  } catch {
    /* ignore storage errors */
  }
}

/**
 * Resolves a hovered link, cursor token, or selected terminal token to a
 * repository-contained SVG file target for the terminal viewport context menu.
 */
export function resolveTerminalSvgCandidate(
  rawText: string | null | undefined,
  workPath: string,
  activeCwdSubdir?: string | null,
): TerminalSvgTarget | null {
  return resolveTerminalFileCandidate(rawText, workPath, isSvgFile, MAX_SVG_MODE_PATH, activeCwdSubdir);
}

// ---------------------------------------------------------------------------
// One-shot right-click mode request & per-session remembered view mode store
// ---------------------------------------------------------------------------

export interface SvgViewModeRequest {
  readonly repo: string | null;
  readonly path: string;
  readonly fallbackPath?: string;
  readonly mode: SvgViewMode;
  readonly at: number;
  readonly nonce: number;
}

const pendingMode = writable<SvgViewModeRequest | null>(null);
let nextModeNonce = 1;

/** Subscribe-only view so mounted viewers react to right-click open requests. */
export const svgViewModeRequest = { subscribe: pendingMode.subscribe };

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
export function requestSvgViewMode(
  repo: string | null | undefined,
  path: string,
  mode: SvgViewMode,
  now: number = Date.now(),
  fallbackPath?: string,
): boolean {
  const cleanPath = normalizeRelativePath(path);
  if (!cleanPath || cleanPath.length > MAX_SVG_MODE_PATH || !isSvgFile(cleanPath)) return false;
  const cleanFallback = fallbackPath ? normalizeRelativePath(fallbackPath) : undefined;
  pendingMode.set({
    repo: normalizeRepoKey(repo),
    path: cleanPath,
    ...(cleanFallback && cleanFallback !== cleanPath && isSvgFile(cleanFallback)
      ? { fallbackPath: cleanFallback }
      : {}),
    mode,
    at: now,
    nonce: nextModeNonce++,
  });
  return true;
}

/** True when `request` is unexpired and names `(repo, path)`. */
export function peekSvgViewModeMatch(
  request: SvgViewModeRequest | null,
  repo: string | null | undefined,
  path: string | null | undefined,
  now: number = Date.now(),
): boolean {
  if (!request || !path) return false;
  if (now - request.at > SVG_MODE_TTL_MS) return false;
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
export function matchSvgViewMode(
  request: SvgViewModeRequest | null,
  repo: string | null | undefined,
  path: string | null | undefined,
  now: number = Date.now(),
): { readonly mode: SvgViewMode; readonly nonce: number } | null {
  if (!request) return null;
  if (now - request.at > SVG_MODE_TTL_MS) return null;
  if (!peekSvgViewModeMatch(request, repo, path, now)) return null;
  return { mode: request.mode, nonce: request.nonce };
}

/** Consumes the pending mode request if it matches `(repo, path)` and has not expired. */
export function consumeSvgViewMode(
  repo: string | null | undefined,
  path: string,
  now: number = Date.now(),
): SvgViewMode | null {
  const current = get(pendingMode);
  if (!current) return null;
  if (now - current.at > SVG_MODE_TTL_MS) {
    pendingMode.set(null);
    return null;
  }
  if (!peekSvgViewModeMatch(current, repo, path, now)) return null;
  pendingMode.set(null);
  return current.mode;
}

/** Clears the pending mode request when it matches `(repo, path)` or has expired. */
export function clearSvgViewModeIfMatches(
  repo: string | null | undefined,
  path: string | null | undefined,
  now: number = Date.now(),
): void {
  const current = get(pendingMode);
  if (!current) return;
  if (now - current.at > SVG_MODE_TTL_MS || peekSvgViewModeMatch(current, repo, path, now)) {
    pendingMode.set(null);
  }
}

/** Clears any pending SVG mode request. For teardown and tests. */
export function clearSvgViewMode(): void {
  pendingMode.set(null);
}

const MAX_REMEMBERED_MODES = 64;
const rememberedModes = new Map<string, SvgViewMode>();

function rememberedModeKey(repo: string | null | undefined, path: string): string {
  return `${normalizeRepoKey(repo) ?? ""}\0${normalizeRelativePath(path)}`;
}

/** Remembers the user's chosen view mode for `(repo, path)` within this session. */
export function rememberSvgViewMode(
  repo: string | null | undefined,
  path: string,
  mode: SvgViewMode,
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
export function recalledSvgViewMode(
  repo: string | null | undefined,
  path: string,
): SvgViewMode | null {
  if (!path) return null;
  return rememberedModes.get(rememberedModeKey(repo, path)) ?? null;
}

/** Clears remembered SVG view modes. For tests. */
export function clearRememberedSvgViewModes(): void {
  rememberedModes.clear();
}

// ---------------------------------------------------------------------------
// Blob text extraction & base64 data URI encoding
// ---------------------------------------------------------------------------

export interface ExtractedSvgBlobText {
  readonly text: string;
  readonly fromBase64: boolean;
  readonly hasNul: boolean;
  readonly invalidBase64: boolean;
}

/**
 * Extracts UTF-8 SVG XML text from either a text-backed `FileBlob` or a
 * legacy/fixture base64-backed `FileBlob`. NUL-containing bytes and invalid
 * base64 payloads are flagged rather than passed to XML parsing.
 */
export function extractSvgBlobText(
  blob: { readonly text?: string | null; readonly base64?: string | null } | null | undefined,
): ExtractedSvgBlobText {
  if (!blob) return { text: "", fromBase64: false, hasNul: false, invalidBase64: false };
  if (typeof blob.text === "string") {
    return {
      text: blob.text,
      fromBase64: false,
      hasNul: blob.text.includes("\0"),
      invalidBase64: false,
    };
  }
  if (typeof blob.base64 === "string" && blob.base64.length > 0) {
    try {
      const binary = atob(blob.base64);
      const bytes = Uint8Array.from(binary, (ch) => ch.charCodeAt(0));
      const hasNul = bytes.includes(0);
      if (hasNul) {
        return { text: "", fromBase64: true, hasNul: true, invalidBase64: false };
      }
      const text = new TextDecoder("utf-8", { fatal: false }).decode(bytes);
      return { text, fromBase64: true, hasNul: false, invalidBase64: false };
    } catch {
      return { text: "", fromBase64: true, hasNul: false, invalidBase64: true };
    }
  }
  return { text: "", fromBase64: false, hasNul: false, invalidBase64: false };
}

/** Encodes UTF-8 SVG XML into a `data:image/svg+xml;base64,...` URI. */
export function encodeSvgDataUrl(svgXml: string): string {
  const bytes = new TextEncoder().encode(svgXml);
  let binary = "";
  for (let i = 0; i < bytes.length; i += 0x8000) {
    binary += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  }
  return `data:image/svg+xml;base64,${btoa(binary)}`;
}

// ---------------------------------------------------------------------------
// Linear, comment- and CDATA-aware XML/SVG scanner (zero regex backtracking)
// ---------------------------------------------------------------------------

interface ParsedAttr {
  readonly name: string;
  readonly rawName: string;
  readonly value: string;
  /** XML-entity-decoded value without whitespace trimming (for XML-strict namespace URI checks). */
  readonly rawValue: string;
  /** Start of attribute name inside the XML string. */
  readonly nameStart: number;
  /** Start of attribute value token (including quotes, if any), or -1 if boolean attr. */
  readonly valueStart: number;
  /** Exclusive end of attribute value token (including closing quote), or name end. */
  readonly valueEnd: number;
}

type XmlToken =
  | { readonly kind: "comment"; readonly start: number; readonly end: number }
  | { readonly kind: "cdata"; readonly start: number; readonly end: number }
  | {
      readonly kind: "pi";
      readonly start: number;
      readonly end: number;
      readonly isXmlStylesheet: boolean;
      readonly attrs: readonly ParsedAttr[];
    }
  | {
      readonly kind: "doctype";
      readonly start: number;
      readonly end: number;
      readonly hasInternalSubset: boolean;
    }
  | { readonly kind: "dtd-decl"; readonly start: number; readonly end: number }
  | {
      readonly kind: "script";
      readonly start: number;
      readonly end: number;
      readonly rawName: string;
      readonly attrs: readonly ParsedAttr[];
    }
  | {
      readonly kind: "style";
      readonly start: number;
      readonly end: number;
      readonly rawName: string;
      readonly contentStart: number;
      readonly contentEnd: number;
      readonly attrs: readonly ParsedAttr[];
    }
  | {
      readonly kind: "title";
      readonly start: number;
      readonly end: number;
      readonly rawName: string;
      readonly contentStart: number;
      readonly contentEnd: number;
      readonly depth: number;
      readonly attrs: readonly ParsedAttr[];
    }
  | {
      readonly kind: "desc";
      readonly start: number;
      readonly end: number;
      readonly rawName: string;
      readonly contentStart: number;
      readonly contentEnd: number;
      readonly depth: number;
      readonly attrs: readonly ParsedAttr[];
    }
  | {
      readonly kind: "tag";
      readonly start: number;
      readonly end: number;
      readonly name: string;
      readonly rawName: string;
      readonly localName: string;
      readonly rawLocalName: string;
      readonly prefix: string | null;
      readonly rawPrefix: string | null;
      readonly isClosing: boolean;
      readonly selfClosing: boolean;
      readonly attrs: readonly ParsedAttr[];
    }
  | {
      readonly kind: "unclosed-tag";
      readonly start: number;
      readonly end: number;
      readonly name: string;
      readonly rawName: string;
      readonly localName: string;
      readonly rawLocalName: string;
    };

const EXTERNAL_URL_RE = EXTERNAL_RESOURCE_URL_RE;

function isAsciiSpace(code: number): boolean {
  return code === 0x20 || code === 0x09 || code === 0x0a || code === 0x0d || code === 0x0c;
}

function isTagNameStart(code: number): boolean {
  return (
    (code >= 65 && code <= 90) ||
    (code >= 97 && code <= 122) ||
    code === 95 // _
  );
}

function isTagNameChar(code: number): boolean {
  return (
    isTagNameStart(code) ||
    (code >= 48 && code <= 57) ||
    code === 45 || // -
    code === 46 || // .
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

function splitTagName(name: string): { readonly prefix: string | null; readonly localName: string } {
  const colon = name.indexOf(":");
  if (colon <= 0) return { prefix: null, localName: name };
  return { prefix: name.slice(0, colon), localName: name.slice(colon + 1) };
}

function decodeXmlEntities(text: string): string {
  return text.replace(
    /&(#x[0-9a-fA-F]+|#[0-9]+|amp|lt|gt|quot|apos|nbsp);/gi,
    (full, entity: string) => {
      const lower = entity.toLowerCase();
      switch (lower) {
        case "amp":
          return "&";
        case "lt":
          return "<";
        case "gt":
          return ">";
        case "quot":
          return '"';
        case "apos":
          return "'";
        case "nbsp":
          return " ";
        default: {
          if (lower.startsWith("#x")) {
            const code = Number.parseInt(lower.slice(2), 16);
            if (Number.isInteger(code) && code > 0 && code <= 0x10ffff) {
              try {
                return String.fromCodePoint(code);
              } catch {
                return full;
              }
            }
          } else if (lower.startsWith("#")) {
            const code = Number.parseInt(lower.slice(1), 10);
            if (Number.isInteger(code) && code > 0 && code <= 0x10ffff) {
              try {
                return String.fromCodePoint(code);
              } catch {
                return full;
              }
            }
          }
          return full;
        }
      }
    },
  );
}

function escapeXmlAttrValue(value: string): string {
  return value
    .replace(/&/g, "&amp;")
    .replace(/"/g, "&quot;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;");
}

function isLocalRef(raw: string): boolean {
  return isLocalAssetRef(raw);
}

function isHrefLikeAttr(lowerName: string): boolean {
  return lowerName === "href" || lowerName.endsWith(":href");
}

function isActionOrHrefAttr(lowerName: string): boolean {
  return isHrefLikeAttr(lowerName) || lowerName === "action" || lowerName === "formaction";
}

function isUnsafeScriptHref(rawValue: string): boolean {
  const decoded = decodeXmlEntities(rawValue);
  let stripped = "";
  for (let i = 0; i < decoded.length; i++) {
    const code = decoded.charCodeAt(i);
    if (code > 0x20 && code !== 0x7f) {
      stripped += decoded[i];
    }
  }
  const lower = stripped.toLowerCase();
  return (
    lower.startsWith("javascript:") ||
    lower.startsWith("vbscript:") ||
    lower.startsWith("data:text/html") ||
    lower.startsWith("data:application/xhtml+xml") ||
    lower.startsWith("data:text/javascript") ||
    lower.startsWith("data:application/javascript") ||
    lower.startsWith("data:text/ecmascript") ||
    lower.startsWith("data:application/ecmascript")
  );
}

function isCssIdentChar(code: number): boolean {
  return (
    (code >= 65 && code <= 90) ||
    (code >= 97 && code <= 122) ||
    (code >= 48 && code <= 57) ||
    code === 45 || // -
    code === 95 || // _
    code >= 0x80
  );
}

function isHexDigit(code: number): boolean {
  return (
    (code >= 48 && code <= 57) ||
    (code >= 65 && code <= 70) ||
    (code >= 97 && code <= 102)
  );
}

/**
 * Decodes XML character references outside `<![CDATA[...]]>` blocks while
 * keeping CDATA sections verbatim, matching XML 1.0 `<style>` text parsing.
 */
function decodeStyleXmlText(raw: string): string {
  if (!raw.includes("<![CDATA[")) return decodeXmlEntities(raw);
  let out = "";
  let cursor = 0;
  while (cursor < raw.length) {
    const cdataStart = raw.indexOf("<![CDATA[", cursor);
    if (cdataStart < 0) {
      out += decodeXmlEntities(raw.slice(cursor));
      break;
    }
    if (cdataStart > cursor) {
      out += decodeXmlEntities(raw.slice(cursor, cdataStart));
    }
    const cdataClose = raw.indexOf("]]>", cdataStart + 9);
    if (cdataClose < 0) {
      out += raw.slice(cdataStart + 9);
      break;
    }
    out += raw.slice(cdataStart + 9, cdataClose);
    cursor = cdataClose + 3;
  }
  return out;
}

/** Consumes a CSS Syntax Level 3 escape sequence starting at `\` (`backslashIdx`). */
function readCssEscape(
  css: string,
  backslashIdx: number,
): { readonly char: string; readonly end: number } {
  const len = css.length;
  let i = backslashIdx + 1;
  if (i >= len) return { char: "\uFFFD", end: len };
  if (isHexDigit(css.charCodeAt(i))) {
    const hexStart = i;
    let count = 0;
    while (i < len && count < 6 && isHexDigit(css.charCodeAt(i))) {
      i += 1;
      count += 1;
    }
    const code = Number.parseInt(css.slice(hexStart, i), 16);
    if (i < len && isAsciiSpace(css.charCodeAt(i))) {
      if (css.charCodeAt(i) === 0x0d && i + 1 < len && css.charCodeAt(i + 1) === 0x0a) {
        i += 2;
      } else {
        i += 1;
      }
    }
    let ch = "\uFFFD";
    if (Number.isInteger(code) && code > 0 && code <= 0x10ffff) {
      try {
        ch = String.fromCodePoint(code);
      } catch {
        ch = "\uFFFD";
      }
    }
    return { char: ch, end: i };
  }
  return { char: css[i], end: i + 1 };
}

function skipCssWhitespaceAndComments(css: string, from: number): number {
  const len = css.length;
  let i = from;
  while (i < len) {
    if (isAsciiSpace(css.charCodeAt(i))) {
      i += 1;
      continue;
    }
    if (css.charCodeAt(i) === 47 && i + 1 < len && css.charCodeAt(i + 1) === 42) {
      const close = css.indexOf("*/", i + 2);
      i = close < 0 ? len : close + 2;
      continue;
    }
    break;
  }
  return i;
}

function readCssIdent(css: string, start: number): { readonly value: string; readonly end: number } {
  const len = css.length;
  let out = "";
  let i = start;
  while (i < len) {
    const code = css.charCodeAt(i);
    if (code === 92) {
      // Backslash escape inside identifier (`\75 rl` -> `url`, `@\69 mport` -> `@import`).
      const nextCode = i + 1 < len ? css.charCodeAt(i + 1) : -1;
      if (nextCode === 0x0a || nextCode === 0x0d || nextCode === 0x0c) break;
      const esc = readCssEscape(css, i);
      out += esc.char;
      i = esc.end;
      continue;
    }
    if (isCssIdentChar(code)) {
      out += css[i];
      i += 1;
      continue;
    }
    break;
  }
  return { value: out, end: i };
}

function readCssQuotedString(css: string, start: number): { readonly value: string; readonly end: number } {
  const quote = css.charCodeAt(start);
  let out = "";
  let i = start + 1;
  while (i < css.length) {
    const code = css.charCodeAt(i);
    if (code === 92 && i + 1 < css.length) {
      const nextCode = css.charCodeAt(i + 1);
      if (nextCode === 0x0a || nextCode === 0x0c) {
        i += 2;
        continue;
      }
      if (nextCode === 0x0d) {
        i += i + 2 < css.length && css.charCodeAt(i + 2) === 0x0a ? 3 : 2;
        continue;
      }
      const esc = readCssEscape(css, i);
      out += esc.char;
      i = esc.end;
      continue;
    }
    if (code === quote) {
      return { value: out, end: i + 1 };
    }
    out += css[i];
    i += 1;
  }
  return { value: out, end: css.length };
}

function readCssUrlArg(css: string, openParenIdx: number): { readonly value: string; readonly end: number } {
  let i = skipCssWhitespaceAndComments(css, openParenIdx + 1);
  if (i < css.length && (css.charCodeAt(i) === 34 || css.charCodeAt(i) === 39)) {
    const quoted = readCssQuotedString(css, i);
    let end = quoted.end;
    while (end < css.length && css.charCodeAt(end) !== 41) end++;
    if (end < css.length && css.charCodeAt(end) === 41) end++;
    return { value: quoted.value.trim(), end };
  }
  let out = "";
  while (i < css.length) {
    const code = css.charCodeAt(i);
    if (code === 92 && i + 1 < css.length) {
      const esc = readCssEscape(css, i);
      out += esc.char;
      i = esc.end;
      continue;
    }
    if (code === 41) {
      return { value: out.trim(), end: i + 1 };
    }
    out += css[i];
    i += 1;
  }
  return { value: out.trim(), end: css.length };
}

/**
 * Counts external `url(...)` and `@import` resource references inside `<style>`
 * blocks and inline `style="..."` attributes without backtracking regexes,
 * decoding CSS hex escapes and skipping comments and unrelated string literals.
 */
function countExternalCssRefs(css: string): number {
  const len = css.length;
  let count = 0;
  let i = 0;

  while (i < len) {
    const code = css.charCodeAt(i);
    if (code === 47 && i + 1 < len && css.charCodeAt(i + 1) === 42) {
      // /* ... */
      const close = css.indexOf("*/", i + 2);
      i = close < 0 ? len : close + 2;
      continue;
    }
    if (code === 34 || code === 39) {
      i = readCssQuotedString(css, i).end;
      continue;
    }
    if (code === 64) {
      // @-rule (handles CSS escapes like `@\69 mport`)
      const ident = readCssIdent(css, i + 1);
      if (ident.value.toLowerCase() === "import") {
        const j = skipCssWhitespaceAndComments(css, ident.end);
        if (j < len && (css.charCodeAt(j) === 34 || css.charCodeAt(j) === 39)) {
          const quoted = readCssQuotedString(css, j);
          if (EXTERNAL_URL_RE.test(quoted.value.trim())) count += 1;
          i = quoted.end;
          continue;
        }
        if (j < len && (isCssIdentChar(css.charCodeAt(j)) || css.charCodeAt(j) === 92)) {
          const fnIdent = readCssIdent(css, j);
          if (fnIdent.value.toLowerCase() === "url" && fnIdent.end < len && css.charCodeAt(fnIdent.end) === 40) {
            const parsed = readCssUrlArg(css, fnIdent.end);
            if (EXTERNAL_URL_RE.test(parsed.value)) count += 1;
            i = parsed.end;
            continue;
          }
          i = Math.max(j + 1, fnIdent.end);
          continue;
        }
        i = j;
        continue;
      }
      i = Math.max(i + 1, ident.end);
      continue;
    }
    if (isCssIdentChar(code) || code === 92) {
      const ident = readCssIdent(css, i);
      if (ident.value.toLowerCase() === "url" && ident.end < len && css.charCodeAt(ident.end) === 40) {
        const parsed = readCssUrlArg(css, ident.end);
        if (EXTERNAL_URL_RE.test(parsed.value)) count += 1;
        i = parsed.end;
        continue;
      }
      i = Math.max(i + 1, ident.end);
      continue;
    }
    i += 1;
  }

  return count;
}

function findTagEnd(xml: string, from: number): number {
  let quote = 0;
  for (let i = from; i < xml.length; i++) {
    const code = xml.charCodeAt(i);
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

function findDoctypeEnd(
  xml: string,
  from: number,
): { readonly end: number; readonly hasInternalSubset: boolean } {
  let quote = 0;
  let bracketDepth = 0;
  let hasInternalSubset = false;
  for (let i = from; i < xml.length; i++) {
    const code = xml.charCodeAt(i);
    if (quote !== 0) {
      if (code === quote) quote = 0;
      continue;
    }
    if (code === 34 || code === 39) {
      quote = code;
    } else if (code === 91) {
      // [
      bracketDepth += 1;
      hasInternalSubset = true;
    } else if (code === 93 && bracketDepth > 0) {
      // ]
      bracketDepth -= 1;
    } else if (code === 62 && bracketDepth === 0) {
      // >
      return { end: i + 1, hasInternalSubset };
    }
  }
  return { end: xml.length, hasInternalSubset };
}

function parseTagAttributes(xml: string, start: number, end: number): ParsedAttr[] {
  const attrs: ParsedAttr[] = [];
  let i = start;
  while (i < end) {
    while (i < end && (isAsciiSpace(xml.charCodeAt(i)) || xml.charCodeAt(i) === 47)) i++;
    if (i >= end) break;
    const nameStart = i;
    while (i < end && !isAttrNameStop(xml.charCodeAt(i))) i++;
    if (i === nameStart) {
      i++;
      continue;
    }
    const rawName = xml.slice(nameStart, i);
    const name = rawName.toLowerCase();
    let j = i;
    while (j < end && isAsciiSpace(xml.charCodeAt(j))) j++;
    if (j < end && xml.charCodeAt(j) === 61) {
      j++;
      while (j < end && isAsciiSpace(xml.charCodeAt(j))) j++;
      if (j < end && (xml.charCodeAt(j) === 34 || xml.charCodeAt(j) === 39)) {
        const quote = xml.charCodeAt(j);
        const valueStart = j;
        j++;
        const innerStart = j;
        while (j < end && xml.charCodeAt(j) !== quote) j++;
        const innerEnd = j;
        if (j < end && xml.charCodeAt(j) === quote) j++;
        const rawValue = decodeXmlEntities(xml.slice(innerStart, innerEnd));
        attrs.push({
          name,
          rawName,
          value: rawValue.trim(),
          rawValue,
          nameStart,
          valueStart,
          valueEnd: j,
        });
        i = j;
      } else {
        const valueStart = j;
        while (j < end && !isAsciiSpace(xml.charCodeAt(j))) j++;
        let valueEnd = j;
        if (valueEnd === end && valueEnd > valueStart && xml.charCodeAt(valueEnd - 1) === 47) {
          valueEnd -= 1;
        }
        const rawValue = decodeXmlEntities(xml.slice(valueStart, valueEnd));
        attrs.push({
          name,
          rawName,
          value: rawValue.trim(),
          rawValue,
          nameStart,
          valueStart,
          valueEnd,
        });
        i = j;
      }
    } else {
      attrs.push({
        name,
        rawName,
        value: "",
        rawValue: "",
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

function getExactAttr(attrs: readonly ParsedAttr[], rawName: string): ParsedAttr | undefined {
  return attrs.find((attr) => attr.rawName === rawName);
}

/**
 * Finds the matching `</tagName>` while skipping `<![CDATA[...]]>` and
 * `<!-- ... -->` sections inside `<script>`, `<style>`, `<title>`, or `<desc>`.
 */
function findElementClose(
  xml: string,
  lower: string,
  tagName: string,
  from: number,
): { readonly contentEnd: number; readonly blockEnd: number } {
  const len = xml.length;
  const marker = `</${tagName}`;
  const markerLen = marker.length;
  let i = from;
  while (i < len) {
    const lt = xml.indexOf("<", i);
    if (lt < 0) return { contentEnd: len, blockEnd: len };
    if (xml.startsWith("<![CDATA[", lt)) {
      const cdataClose = xml.indexOf("]]>", lt + 9);
      i = cdataClose < 0 ? len : cdataClose + 3;
      continue;
    }
    if (xml.startsWith("<!--", lt)) {
      const commentClose = xml.indexOf("-->", lt + 4);
      i = commentClose < 0 ? len : commentClose + 3;
      continue;
    }
    if (lower.startsWith(marker, lt)) {
      const afterClose = lower[lt + markerLen];
      if (
        afterClose === undefined ||
        isAsciiSpace(afterClose.charCodeAt(0)) ||
        afterClose === ">" ||
        afterClose === "/"
      ) {
        const closeGt = xml.indexOf(">", lt + markerLen);
        return { contentEnd: lt, blockEnd: closeGt < 0 ? len : closeGt + 1 };
      }
    }
    i = lt + 1;
  }
  return { contentEnd: len, blockEnd: len };
}

function scanXmlTokens(xml: string): XmlToken[] {
  const tokens: XmlToken[] = [];
  const lower = xml.toLowerCase();
  const len = xml.length;
  let svgElementDepth = 0;
  let i = 0;

  while (i < len) {
    const lt = xml.indexOf("<", i);
    if (lt < 0) break;

    if (xml.startsWith("<!--", lt)) {
      const close = xml.indexOf("-->", lt + 4);
      const end = close < 0 ? len : close + 3;
      tokens.push({ kind: "comment", start: lt, end });
      i = end;
      continue;
    }

    if (xml.startsWith("<![CDATA[", lt)) {
      const close = xml.indexOf("]]>", lt + 9);
      const end = close < 0 ? len : close + 3;
      tokens.push({ kind: "cdata", start: lt, end });
      i = end;
      continue;
    }

    if (xml.startsWith("<?", lt)) {
      const close = xml.indexOf("?>", lt + 2);
      const end = close < 0 ? len : close + 2;
      const bodyEnd = close < 0 ? len : close;
      let targetStart = lt + 2;
      while (targetStart < bodyEnd && isAsciiSpace(xml.charCodeAt(targetStart))) targetStart++;
      let targetEnd = targetStart;
      while (targetEnd < bodyEnd && !isAsciiSpace(xml.charCodeAt(targetEnd))) targetEnd++;
      const piTarget = lower.slice(targetStart, targetEnd);
      const isXmlStylesheet = piTarget === "xml-stylesheet";
      const attrs = isXmlStylesheet ? parseTagAttributes(xml, targetEnd, bodyEnd) : [];
      tokens.push({ kind: "pi", start: lt, end, isXmlStylesheet, attrs });
      i = end;
      continue;
    }

    if (lower.startsWith("<!doctype", lt)) {
      const next = lower[lt + 9];
      if (
        next === undefined ||
        isAsciiSpace(next.charCodeAt(0)) ||
        next === ">" ||
        next === "["
      ) {
        const { end, hasInternalSubset } = findDoctypeEnd(xml, lt + 9);
        tokens.push({ kind: "doctype", start: lt, end, hasInternalSubset });
        i = end;
        continue;
      }
    }

    if (
      lower.startsWith("<!entity", lt) ||
      lower.startsWith("<!element", lt) ||
      lower.startsWith("<!attlist", lt) ||
      lower.startsWith("<!notation", lt)
    ) {
      const gt = findTagEnd(xml, lt + 2);
      const end = gt < 0 ? len : gt + 1;
      tokens.push({ kind: "dtd-decl", start: lt, end });
      i = end;
      continue;
    }

    // Closing tag: </name ...>
    if (lt + 1 < len && xml.charCodeAt(lt + 1) === 47) {
      const nameIdx = lt + 2;
      if (nameIdx < len && isTagNameStart(xml.charCodeAt(nameIdx))) {
        let nameEnd = nameIdx + 1;
        while (nameEnd < len && isTagNameChar(xml.charCodeAt(nameEnd))) nameEnd++;
        const gt = xml.indexOf(">", nameEnd);
        const end = gt < 0 ? len : gt + 1;
        const rawName = xml.slice(nameIdx, nameEnd);
        const name = lower.slice(nameIdx, nameEnd);
        const { prefix: rawPrefix, localName: rawLocalName } = splitTagName(rawName);
        const { prefix, localName } = splitTagName(name);
        if (svgElementDepth > 0) svgElementDepth -= 1;
        tokens.push({
          kind: "tag",
          start: lt,
          end,
          name,
          rawName,
          localName,
          rawLocalName,
          prefix,
          rawPrefix,
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
    if (lt + 1 < len && isTagNameStart(xml.charCodeAt(lt + 1))) {
      let nameEnd = lt + 2;
      while (nameEnd < len && isTagNameChar(xml.charCodeAt(nameEnd))) nameEnd++;
      const rawName = xml.slice(lt + 1, nameEnd);
      const name = lower.slice(lt + 1, nameEnd);
      const { prefix: rawPrefix, localName: rawLocalName } = splitTagName(rawName);
      const { prefix, localName } = splitTagName(name);
      const afterName = xml.charCodeAt(nameEnd);
      if (
        nameEnd < len &&
        !isAsciiSpace(afterName) &&
        afterName !== 62 &&
        afterName !== 47
      ) {
        i = nameEnd;
        continue;
      }
      const gt = findTagEnd(xml, nameEnd);
      if (gt < 0) {
        tokens.push({
          kind: "unclosed-tag",
          start: lt,
          end: len,
          name,
          rawName,
          localName,
          rawLocalName,
        });
        break;
      }
      let prevNonSpace = gt - 1;
      while (prevNonSpace >= nameEnd && isAsciiSpace(xml.charCodeAt(prevNonSpace))) {
        prevNonSpace -= 1;
      }
      const selfClosing = prevNonSpace >= nameEnd && xml.charCodeAt(prevNonSpace) === 47;
      const attrEnd = selfClosing ? prevNonSpace : gt;
      const attrs = parseTagAttributes(xml, nameEnd, attrEnd);

      if (localName === "script") {
        if (selfClosing) {
          tokens.push({ kind: "script", start: lt, end: gt + 1, rawName, attrs });
          i = gt + 1;
        } else {
          const { blockEnd } = findElementClose(xml, lower, name, gt + 1);
          tokens.push({ kind: "script", start: lt, end: blockEnd, rawName, attrs });
          i = blockEnd;
        }
        continue;
      }

      if (localName === "style" && !selfClosing) {
        const { contentEnd, blockEnd } = findElementClose(xml, lower, name, gt + 1);
        tokens.push({
          kind: "style",
          start: lt,
          end: blockEnd,
          rawName,
          contentStart: gt + 1,
          contentEnd,
          attrs,
        });
        i = blockEnd;
        continue;
      }

      if (localName === "title" && !selfClosing) {
        const { contentEnd, blockEnd } = findElementClose(xml, lower, name, gt + 1);
        tokens.push({
          kind: "title",
          start: lt,
          end: blockEnd,
          rawName,
          contentStart: gt + 1,
          contentEnd,
          depth: svgElementDepth,
          attrs,
        });
        i = blockEnd;
        continue;
      }

      if (localName === "desc" && !selfClosing) {
        const { contentEnd, blockEnd } = findElementClose(xml, lower, name, gt + 1);
        tokens.push({
          kind: "desc",
          start: lt,
          end: blockEnd,
          rawName,
          contentStart: gt + 1,
          contentEnd,
          depth: svgElementDepth,
          attrs,
        });
        i = blockEnd;
        continue;
      }

      tokens.push({
        kind: "tag",
        start: lt,
        end: gt + 1,
        name,
        rawName,
        localName,
        rawLocalName,
        prefix,
        rawPrefix,
        isClosing: false,
        selfClosing,
        attrs,
      });
      if (!selfClosing) {
        svgElementDepth += 1;
      }
      i = gt + 1;
      continue;
    }

    i = lt + 1;
  }

  return tokens;
}

// ---------------------------------------------------------------------------
// SVG Document Analysis, Sanitization, Normalization & Asset Inlining
// ---------------------------------------------------------------------------

export interface SvgViewBox {
  readonly minX: number;
  readonly minY: number;
  readonly width: number;
  readonly height: number;
  readonly raw: string;
}

export interface SvgDocumentStats {
  readonly title: string | null;
  readonly description: string | null;
  readonly byteCount: number;
  readonly lineCount: number;
  readonly viewBox: SvgViewBox | null;
  readonly declaredWidth: string | null;
  readonly declaredHeight: string | null;
  readonly intrinsicWidth: number;
  readonly intrinsicHeight: number;
  readonly dimensionSource: SvgDimensionSource;
  readonly hasXmlns: boolean;
  readonly usesXlink: boolean;
  readonly hasXlinkXmlns: boolean;
  readonly usesCurrentColor: boolean;
  readonly elementCount: number;
  readonly pathCount: number;
  readonly shapeCount: number;
  readonly textCount: number;
  readonly groupCount: number;
  readonly imageCount: number;
  readonly defsCount: number;
  readonly foreignObjectCount: number;
  readonly animationCount: number;
  readonly scriptCount: number;
  readonly eventHandlerCount: number;
  readonly unsafeHrefCount: number;
  readonly unsafeSmilCount: number;
  readonly strippedStylesheetPiCount: number;
  readonly externalRefCount: number;
  readonly strippedDtd: boolean;
  readonly localImageRefs: readonly string[];
}

export interface PreparedSvgDocument {
  readonly dataUrl: string;
  readonly normalizedXml: string;
  readonly stats: SvgDocumentStats;
  readonly tooLarge: boolean;
  readonly empty: boolean;
  readonly parseError: string | null;
  readonly omittedBytes: number;
  readonly inlinedImageCount: number;
  readonly synthesizedViewBox: boolean;
  readonly addedXmlns: boolean;
}

export interface PrepareSvgOptions {
  readonly canvas?: SvgCanvasSurface;
  /** Map from raw `<image>`/`<feImage>` `href` or `xlink:href` to `data:` URI. */
  readonly inlinedImages?: Readonly<Record<string, string>>;
}

export interface InlinedSvgAssetResult {
  readonly images: Record<string, string>;
  readonly inlinedImageCount: number;
  readonly missingAssets: readonly string[];
  readonly skippedCapCount: number;
}

const SHAPE_TAGS = new Set(["rect", "circle", "ellipse", "line", "polyline", "polygon"]);
const ANIMATION_TAGS = new Set([
  "animate",
  "animatetransform",
  "animatemotion",
  "set",
]);
const DEFS_TAGS = new Set([
  "defs",
  "lineargradient",
  "radialgradient",
  "pattern",
  "clippath",
  "mask",
  "filter",
  "marker",
  "symbol",
]);
const CURRENT_COLOR_ATTRS = new Set([
  "fill",
  "stroke",
  "color",
  "stop-color",
  "flood-color",
  "lighting-color",
  "style",
]);
const EVENT_HANDLER_STOP = new Set(["on"]);
const SVG_LENGTH_RE = /^\s*([+-]?(?:\d+(?:\.\d+)?|\.\d+)(?:[eE][+-]?\d+)?)\s*(px|pt|pc|in|cm|mm|q|em|rem|%)?\s*$/i;

function parseSvgLengthPx(raw: string | null | undefined): number | null {
  if (!raw) return null;
  const match = SVG_LENGTH_RE.exec(raw);
  if (!match) return null;
  const value = Number.parseFloat(match[1]);
  if (!Number.isFinite(value) || value <= 0) return null;
  const unit = (match[2] ?? "").toLowerCase();
  switch (unit) {
    case "":
    case "px":
      return value;
    case "pt":
      return (value * 96) / 72;
    case "pc":
      return value * 16;
    case "in":
      return value * 96;
    case "cm":
      return (value * 96) / 2.54;
    case "mm":
      return (value * 96) / 25.4;
    case "q":
      return (value * 96) / 101.6;
    case "em":
    case "rem":
      return value * 16;
    case "%":
    default:
      return null;
  }
}

function parseViewBox(raw: string | null | undefined): SvgViewBox | null {
  if (!raw) return null;
  const trimmed = raw.trim();
  if (!trimmed) return null;
  const parts = trimmed.split(/[\s,]+/).filter(Boolean);
  if (parts.length !== 4) return null;
  const nums = parts.map((part) => Number(part));
  if (nums.some((n) => !Number.isFinite(n))) return null;
  const [minX, minY, width, height] = nums;
  if (width <= 0 || height <= 0) return null;
  return { minX, minY, width, height, raw: trimmed };
}

function roundDim(value: number): number {
  return Math.round(value * 1000) / 1000;
}

function resolveIntrinsicDimensions(
  declaredWidth: string | null,
  declaredHeight: string | null,
  viewBox: SvgViewBox | null,
): {
  readonly width: number;
  readonly height: number;
  readonly source: SvgDimensionSource;
  readonly parsedWidth: number | null;
  readonly parsedHeight: number | null;
} {
  const wPx = parseSvgLengthPx(declaredWidth);
  const hPx = parseSvgLengthPx(declaredHeight);

  if (wPx !== null && hPx !== null) {
    return {
      width: roundDim(wPx),
      height: roundDim(hPx),
      source: "attributes",
      parsedWidth: wPx,
      parsedHeight: hPx,
    };
  }
  if (wPx !== null && hPx === null) {
    const derivedH = viewBox ? (wPx * viewBox.height) / viewBox.width : DEFAULT_SVG_HEIGHT;
    return {
      width: roundDim(wPx),
      height: roundDim(derivedH),
      source: "attributes",
      parsedWidth: wPx,
      parsedHeight: null,
    };
  }
  if (wPx === null && hPx !== null) {
    const derivedW = viewBox ? (hPx * viewBox.width) / viewBox.height : DEFAULT_SVG_WIDTH;
    return {
      width: roundDim(derivedW),
      height: roundDim(hPx),
      source: "attributes",
      parsedWidth: null,
      parsedHeight: hPx,
    };
  }
  if (viewBox) {
    return {
      width: roundDim(viewBox.width),
      height: roundDim(viewBox.height),
      source: "viewBox",
      parsedWidth: null,
      parsedHeight: null,
    };
  }
  return {
    width: DEFAULT_SVG_WIDTH,
    height: DEFAULT_SVG_HEIGHT,
    source: "default",
    parsedWidth: null,
    parsedHeight: null,
  };
}

function isEventHandlerAttr(name: string): boolean {
  return name.startsWith("on") && name.length > 2 && !EVENT_HANDLER_STOP.has(name);
}

const CSS_URL_ATTRS = new Set([
  "style",
  "fill",
  "stroke",
  "filter",
  "clip-path",
  "mask",
  "marker-start",
  "marker-mid",
  "marker-end",
  "cursor",
]);

function isUnsafeSmilAnimationTag(tok: Extract<XmlToken, { kind: "tag" }>): boolean {
  if (tok.isClosing || !ANIMATION_TAGS.has(tok.localName)) return false;
  const attrName = getAttr(tok.attrs, "attributename");
  if (!attrName) return false;
  let stripped = "";
  for (let i = 0; i < attrName.value.length; i++) {
    const code = attrName.value.charCodeAt(i);
    if (code > 0x20 && code !== 0x7f) stripped += attrName.value[i];
  }
  const target = stripped.toLowerCase();
  if (!target) return false;
  if (
    target === "href" ||
    target === "xlink:href" ||
    target.endsWith(":href") ||
    target === "action" ||
    target === "formaction"
  ) {
    return true;
  }
  return isEventHandlerAttr(target);
}

function stripMetadataInner(rawInner: string): string {
  // Remove simple XML comment or CDATA wrappers cleanly without backtracking regexes.
  let text = rawInner;
  if (text.includes("<![CDATA[")) {
    text = text.replace(/<!\[CDATA\[([\s\S]*?)\]\]>/g, "$1");
  }
  if (text.includes("<!--")) {
    text = text.replace(/<!--[\s\S]*?-->/g, "");
  }
  return decodeXmlEntities(text).replace(/\s+/g, " ").trim().slice(0, 512);
}

function emptySvgStats(byteCount: number, lineCount: number): SvgDocumentStats {
  return {
    title: null,
    description: null,
    byteCount,
    lineCount,
    viewBox: null,
    declaredWidth: null,
    declaredHeight: null,
    intrinsicWidth: DEFAULT_SVG_WIDTH,
    intrinsicHeight: DEFAULT_SVG_HEIGHT,
    dimensionSource: "default",
    hasXmlns: false,
    usesXlink: false,
    hasXlinkXmlns: false,
    usesCurrentColor: false,
    elementCount: 0,
    pathCount: 0,
    shapeCount: 0,
    textCount: 0,
    groupCount: 0,
    imageCount: 0,
    defsCount: 0,
    foreignObjectCount: 0,
    animationCount: 0,
    scriptCount: 0,
    eventHandlerCount: 0,
    unsafeHrefCount: 0,
    unsafeSmilCount: 0,
    strippedStylesheetPiCount: 0,
    externalRefCount: 0,
    strippedDtd: false,
    localImageRefs: [],
  };
}

function inspectSvgTokens(
  rawSvg: string,
  tokens: readonly XmlToken[],
  byteCount: number,
  lineCount: number,
): {
  readonly stats: SvgDocumentStats;
  readonly rootSvg: Extract<XmlToken, { kind: "tag" }> | null;
  readonly nonSvgRootName: string | null;
  readonly wrongRootNamespace: string | null;
  readonly unclosedRootTag: boolean;
  readonly unclosedSvgTree: boolean;
} {
  let rootSvg: Extract<XmlToken, { kind: "tag" }> | null = null;
  let sawRootElement = false;
  let nonSvgRootName: string | null = null;
  let unclosedRootTag = false;
  const svgOpenStack: string[] = [];
  let firstTitle: string | null = null;
  let directTitle: string | null = null;
  let firstDesc: string | null = null;
  let directDesc: string | null = null;
  let usesXlink = false;
  let usesCurrentColor = false;

  let elementCount = 0;
  let pathCount = 0;
  let shapeCount = 0;
  let textCount = 0;
  let groupCount = 0;
  let imageCount = 0;
  let defsCount = 0;
  let foreignObjectCount = 0;
  let animationCount = 0;
  let scriptCount = 0;
  let eventHandlerCount = 0;
  let unsafeHrefCount = 0;
  let unsafeSmilCount = 0;
  let strippedStylesheetPiCount = 0;
  let externalRefCount = 0;
  let strippedDtd = false;
  const localImages = new Set<string>();

  const inspectAttrs = (attrs: readonly ParsedAttr[], tagLocal: string) => {
    for (const attr of attrs) {
      if (attr.rawName.startsWith("xlink:")) usesXlink = true;
      if (isEventHandlerAttr(attr.name)) {
        eventHandlerCount += 1;
      }
      if (isActionOrHrefAttr(attr.name)) {
        if (isUnsafeScriptHref(attr.value)) {
          unsafeHrefCount += 1;
        } else if (isHrefLikeAttr(attr.name)) {
          const val = attr.value.trim();
          if (EXTERNAL_URL_RE.test(val)) {
            externalRefCount += 1;
          } else if (
            (tagLocal === "image" || tagLocal === "feimage") &&
            isLocalRef(val)
          ) {
            localImages.add(val);
          }
        }
      }
      if (attr.name === "style") {
        externalRefCount += countExternalCssRefs(attr.value);
      } else if (CSS_URL_ATTRS.has(attr.name) && attr.value.includes("(")) {
        externalRefCount += countExternalCssRefs(attr.value);
      }
      if (CURRENT_COLOR_ATTRS.has(attr.name) && /currentcolor/i.test(attr.value)) {
        usesCurrentColor = true;
      }
    }
  };

  for (const tok of tokens) {
    if (tok.kind === "comment" || tok.kind === "cdata") {
      continue;
    }
    if (tok.kind === "pi") {
      if (tok.isXmlStylesheet) {
        strippedStylesheetPiCount += 1;
        const hrefAttr = getAttr(tok.attrs, "href");
        if (hrefAttr && EXTERNAL_URL_RE.test(hrefAttr.value.trim())) {
          externalRefCount += 1;
        }
      }
      continue;
    }
    if (tok.kind === "doctype") {
      if (tok.hasInternalSubset) strippedDtd = true;
      continue;
    }
    if (tok.kind === "dtd-decl") {
      strippedDtd = true;
      continue;
    }
    if (tok.kind === "unclosed-tag") {
      if (!sawRootElement) {
        sawRootElement = true;
        if (tok.rawLocalName === "svg") {
          unclosedRootTag = true;
        } else {
          nonSvgRootName = tok.rawName;
        }
      }
      continue;
    }
    if (tok.kind === "script") {
      if (!sawRootElement) {
        sawRootElement = true;
        nonSvgRootName = tok.rawName;
      }
      scriptCount += 1;
      inspectAttrs(tok.attrs, "script");
      continue;
    }
    if (tok.kind === "style") {
      if (!sawRootElement) {
        sawRootElement = true;
        nonSvgRootName = tok.rawName;
      }
      elementCount += 1;
      inspectAttrs(tok.attrs, "style");
      const css = decodeStyleXmlText(rawSvg.slice(tok.contentStart, tok.contentEnd));
      if (/currentcolor/i.test(css)) usesCurrentColor = true;
      externalRefCount += countExternalCssRefs(css);
      continue;
    }
    if (tok.kind === "title") {
      if (!sawRootElement) {
        sawRootElement = true;
        nonSvgRootName = tok.rawName;
      }
      elementCount += 1;
      inspectAttrs(tok.attrs, "title");
      const text = stripMetadataInner(rawSvg.slice(tok.contentStart, tok.contentEnd));
      if (text) {
        if (firstTitle === null) firstTitle = text;
        if (directTitle === null && tok.depth === 1) directTitle = text;
      }
      continue;
    }
    if (tok.kind === "desc") {
      if (!sawRootElement) {
        sawRootElement = true;
        nonSvgRootName = tok.rawName;
      }
      elementCount += 1;
      inspectAttrs(tok.attrs, "desc");
      const text = stripMetadataInner(rawSvg.slice(tok.contentStart, tok.contentEnd));
      if (text) {
        if (firstDesc === null) firstDesc = text;
        if (directDesc === null && tok.depth === 1) directDesc = text;
      }
      continue;
    }
    if (tok.kind === "tag") {
      if (tok.isClosing) {
        if (tok.rawLocalName === "svg" && svgOpenStack.length > 0 && svgOpenStack[svgOpenStack.length - 1] === tok.rawName) {
          svgOpenStack.pop();
        }
        continue;
      }

      if (!sawRootElement) {
        sawRootElement = true;
        if (tok.rawLocalName === "svg") {
          rootSvg = tok;
        } else {
          nonSvgRootName = tok.rawName;
        }
      }

      if (tok.rawLocalName === "svg" && rootSvg && !tok.selfClosing) {
        svgOpenStack.push(tok.rawName);
      }

      elementCount += 1;
      if (tok.localName === "path") pathCount += 1;
      else if (SHAPE_TAGS.has(tok.localName)) shapeCount += 1;
      else if (tok.localName === "text" || tok.localName === "tspan") textCount += 1;
      else if (tok.localName === "g") groupCount += 1;
      else if (tok.localName === "image" || tok.localName === "feimage") imageCount += 1;
      else if (DEFS_TAGS.has(tok.localName)) defsCount += 1;
      else if (tok.localName === "foreignobject") foreignObjectCount += 1;
      else if (ANIMATION_TAGS.has(tok.localName)) {
        animationCount += 1;
        if (isUnsafeSmilAnimationTag(tok)) {
          unsafeSmilCount += 1;
        }
      }

      inspectAttrs(tok.attrs, tok.localName);
    }
  }

  const rootAttrs = rootSvg?.attrs ?? [];
  const viewBoxAttr = getExactAttr(rootAttrs, "viewBox")?.value ?? null;
  const declaredWidth = getExactAttr(rootAttrs, "width")?.value ?? null;
  const declaredHeight = getExactAttr(rootAttrs, "height")?.value ?? null;
  const viewBox = parseViewBox(viewBoxAttr);
  const dims = resolveIntrinsicDimensions(declaredWidth, declaredHeight, viewBox);
  const rootRawPrefix = rootSvg?.rawPrefix ?? null;
  const xmlnsAttr = rootSvg
    ? rootRawPrefix
      ? getExactAttr(rootAttrs, `xmlns:${rootRawPrefix}`)
      : getExactAttr(rootAttrs, "xmlns")
    : undefined;
  const hasXmlns = Boolean(xmlnsAttr);
  const hasXlinkXmlns = Boolean(getExactAttr(rootAttrs, "xmlns:xlink"));
  const wrongRootNamespace =
    xmlnsAttr && xmlnsAttr.rawValue !== "http://www.w3.org/2000/svg" ? xmlnsAttr.rawValue : null;

  return {
    rootSvg,
    nonSvgRootName,
    wrongRootNamespace,
    unclosedRootTag,
    unclosedSvgTree: rootSvg !== null && !rootSvg.selfClosing && svgOpenStack.length > 0,
    stats: {
      title: directTitle ?? firstTitle,
      description: directDesc ?? firstDesc,
      byteCount,
      lineCount,
      viewBox,
      declaredWidth,
      declaredHeight,
      intrinsicWidth: dims.width,
      intrinsicHeight: dims.height,
      dimensionSource: dims.source,
      hasXmlns,
      usesXlink,
      hasXlinkXmlns,
      usesCurrentColor,
      elementCount,
      pathCount,
      shapeCount,
      textCount,
      groupCount,
      imageCount,
      defsCount,
      foreignObjectCount,
      animationCount,
      scriptCount,
      eventHandlerCount,
      unsafeHrefCount,
      unsafeSmilCount,
      strippedStylesheetPiCount,
      externalRefCount,
      strippedDtd,
      localImageRefs: [...localImages],
    },
  };
}

/** Inspects an SVG XML string for dimensions, elements, local images, and safety counters. */
export function analyzeSvgDocument(rawSvg: string): SvgDocumentStats {
  const byteCount = utf8ByteLength(rawSvg);
  if (!rawSvg) return emptySvgStats(0, 0);
  const lineCount = rawSvg.split(/\r?\n/).length;
  const tokens = scanXmlTokens(rawSvg);
  return inspectSvgTokens(rawSvg, tokens, byteCount, lineCount).stats;
}

interface TextEdit {
  readonly start: number;
  readonly end: number;
  readonly replacement: string;
}

function applyTextEdits(source: string, edits: readonly TextEdit[]): string {
  if (edits.length === 0) return source;
  const sorted = [...edits].sort((a, b) => a.start - b.start || a.end - b.end);
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

function attrRemovalRange(source: string, attr: ParsedAttr): { readonly start: number; readonly end: number } {
  let start = attr.nameStart;
  while (start > 0 && isAsciiSpace(source.charCodeAt(start - 1))) {
    start -= 1;
  }
  return { start, end: attr.valueEnd };
}

/**
 * Prepares untrusted repository SVG XML for safe, deterministic preview inside
 * an `<img>` element (`data:image/svg+xml;base64,...`) by enforcing a 1 MiB cap,
 * checking structural integrity, stripping `<script>` / `on*` / `javascript:` /
 * `data:text/html` / unsafe SMIL / `<?xml-stylesheet?>` / DOCTYPE internal subsets,
 * normalizing missing `xmlns` / `xmlns:xlink` / `viewBox` / intrinsic dimensions /
 * `currentColor` fallback, and substituting pre-inlined local `<image>`/`<feImage>` assets.
 */
export function prepareSvgPreviewDocument(
  rawSvg: string,
  options: PrepareSvgOptions = {},
): PreparedSvgDocument {
  const byteCount = utf8ByteLength(rawSvg);
  const lineCount = rawSvg ? rawSvg.split(/\r?\n/).length : 0;

  if (byteCount > MAX_SVG_PREVIEW_BYTES) {
    return {
      dataUrl: "",
      normalizedXml: "",
      stats: emptySvgStats(byteCount, lineCount),
      tooLarge: true,
      empty: false,
      parseError: null,
      omittedBytes: byteCount - MAX_SVG_PREVIEW_BYTES,
      inlinedImageCount: 0,
      synthesizedViewBox: false,
      addedXmlns: false,
    };
  }

  if (!rawSvg || rawSvg.trim().length === 0) {
    return {
      dataUrl: "",
      normalizedXml: "",
      stats: emptySvgStats(byteCount, lineCount),
      tooLarge: false,
      empty: true,
      parseError: null,
      omittedBytes: 0,
      inlinedImageCount: 0,
      synthesizedViewBox: false,
      addedXmlns: false,
    };
  }

  const tokens = scanXmlTokens(rawSvg);
  const inspected = inspectSvgTokens(rawSvg, tokens, byteCount, lineCount);
  const { stats, rootSvg, nonSvgRootName, wrongRootNamespace, unclosedRootTag, unclosedSvgTree } =
    inspected;

  if (unclosedRootTag) {
    return {
      dataUrl: "",
      normalizedXml: "",
      stats,
      tooLarge: false,
      empty: false,
      parseError: "Unclosed <svg> start tag.",
      omittedBytes: 0,
      inlinedImageCount: 0,
      synthesizedViewBox: false,
      addedXmlns: false,
    };
  }

  if (!rootSvg) {
    return {
      dataUrl: "",
      normalizedXml: "",
      stats,
      tooLarge: false,
      empty: false,
      parseError: nonSvgRootName
        ? `Missing root <svg> element. Document root is <${nonSvgRootName}>.`
        : "Missing root <svg> element.",
      omittedBytes: 0,
      inlinedImageCount: 0,
      synthesizedViewBox: false,
      addedXmlns: false,
    };
  }

  if (wrongRootNamespace !== null) {
    return {
      dataUrl: "",
      normalizedXml: "",
      stats,
      tooLarge: false,
      empty: false,
      parseError: "Root <svg> element must use the SVG namespace (http://www.w3.org/2000/svg).",
      omittedBytes: 0,
      inlinedImageCount: 0,
      synthesizedViewBox: false,
      addedXmlns: false,
    };
  }

  if (unclosedSvgTree) {
    return {
      dataUrl: "",
      normalizedXml: "",
      stats,
      tooLarge: false,
      empty: false,
      parseError: "Unclosed <svg> element (missing </svg>).",
      omittedBytes: 0,
      inlinedImageCount: 0,
      synthesizedViewBox: false,
      addedXmlns: false,
    };
  }

  const canvas = options.canvas ?? "grid";
  const inlinedImages = options.inlinedImages ?? {};
  const edits: TextEdit[] = [];
  let inlinedImageCount = 0;

  const stripUnsafeAttrs = (attrs: readonly ParsedAttr[]) => {
    for (const attr of attrs) {
      if (isEventHandlerAttr(attr.name)) {
        const span = attrRemovalRange(rawSvg, attr);
        edits.push({ start: span.start, end: span.end, replacement: "" });
        continue;
      }
      if (isActionOrHrefAttr(attr.name) && isUnsafeScriptHref(attr.value)) {
        const span = attrRemovalRange(rawSvg, attr);
        edits.push({ start: span.start, end: span.end, replacement: "" });
      }
    }
  };

  for (let idx = 0; idx < tokens.length; idx++) {
    const tok = tokens[idx];
    if (tok.kind === "pi") {
      if (tok.isXmlStylesheet) {
        edits.push({ start: tok.start, end: tok.end, replacement: "" });
      }
      continue;
    }
    if (tok.kind === "doctype") {
      if (tok.hasInternalSubset) {
        edits.push({ start: tok.start, end: tok.end, replacement: "" });
      }
      continue;
    }
    if (tok.kind === "dtd-decl" || tok.kind === "script") {
      edits.push({ start: tok.start, end: tok.end, replacement: "" });
      continue;
    }
    if (tok.kind === "style" || tok.kind === "title" || tok.kind === "desc") {
      stripUnsafeAttrs(tok.attrs);
      continue;
    }
    if (tok.kind === "tag" && !tok.isClosing) {
      if (isUnsafeSmilAnimationTag(tok)) {
        if (tok.selfClosing) {
          edits.push({ start: tok.start, end: tok.end, replacement: "" });
        } else {
          let depth = 1;
          let removeEnd = tok.end;
          let j = idx + 1;
          while (j < tokens.length && depth > 0) {
            const nextTok = tokens[j];
            if (nextTok.kind === "tag" && nextTok.name === tok.name) {
              if (nextTok.isClosing) {
                depth -= 1;
                if (depth === 0) {
                  removeEnd = nextTok.end;
                  break;
                }
              } else if (!nextTok.selfClosing) {
                depth += 1;
              }
            }
            j += 1;
          }
          edits.push({ start: tok.start, end: removeEnd, replacement: "" });
          if (depth === 0) idx = j;
        }
        continue;
      }

      for (const attr of tok.attrs) {
        if (isEventHandlerAttr(attr.name)) {
          const span = attrRemovalRange(rawSvg, attr);
          edits.push({ start: span.start, end: span.end, replacement: "" });
          continue;
        }
        if (isActionOrHrefAttr(attr.name) && isUnsafeScriptHref(attr.value)) {
          const span = attrRemovalRange(rawSvg, attr);
          edits.push({ start: span.start, end: span.end, replacement: "" });
          continue;
        }
        if (
          (tok.localName === "image" || tok.localName === "feimage") &&
          isHrefLikeAttr(attr.name) &&
          attr.valueStart >= 0 &&
          Object.prototype.hasOwnProperty.call(inlinedImages, attr.value)
        ) {
          inlinedImageCount += 1;
          edits.push({
            start: attr.valueStart,
            end: attr.valueEnd,
            replacement: `"${escapeXmlAttrValue(inlinedImages[attr.value])}"`,
          });
        }
      }
    }
  }

  // Normalize the root `<svg>` element on the preview copy only (exact XML case).
  const rootAttrs = rootSvg.attrs;
  const viewBoxAttrs = rootAttrs.filter((a) => a.rawName === "viewBox");
  const widthAttrs = rootAttrs.filter((a) => a.rawName === "width");
  const heightAttrs = rootAttrs.filter((a) => a.rawName === "height");
  const parsedW = parseSvgLengthPx(stats.declaredWidth);
  const parsedH = parseSvgLengthPx(stats.declaredHeight);
  const colorAttr = getExactAttr(rootAttrs, "color");
  const addedXmlns = !stats.hasXmlns;
  const synthesizedViewBox = !stats.viewBox;

  let appendAttrs = "";
  if (addedXmlns) {
    appendAttrs += rootSvg.rawPrefix
      ? ` xmlns:${rootSvg.rawPrefix}="http://www.w3.org/2000/svg"`
      : ' xmlns="http://www.w3.org/2000/svg"';
  }
  if (stats.usesXlink && !stats.hasXlinkXmlns) {
    appendAttrs += ' xmlns:xlink="http://www.w3.org/1999/xlink"';
  }

  if (synthesizedViewBox) {
    const synthViewBox = `0 0 ${stats.intrinsicWidth} ${stats.intrinsicHeight}`;
    if (viewBoxAttrs.length > 0) {
      const first = viewBoxAttrs[0];
      if (first.valueStart >= 0) {
        edits.push({
          start: first.valueStart,
          end: first.valueEnd,
          replacement: `"${synthViewBox}"`,
        });
      } else {
        edits.push({
          start: first.nameStart,
          end: first.valueEnd,
          replacement: `viewBox="${synthViewBox}"`,
        });
      }
      for (const extra of viewBoxAttrs.slice(1)) {
        const span = attrRemovalRange(rawSvg, extra);
        edits.push({ start: span.start, end: span.end, replacement: "" });
      }
    } else {
      appendAttrs += ` viewBox="${synthViewBox}"`;
    }
  }

  if (parsedW === null) {
    if (widthAttrs.length > 0) {
      const first = widthAttrs[0];
      if (first.valueStart >= 0) {
        edits.push({
          start: first.valueStart,
          end: first.valueEnd,
          replacement: `"${stats.intrinsicWidth}"`,
        });
      } else {
        edits.push({
          start: first.nameStart,
          end: first.valueEnd,
          replacement: `width="${stats.intrinsicWidth}"`,
        });
      }
      for (const extra of widthAttrs.slice(1)) {
        const span = attrRemovalRange(rawSvg, extra);
        edits.push({ start: span.start, end: span.end, replacement: "" });
      }
    } else {
      appendAttrs += ` width="${stats.intrinsicWidth}"`;
    }
  }

  if (parsedH === null) {
    if (heightAttrs.length > 0) {
      const first = heightAttrs[0];
      if (first.valueStart >= 0) {
        edits.push({
          start: first.valueStart,
          end: first.valueEnd,
          replacement: `"${stats.intrinsicHeight}"`,
        });
      } else {
        edits.push({
          start: first.nameStart,
          end: first.valueEnd,
          replacement: `height="${stats.intrinsicHeight}"`,
        });
      }
      for (const extra of heightAttrs.slice(1)) {
        const span = attrRemovalRange(rawSvg, extra);
        edits.push({ start: span.start, end: span.end, replacement: "" });
      }
    } else {
      appendAttrs += ` height="${stats.intrinsicHeight}"`;
    }
  }

  if (!colorAttr) {
    const ink = canvas === "dark" || canvas === "dark-grid" ? DARK_CANVAS_INK : LIGHT_CANVAS_INK;
    appendAttrs += ` color="${ink}"`;
  }

  if (appendAttrs) {
    let insertAt = rootSvg.end - 1;
    if (rootSvg.selfClosing) {
      let slash = rootSvg.end - 2;
      while (slash > rootSvg.start && isAsciiSpace(rawSvg.charCodeAt(slash))) {
        slash -= 1;
      }
      insertAt = slash;
    }
    edits.push({
      start: insertAt,
      end: insertAt,
      replacement: appendAttrs,
    });
  }

  const normalizedXml = applyTextEdits(rawSvg, edits);
  return {
    dataUrl: encodeSvgDataUrl(normalizedXml),
    normalizedXml,
    stats,
    tooLarge: false,
    empty: false,
    parseError: null,
    omittedBytes: 0,
    inlinedImageCount,
    synthesizedViewBox,
    addedXmlns,
  };
}

/**
 * Parses an already-sanitized SVG XML string with the host `DOMParser` when
 * available and returns a concise first-line XML error message if malformed
 * or if the document root is not `<svg>` in the SVG namespace, or `null` when
 * valid (or when `DOMParser` is unavailable, e.g. SSR).
 */
export function validateSvgXmlInDom(svgXml: string): string | null {
  if (!svgXml || typeof DOMParser === "undefined") return null;
  try {
    const doc = new DOMParser().parseFromString(svgXml, "image/svg+xml");
    const parserErr =
      doc.querySelector("parsererror") ?? doc.getElementsByTagName("parsererror")[0] ?? null;
    if (parserErr) {
      const raw = (parserErr.textContent ?? "")
        .replace(/This page contains the following errors:\s*/i, "")
        .replace(/Below is a rendering of the page up to the first error\.?\s*/i, "")
        .trim();
      const firstLine = raw.split(/\r?\n/).find((line) => line.trim().length > 0)?.trim();
      return firstLine ? firstLine.slice(0, 240) : "Invalid SVG XML syntax.";
    }
    const root = doc.documentElement;
    if (!root || root.localName !== "svg" || root.namespaceURI !== "http://www.w3.org/2000/svg") {
      return "Root element must be <svg> in the SVG namespace.";
    }
    return null;
  } catch (err: unknown) {
    return err instanceof Error && err.message ? err.message : "Invalid SVG XML syntax.";
  }
}

type ImageReadOutcome =
  | { readonly kind: "ok"; readonly dataUri: string }
  | { readonly kind: "cap" }
  | { readonly kind: "missing" };

async function readSvgChildImageDataUri(
  repoPath: string,
  resolvedPath: string,
  readBlob: (repoPath: string, filePath: string) => Promise<FileBlob | null>,
  cache: Map<string, Promise<ImageReadOutcome>>,
  canvas: SvgCanvasSurface,
): Promise<ImageReadOutcome> {
  const existing = cache.get(resolvedPath);
  if (existing) return existing;
  const promise = (async (): Promise<ImageReadOutcome> => {
    try {
      const blob = await readBlob(repoPath, resolvedPath);
      if (!blob) return { kind: "missing" };
      if (isSvgFile(resolvedPath)) {
        const extracted = extractSvgBlobText(blob);
        if (extracted.hasNul || extracted.invalidBase64 || !extracted.text.trim()) {
          return { kind: "missing" };
        }
        if (utf8ByteLength(extracted.text) > MAX_SVG_INLINED_IMAGE_BYTES) return { kind: "cap" };
        // Depth-1 only: nested SVGs are sanitized & normalized without recursive reads.
        const prepared = prepareSvgPreviewDocument(extracted.text, { canvas });
        if (!prepared.dataUrl) return { kind: "missing" };
        if (prepared.normalizedXml && validateSvgXmlInDom(prepared.normalizedXml)) {
          return { kind: "missing" };
        }
        return { kind: "ok", dataUri: prepared.dataUrl };
      }
      if (blob.base64) {
        const approxBytes = Math.floor((blob.base64.length * 3) / 4);
        if (approxBytes > MAX_SVG_INLINED_IMAGE_BYTES) return { kind: "cap" };
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
 * Resolves bounded repository-local `<image>` and `<feImage>` references
 * inside `rawSvg` so composite SVGs render inside an isolated `<img>` data URI.
 */
export async function inlineLocalSvgAssets(
  rawSvg: string,
  location: { readonly repoPath: string; readonly filePath: string },
  readBlob: (repoPath: string, filePath: string) => Promise<FileBlob | null>,
  options: { readonly canvas?: SvgCanvasSurface } = {},
): Promise<InlinedSvgAssetResult> {
  const stats = analyzeSvgDocument(rawSvg);
  const images: Record<string, string> = {};
  const missingAssets: string[] = [];
  let skippedCapCount = 0;

  if (stats.byteCount > MAX_SVG_PREVIEW_BYTES || !location.repoPath || !location.filePath) {
    return {
      images,
      inlinedImageCount: 0,
      missingAssets,
      skippedCapCount,
    };
  }

  const canvas = options.canvas ?? "grid";
  const cleanOwnerPath = normalizeRepoRelativePath(location.filePath);
  const imgRefs = stats.localImageRefs.slice(0, MAX_SVG_INLINED_IMAGES);
  skippedCapCount += Math.max(0, stats.localImageRefs.length - imgRefs.length);
  const cache = new Map<string, Promise<ImageReadOutcome>>();

  await Promise.all(
    imgRefs.map(async (rawRef) => {
      const resolved = resolveRelativeRepoAsset(location.repoPath, location.filePath, rawRef);
      if (!resolved || resolved === cleanOwnerPath) {
        missingAssets.push(rawRef);
        return;
      }
      const outcome = await readSvgChildImageDataUri(
        location.repoPath,
        resolved,
        readBlob,
        cache,
        canvas,
      );
      if (outcome.kind === "ok") {
        images[rawRef] = outcome.dataUri;
      } else if (outcome.kind === "cap") {
        skippedCapCount += 1;
      } else {
        missingAssets.push(rawRef);
      }
    }),
  );

  return {
    images,
    inlinedImageCount: Object.keys(images).length,
    missingAssets,
    skippedCapCount,
  };
}

export interface SvgBlobPreviewResult {
  readonly dataUrl: string | null;
  readonly error: string | null;
}

/**
 * Converts a text- or base64-backed SVG `FileBlob` into a sanitized,
 * dimension-normalized `data:image/svg+xml;base64,...` URI and surfaces
 * parse/size/NUL errors rather than collapsing them to an absent file.
 */
export function svgBlobToPreviewResult(
  blob: { readonly text?: string | null; readonly base64?: string | null } | null | undefined,
  canvas: SvgCanvasSurface = "light",
): SvgBlobPreviewResult {
  if (!blob) return { dataUrl: null, error: null };
  const extracted = extractSvgBlobText(blob);
  if (extracted.hasNul) {
    return { dataUrl: null, error: "SVG contains NUL bytes and cannot be rendered as XML." };
  }
  if (extracted.invalidBase64) {
    return { dataUrl: null, error: "SVG base64 payload couldn't be decoded." };
  }
  if (!extracted.text.trim()) {
    return { dataUrl: null, error: "SVG document is empty." };
  }
  const prepared = prepareSvgPreviewDocument(extracted.text, { canvas });
  if (prepared.tooLarge) {
    return {
      dataUrl: null,
      error: `SVG exceeds the ${Math.round(MAX_SVG_PREVIEW_BYTES / (1024 * 1024))} MiB preview limit.`,
    };
  }
  if (prepared.parseError) {
    return { dataUrl: null, error: prepared.parseError };
  }
  const domErr = prepared.normalizedXml ? validateSvgXmlInDom(prepared.normalizedXml) : null;
  if (domErr) {
    return { dataUrl: null, error: domErr };
  }
  return {
    dataUrl: prepared.dataUrl || null,
    error: prepared.dataUrl ? null : "Couldn't encode SVG preview.",
  };
}

/**
 * Converts a text- or base64-backed SVG `FileBlob` into a sanitized,
 * dimension-normalized `data:image/svg+xml;base64,...` URI for image diffs
 * and HTML asset inlining.
 */
export function svgBlobToDataUrl(
  blob: { readonly text?: string | null; readonly base64?: string | null } | null | undefined,
  canvas: SvgCanvasSurface = "light",
): string | null {
  return svgBlobToPreviewResult(blob, canvas).dataUrl;
}
