<script lang="ts">
  import { tick, untrack } from "svelte";
  import { get } from "svelte/store";
  import {
    Eye,
    Code,
    Columns,
    Rows2,
    ExternalLink,
    Copy,
    Check,
    RotateCw,
    Maximize2,
    Scan,
    Sun,
    Moon,
    Grid3x3,
    Grid2x2,
    Link2,
    ImageDown,
    Pause,
    Play,
    Info,
    Shield,
    AlertTriangle,
    FileImage,
  } from "@lucide/svelte";
  import { invoke } from "../../ipc/invoke";
  import { createAsyncGuard, type AsyncGuard } from "../../async/guard";
  import { observeResize } from "../../dom/observeResize";
  import { openInDefaultApp } from "../../desktop/openInShell";
  import { copyImageBlob, copyText } from "../../desktop/clipboard";
  import { currentRepoPath, repoStore } from "../../stores/repoStore";
  import { describeError } from "../../ui/copy/describeFailure";
  import { consumeReveal, REVEAL_TTL_MS, revealRequest } from "../../files/revealRequests";
  import {
    fileSearchControl,
    triggerFileSearch,
    type FileSearchController,
  } from "../../files/fileSearchTarget";
  import type { FileBlob, LineEnding } from "../../files/types";
  import {
    clearSvgViewModeIfMatches,
    computeSvgDisplaySize,
    extractSvgBlobText,
    inlineLocalSvgAssets,
    isSvgFile,
    loadSvgPreviewPreferences,
    matchSvgViewMode,
    MAX_SVG_PREVIEW_BYTES,
    prepareSvgPreviewDocument,
    recalledSvgViewMode,
    rememberSvgViewMode,
    resolveSvgPreviewLayout,
    resolveSvgPreviewShortcut,
    resolveSvgSplitDirection,
    saveSvgPreviewPreferences,
    stepSvgZoom,
    SVG_ZOOM_MAX,
    SVG_ZOOM_MIN,
    svgViewModeRequest,
    validateSvgXmlInDom,
    type InlinedSvgAssetResult,
    type PreparedSvgDocument,
    type SvgCanvasSurface,
    type SvgSplitDirection,
    type SvgViewMode,
  } from "../../files/svgPreview";
  import CodeViewer from "./CodeViewer.svelte";
  import LanguageLogo from "../LanguageLogo.svelte";
  import ScrollCue from "../ScrollCue.svelte";

  let {
    filePath,
    blob,
    repoPathOverride = null,
    draftContent = null,
    dirty = false,
    onSave,
    onDraftChange,
    onRequestDiscard,
    revealTarget = null,
    onSendSelectionToAgent,
  }: {
    filePath: string;
    blob: {
      path: string;
      is_binary: boolean;
      is_image: boolean;
      mime: string;
      text?: string | null;
      base64?: string | null;
      invalid_utf8_bytes?: number;
      disk_base?: string | null;
      eol?: LineEnding | null;
    };
    repoPathOverride?: string | null;
    draftContent?: string | null;
    dirty?: boolean;
    onSave?: (newContent: string) => Promise<void>;
    onDraftChange?: (newContent: string, sourceContent: string) => void;
    onRequestDiscard?: () => Promise<boolean>;
    revealTarget?: { line: number | null; column?: number | null; seq: number } | null;
    onSendSelectionToAgent?: (reference: string) => void;
  } = $props();

  const repoPath = $derived(repoPathOverride ?? $currentRepoPath);
  const extracted = $derived(extractSvgBlobText(blob));
  const sourceContent = $derived(extracted.text);
  const rawContent = $derived(draftContent ?? sourceContent);

  const RERENDER_DELAY_MS = 150;
  const EMPTY_ASSETS: InlinedSvgAssetResult = {
    images: {},
    inlinedImageCount: 0,
    missingAssets: [],
    skippedCapCount: 0,
  };

  let lastHandledModeNonce = 0;

  function resolveInitialMode(
    repo: string | null,
    path: string,
  ): { mode: SvgViewMode; explicit: boolean } {
    const matched = matchSvgViewMode(get(svgViewModeRequest), repo, path);
    if (matched) {
      lastHandledModeNonce = matched.nonce;
      rememberSvgViewMode(repo, path, matched.mode);
      return { mode: matched.mode, explicit: true };
    }
    const recalled = recalledSvgViewMode(repo, path);
    if (recalled) return { mode: recalled, explicit: false };
    return { mode: "preview", explicit: false };
  }

  const initialChoice = untrack(() => resolveInitialMode(repoPath, filePath));
  const initialPrefs = untrack(() => loadSvgPreviewPreferences());
  let viewMode = $state<SvgViewMode>(initialChoice.mode);
  let lastHandledRevealSeq = untrack(() =>
    initialChoice.explicit && initialChoice.mode === "preview" ? (revealTarget?.seq ?? 0) : 0,
  );
  let fitMode = $state(true);
  let zoom = $state(100);
  let canvasSurface = $state<SvgCanvasSurface>(initialPrefs.canvasSurface);
  let showBounds = $state(initialPrefs.showBounds);
  let liveAutoUpdate = $state(true);
  let showInspector = $state(false);
  let copiedKind = $state<"source" | "data-uri" | "png" | null>(null);
  let splitRatio = $state(50);
  let splitVerticalRatio = $state(50);
  let preferredSplitDirection = $state<SvgSplitDirection>(initialPrefs.preferredSplitDirection);
  let isDraggingSplit = $state(false);
  let isPanning = $state(false);
  let refreshNonce = $state(0);
  let isRefreshing = $state(false);
  let imgLoadError = $state<string | null>(null);

  let viewerRootEl: HTMLDivElement | undefined = $state();
  let svgBar: HTMLDivElement | undefined = $state();
  let subBar: HTMLDivElement | undefined = $state();
  let viewportEl: HTMLDivElement | undefined = $state();
  let stageEl: HTMLDivElement | undefined = $state();
  let previewImgEl: HTMLImageElement | undefined = $state();
  let splitHostEl: HTMLDivElement | undefined = $state();
  let viewportWidth = $state(0);
  let stageWidth = $state(0);
  let stageHeight = $state(0);
  let copiedTimer: ReturnType<typeof setTimeout> | null = null;
  let stopSplitDrag: (() => void) | null = null;
  let stopPanDrag: (() => void) | null = null;

  /** Total chrome (p-3 = 24px + 1px border × 2 = 26px + 2px subpixel guard) around the artboard inside the stage content box. */
  const STAGE_SURFACE_CHROME_PX = 28;
  const PNG_EXPORT_MAX_DIM = 4096;

  let previewedContent = $state(untrack(() => rawContent));
  let inlinedAssets = $state<InlinedSvgAssetResult>(EMPTY_ASSETS);
  let shownFileKey: string | null = null;
  let lastAssetKey = "";
  let assetGuard: AsyncGuard | null = null;

  const hasPendingPausedEdit = $derived(!liveAutoUpdate && previewedContent !== rawContent);

  function setCanvasSurface(next: SvgCanvasSurface) {
    canvasSurface = next;
    saveSvgPreviewPreferences({ canvasSurface: next });
  }

  function toggleBounds() {
    showBounds = !showBounds;
    saveSvgPreviewPreferences({ showBounds });
  }

  function toggleSplitDirection() {
    const next: SvgSplitDirection = preferredSplitDirection === "horizontal" ? "vertical" : "horizontal";
    preferredSplitDirection = next;
    saveSvgPreviewPreferences({ preferredSplitDirection: next });
  }

  function setMode(next: SvgViewMode, fromUser = true) {
    viewMode = next;
    rememberSvgViewMode(repoPath, filePath, next);
    if (fromUser) {
      clearSvgViewModeIfMatches(repoPath, filePath);
    }
  }

  // Apply one-shot mode overrides (right-click "Open as Preview" / "Open Code & Preview" /
  // "Open in editor") non-destructively via nonce tracking so concurrently mounted hosts
  // never steal a right-click request from one another.
  $effect(() => {
    const req = $svgViewModeRequest;
    const repo = repoPath;
    const file = filePath;
    if (!req) return;
    untrack(() => {
      const matched = matchSvgViewMode(req, repo, file);
      if (matched && matched.nonce > lastHandledModeNonce) {
        lastHandledModeNonce = matched.nonce;
        setMode(matched.mode, false);
        if (matched.mode === "preview") {
          lastHandledRevealSeq = Math.max(lastHandledRevealSeq, revealTarget?.seq ?? 0);
          consumeReveal(file);
        }
      }
    });
  });

  // Switching to a different SVG file or repository recalls its last mode (or defaults
  // to preview) unless an explicit mode request is waiting, and resets zoom/fit state.
  let trackedFileKey = untrack(() => `${repoPath ?? ""}\0${filePath}`);
  $effect(() => {
    const repo = repoPath;
    const file = filePath;
    const key = `${repo ?? ""}\0${file}`;
    if (key === trackedFileKey) return;
    trackedFileKey = key;
    untrack(() => {
      const choice = resolveInitialMode(repo, file);
      viewMode = choice.mode;
      rememberSvgViewMode(repo, file, choice.mode);
      lastHandledRevealSeq =
        choice.explicit && choice.mode === "preview" ? (revealTarget?.seq ?? 0) : 0;
      fitMode = true;
      zoom = 100;
      imgLoadError = null;
    });
  });

  // A fresh line reveal (terminal link with `:line` or search jump) switches out of
  // pure Preview so CodeViewer can scroll to the requested line; Split already mounts it.
  $effect(() => {
    const req = $revealRequest;
    const target = revealTarget;
    const file = filePath;
    untrack(() => {
      const hasFreshTarget =
        target !== null && target.line !== null && target.seq > lastHandledRevealSeq;
      const now = Date.now();
      const isStoreRevealExpired = req !== null && now - req.at > REVEAL_TTL_MS;
      if (isStoreRevealExpired) {
        consumeReveal(file, now);
      }
      const hasStoreReveal = req !== null && !isStoreRevealExpired && req.path === file;
      if (!hasFreshTarget && !hasStoreReveal) return;
      if (hasFreshTarget && target) {
        lastHandledRevealSeq = target.seq;
      }
      if (viewMode === "preview") {
        clearSvgViewModeIfMatches(repoPath, file);
        setMode("code", false);
      }
    });
  });

  // Debounce live SVG updates while typing in Split mode; update immediately on
  // file/worktree switch, first load, or manual Refresh.
  $effect(() => {
    const content = rawContent;
    const repo = repoPath;
    const file = filePath;
    const live = liveAutoUpdate;
    const fileKey = `${repo ?? ""}\0${file}`;
    if (shownFileKey !== fileKey) {
      shownFileKey = fileKey;
      previewedContent = content;
      inlinedAssets = EMPTY_ASSETS;
      lastAssetKey = "";
      return;
    }
    if (!live) return;
    const timer = setTimeout(() => {
      previewedContent = content;
    }, RERENDER_DELAY_MS);
    return () => clearTimeout(timer);
  });

  // Key the preview `<img>` by file identity + `refreshNonce` so clicking Refresh
  // always forces a clean image reload even when the SVG markup is unchanged.
  const imgKey = $derived(`${repoPath ?? ""}\0${filePath}\0${refreshNonce}`);

  const prepared = $derived<PreparedSvgDocument>(
    prepareSvgPreviewDocument(previewedContent, {
      canvas: canvasSurface,
      inlinedImages: inlinedAssets.images,
    }),
  );

  const domXmlError = $derived(
    !prepared.tooLarge && !prepared.empty && !prepared.parseError && prepared.normalizedXml
      ? validateSvgXmlInDom(prepared.normalizedXml)
      : null,
  );

  $effect(() => {
    void prepared.dataUrl;
    void refreshNonce;
    untrack(() => {
      imgLoadError = null;
    });
  });

  const effectiveParseError = $derived(
    extracted.hasNul
      ? "SVG contains NUL bytes and cannot be rendered as XML."
      : extracted.invalidBase64
        ? "SVG base64 payload couldn't be decoded."
        : (prepared.parseError ?? domXmlError ?? imgLoadError),
  );

  const hasLocalSvgRef = $derived(
    prepared.stats.localImageRefs.some((ref) => isSvgFile(ref)),
  );

  // Include `blob.disk_base` so saves and disk reloads re-read local `<image>` assets
  // automatically without re-fetching per keystroke; include `canvasSurface` only when
  // a child `.svg` image needs matching `currentColor` ink.
  const assetSignature = $derived(
    `${repoPath ?? ""}\0${filePath}\0${blob.disk_base ?? ""}\0${refreshNonce}\0${hasLocalSvgRef ? canvasSurface : ""}\0${prepared.stats.localImageRefs.join("|")}`,
  );

  $effect(() => {
    const sig = assetSignature;
    const repo = repoPath;
    const file = filePath;
    const svg = previewedContent;
    const canvas = canvasSurface;
    const hasLocalRefs = prepared.stats.localImageRefs.length > 0;
    if (!hasLocalRefs || !repo || !file || prepared.tooLarge || prepared.empty) {
      assetGuard?.cancel();
      if (lastAssetKey !== sig) {
        lastAssetKey = sig;
        inlinedAssets = EMPTY_ASSETS;
      }
      isRefreshing = false;
      return;
    }
    if (sig === lastAssetKey) return;
    lastAssetKey = sig;
    assetGuard?.cancel();
    const guard = createAsyncGuard();
    assetGuard = guard;
    isRefreshing = true;
    void inlineLocalSvgAssets(
      svg,
      { repoPath: repo, filePath: file },
      async (r, p) => {
        try {
          return await invoke<FileBlob>("cmd_get_file_blob", {
            repoPath: r,
            filePath: p,
            commitId: null,
            withDigest: false,
          });
        } catch {
          return null;
        }
      },
      { canvas },
    ).then((result) => {
      if (!guard.isLive()) return;
      inlinedAssets = result;
      isRefreshing = false;
    });
    return () => guard.cancel();
  });

  $effect(() => {
    const el = viewportEl;
    if (!el) return;
    const sync = (w: number) => {
      viewportWidth = Number.isFinite(w) && w > 0 ? w : 0;
    };
    sync(el.clientWidth);
    return observeResize(el, (entries) => {
      const rect = entries.find((entry) => entry.target === el)?.contentRect ?? entries[0]?.contentRect;
      if (rect) sync(rect.width);
    });
  });

  function readElementContentBox(el: HTMLElement): { readonly width: number; readonly height: number } {
    const style = getComputedStyle(el);
    const padX = (Number.parseFloat(style.paddingLeft) || 0) + (Number.parseFloat(style.paddingRight) || 0);
    const padY = (Number.parseFloat(style.paddingTop) || 0) + (Number.parseFloat(style.paddingBottom) || 0);
    return {
      width: Math.max(0, el.clientWidth - padX),
      height: Math.max(0, el.clientHeight - padY),
    };
  }

  function syncStageDimensions(w: number, h: number) {
    const nextW = Number.isFinite(w) && w > 0 ? Math.floor(w) : 0;
    const nextH = Number.isFinite(h) && h > 0 ? Math.floor(h) : 0;
    if (nextW !== stageWidth) stageWidth = nextW;
    if (nextH !== stageHeight) stageHeight = nextH;
  }

  $effect(() => {
    const el = stageEl;
    if (!el) return;
    const initialBox = readElementContentBox(el);
    untrack(() => syncStageDimensions(initialBox.width, initialBox.height));
    return observeResize(el, (entries) => {
      const rect = entries.find((entry) => entry.target === el)?.contentRect ?? entries[0]?.contentRect;
      if (rect) syncStageDimensions(rect.width, rect.height);
    });
  });

  // Re-measure the stage when sibling chrome (inspector notes, paused banner,
  // toolbar stack, or split ratio) changes height: WebKitGTK 2.50.4 can omit
  // ResizeObserver delivery on a nested flex child inside a CSS container when
  // only a sibling's intrinsic height changes.
  $effect(() => {
    const el = stageEl;
    if (!el) return;
    void showInspector;
    void hasPendingPausedEdit;
    void prepared;
    void inlinedAssets;
    void effectiveParseError;
    void layout.toolbar;
    void effectiveSplitDirection;
    void splitRatio;
    void splitVerticalRatio;
    const measure = () => {
      const box = readElementContentBox(el);
      syncStageDimensions(box.width, box.height);
    };
    untrack(measure);
    if (typeof requestAnimationFrame !== "function") return;
    let rafId = requestAnimationFrame(() => {
      rafId = 0;
      measure();
    });
    return () => {
      if (rafId) cancelAnimationFrame(rafId);
    };
  });

  // In Preview mode, ⌘F / ⌘G opens Split mode (keeping live preview beside source)
  // and delegates immediately to CodeViewer's inline Find / Go-to-Line bar.
  const previewSearchController = $derived<FileSearchController | null>(
    viewMode === "preview"
      ? {
          canGoToLine: true,
          openFind(seed?: string) {
            setMode("split");
            void tick().then(() => {
              if (viewerRootEl) triggerFileSearch(viewerRootEl, "find", seed);
            });
          },
          openLine() {
            setMode("split");
            void tick().then(() => {
              if (viewerRootEl) triggerFileSearch(viewerRootEl, "line");
            });
          },
        }
      : null,
  );

  const layout = $derived(resolveSvgPreviewLayout(viewportWidth));
  const splitChoice = $derived(resolveSvgSplitDirection(viewportWidth, preferredSplitDirection));
  const effectiveSplitDirection = $derived(splitChoice.direction);
  const canToggleSplitDirection = $derived(splitChoice.canToggle);
  const modeButtonClass = $derived(
    layout.toolbar === "stacked"
      ? "gp-seg-btn flex flex-1 min-w-0 items-center justify-center gap-1 px-2!"
      : "gp-seg-btn flex items-center gap-1 @md/svgview:gap-1.5 px-2! @md/svgview:px-2.5!",
  );
  const modeLabelClass = $derived(layout.toolbar === "stacked" ? "hidden @3xs/svgview:inline truncate" : "");

  const displaySize = $derived(
    computeSvgDisplaySize({
      intrinsicWidth: prepared.stats.intrinsicWidth,
      intrinsicHeight: prepared.stats.intrinsicHeight,
      stageWidth,
      stageHeight,
      fit: fitMode,
      zoom,
      inset: STAGE_SURFACE_CHROME_PX,
    }),
  );

  const canRenderStageImage = $derived(
    !prepared.tooLarge && !prepared.empty && !effectiveParseError && Boolean(prepared.dataUrl),
  );

  const canPan = $derived(
    !fitMode &&
      canRenderStageImage &&
      stageWidth > 0 &&
      stageHeight > 0 &&
      (displaySize.width + 26 > stageWidth || displaySize.height + 26 > stageHeight),
  );

  const shownPercent = $derived(fitMode ? displaySize.effectivePercent : zoom);
  const totalInlinedImages = $derived(
    Math.max(prepared.inlinedImageCount, inlinedAssets.inlinedImageCount),
  );
  const sanitizedCount = $derived(
    prepared.stats.scriptCount +
      prepared.stats.eventHandlerCount +
      prepared.stats.unsafeHrefCount +
      prepared.stats.unsafeSmilCount +
      prepared.stats.strippedStylesheetPiCount +
      (prepared.stats.strippedDtd ? 1 : 0),
  );
  const inspectorNoticeCount = $derived(
    sanitizedCount +
      prepared.stats.externalRefCount +
      inlinedAssets.missingAssets.length +
      inlinedAssets.skippedCapCount +
      (effectiveParseError ? 1 : 0),
  );

  function refreshPreview() {
    previewedContent = rawContent;
    lastAssetKey = "";
    imgLoadError = null;
    refreshNonce += 1;
  }

  function toggleLiveUpdates() {
    liveAutoUpdate = !liveAutoUpdate;
    if (liveAutoUpdate && previewedContent !== rawContent) {
      previewedContent = rawContent;
    }
  }

  /** The zoom the stage shows now: the fitted percent while fitting, else the set zoom. */
  function currentZoomPercent(): number {
    return fitMode
      ? Math.max(SVG_ZOOM_MIN, Math.min(SVG_ZOOM_MAX, displaySize.effectivePercent))
      : zoom;
  }

  function handleStepZoom(delta: 1 | -1) {
    const current = currentZoomPercent();
    fitMode = false;
    zoom = stepSvgZoom(current, delta);
  }

  // Ctrl/⌘ + wheel (and trackpad pinch) zoom. A plain handler rather than a
  // closure inside the stage effect: it reads and writes fitMode/zoom, and
  // only ever runs from an event, never while that effect is tracking.
  function handleStageWheel(event: WheelEvent) {
    if (!event.ctrlKey && !event.metaKey) return;
    if (!canRenderStageImage) return;
    event.preventDefault();
    if (event.deltaY === 0) return;
    const current = currentZoomPercent();
    fitMode = false;
    if (Math.abs(event.deltaY) >= 40) {
      zoom = stepSvgZoom(current, event.deltaY < 0 ? 1 : -1);
    } else {
      const factor = Math.exp(-event.deltaY * 0.01);
      let next = Math.round(current * factor);
      if (next === current) next = current + (event.deltaY < 0 ? 1 : -1);
      zoom = Math.max(SVG_ZOOM_MIN, Math.min(SVG_ZOOM_MAX, next));
    }
  }

  function resetZoomActual() {
    fitMode = false;
    zoom = 100;
  }

  // Direct stage manipulation: Ctrl/⌘ + wheel (and trackpad pinch) zoom + click-drag pan.
  $effect(() => {
    const el = stageEl;
    if (!el) return;

    const onPointerDown = (event: PointerEvent) => {
      if (event.button !== 0) return;
      const target = event.target as HTMLElement | null;
      if (target?.closest("button, a, input, textarea, select")) return;
      // Preserve native classic scrollbar dragging when scrollbars occupy layout space.
      const rect = el.getBoundingClientRect();
      const onVerticalScrollbar =
        el.offsetWidth > el.clientWidth + el.clientLeft &&
        event.clientX >= rect.left + el.clientLeft + el.clientWidth;
      const onHorizontalScrollbar =
        el.offsetHeight > el.clientHeight + el.clientTop &&
        event.clientY >= rect.top + el.clientTop + el.clientHeight;
      if (onVerticalScrollbar || onHorizontalScrollbar) return;
      el.focus({ preventScroll: true });
      const overflows =
        canPan || el.scrollWidth > el.clientWidth + 1 || el.scrollHeight > el.clientHeight + 1;
      if (!overflows || !canRenderStageImage) return;
      event.preventDefault();
      stopPanDrag?.();
      const startX = event.clientX;
      const startY = event.clientY;
      const startScrollLeft = el.scrollLeft;
      const startScrollTop = el.scrollTop;
      isPanning = true;
      const onMove = (moveEvent: PointerEvent) => {
        el.scrollLeft = startScrollLeft - (moveEvent.clientX - startX);
        el.scrollTop = startScrollTop - (moveEvent.clientY - startY);
      };
      const cleanup = () => {
        isPanning = false;
        window.removeEventListener("pointermove", onMove);
        window.removeEventListener("pointerup", cleanup);
        window.removeEventListener("pointercancel", cleanup);
        if (stopPanDrag === cleanup) stopPanDrag = null;
      };
      stopPanDrag = cleanup;
      window.addEventListener("pointermove", onMove);
      window.addEventListener("pointerup", cleanup);
      window.addEventListener("pointercancel", cleanup);
    };

    el.addEventListener("wheel", handleStageWheel, { passive: false });
    el.addEventListener("pointerdown", onPointerDown);
    return () => {
      el.removeEventListener("wheel", handleStageWheel);
      el.removeEventListener("pointerdown", onPointerDown);
      stopPanDrag?.();
    };
  });

  // Scoped preview keyboard shortcuts: +/= zoom in, - zoom out, 0 Fit, 1 1:1, b Bounds.
  $effect(() => {
    const root = viewerRootEl;
    if (!root) return;
    const onKeydown = (event: KeyboardEvent) => {
      if (event.defaultPrevented || viewMode === "code") return;
      const target = event.target as HTMLElement | null;
      if (
        target &&
        (target.isContentEditable ||
          target.tagName === "INPUT" ||
          target.tagName === "TEXTAREA" ||
          target.tagName === "SELECT" ||
          Boolean(target.closest('[contenteditable="true"], [contenteditable=""], .cm-editor, [data-svg-split-code]')))
      ) {
        return;
      }
      const action = resolveSvgPreviewShortcut(event);
      if (!action) return;
      event.preventDefault();
      if (action === "zoom-in") handleStepZoom(1);
      else if (action === "zoom-out") handleStepZoom(-1);
      else if (action === "fit") fitMode = true;
      else if (action === "actual") resetZoomActual();
      else if (action === "bounds") toggleBounds();
    };
    root.addEventListener("keydown", onKeydown);
    return () => root.removeEventListener("keydown", onKeydown);
  });

  function flashCopied(kind: "source" | "data-uri" | "png") {
    copiedKind = kind;
    if (copiedTimer) clearTimeout(copiedTimer);
    copiedTimer = setTimeout(() => {
      copiedTimer = null;
      copiedKind = null;
    }, 1800);
  }

  async function handleCopySource() {
    if (!(await copyText(rawContent))) {
      repoStore.setError("Couldn't copy to clipboard");
      return;
    }
    flashCopied("source");
  }

  async function handleCopyDataUri() {
    if (!canRenderStageImage) return;
    if (!(await copyText(prepared.dataUrl))) {
      repoStore.setError("Couldn't copy data URI to clipboard");
      return;
    }
    flashCopied("data-uri");
  }

  function rasterizeCurrentSvgToPngBlob(
    dataUrl: string,
    intrinsicWidth: number,
    intrinsicHeight: number,
    existingImg: HTMLImageElement | undefined,
  ): Promise<Blob> {
    return new Promise<Blob>((resolve, reject) => {
      const renderImage = (img: HTMLImageElement) => {
        try {
          const baseW = Math.max(1, intrinsicWidth || img.naturalWidth || 300);
          const baseH = Math.max(1, intrinsicHeight || img.naturalHeight || 150);
          const dpr = typeof window !== "undefined" ? Math.min(2, Math.max(1, window.devicePixelRatio || 1)) : 1;
          const clampedScale = Math.min(dpr, PNG_EXPORT_MAX_DIM / Math.max(baseW, baseH));
          const outW = Math.max(1, Math.round(baseW * clampedScale));
          const outH = Math.max(1, Math.round(baseH * clampedScale));
          const canvas = document.createElement("canvas");
          canvas.width = outW;
          canvas.height = outH;
          const ctx = canvas.getContext("2d");
          if (!ctx) {
            reject(new Error("Canvas 2D context unavailable"));
            return;
          }
          ctx.drawImage(img, 0, 0, outW, outH);
          canvas.toBlob((blob) => {
            if (blob) resolve(blob);
            else reject(new Error("PNG rasterization produced an empty image"));
          }, "image/png");
        } catch (err) {
          reject(err instanceof Error ? err : new Error(String(err)));
        }
      };

      if (existingImg && existingImg.complete && existingImg.naturalWidth > 0 && existingImg.src === dataUrl) {
        renderImage(existingImg);
        return;
      }

      const offscreen = new Image();
      offscreen.onload = () => renderImage(offscreen);
      offscreen.onerror = () => reject(new Error("Couldn't decode SVG for PNG export"));
      offscreen.src = dataUrl;
    });
  }

  async function handleCopyPng() {
    if (!canRenderStageImage) return;
    const pngPromise = rasterizeCurrentSvgToPngBlob(
      prepared.dataUrl,
      prepared.stats.intrinsicWidth,
      prepared.stats.intrinsicHeight,
      previewImgEl,
    );
    if (!(await copyImageBlob(pngPromise, "image/png"))) {
      repoStore.setError("Couldn't copy PNG image to clipboard");
      return;
    }
    flashCopied("png");
  }

  async function openExternally() {
    const repo = repoPath;
    if (!repo) return;
    try {
      await openInDefaultApp(repo, filePath);
    } catch (err) {
      repoStore.setError(describeError(err, "repo"));
    }
  }

  function formatBytes(bytes: number): string {
    if (bytes < 1024) return `${bytes} B`;
    return `${(bytes / 1024).toFixed(1)} KiB`;
  }

  function formatDimension(value: number): string {
    return Number.isInteger(value) ? String(value) : String(Math.round(value * 10) / 10);
  }

  function startSplitDrag(event: PointerEvent, axis: "horizontal" | "vertical") {
    if (event.button !== 0 || !splitHostEl) return;
    event.preventDefault();
    stopSplitDrag?.();
    const host = splitHostEl;
    isDraggingSplit = true;
    const onMove = (moveEvent: PointerEvent) => {
      const rect = host.getBoundingClientRect();
      if (axis === "horizontal") {
        if (rect.width <= 0) return;
        const pct = ((moveEvent.clientX - rect.left) / rect.width) * 100;
        splitRatio = Math.max(25, Math.min(75, Math.round(pct * 10) / 10));
      } else {
        if (rect.height <= 0) return;
        const pct = ((moveEvent.clientY - rect.top) / rect.height) * 100;
        splitVerticalRatio = Math.max(25, Math.min(75, Math.round(pct * 10) / 10));
      }
    };
    const cleanup = () => {
      isDraggingSplit = false;
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", cleanup);
      window.removeEventListener("pointercancel", cleanup);
      if (stopSplitDrag === cleanup) stopSplitDrag = null;
    };
    stopSplitDrag = cleanup;
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", cleanup);
    window.addEventListener("pointercancel", cleanup);
  }

  $effect(() => {
    return () => {
      assetGuard?.cancel();
      stopSplitDrag?.();
      stopPanDrag?.();
      if (copiedTimer) clearTimeout(copiedTimer);
    };
  });

  function handleSplitSashKeydown(event: KeyboardEvent, axis: "horizontal" | "vertical") {
    if (axis === "horizontal") {
      if (event.key === "ArrowLeft") {
        event.preventDefault();
        splitRatio = Math.max(25, splitRatio - 5);
      } else if (event.key === "ArrowRight") {
        event.preventDefault();
        splitRatio = Math.min(75, splitRatio + 5);
      } else if (event.key === "Home") {
        event.preventDefault();
        splitRatio = 25;
      } else if (event.key === "End") {
        event.preventDefault();
        splitRatio = 75;
      }
    } else {
      if (event.key === "ArrowUp") {
        event.preventDefault();
        splitVerticalRatio = Math.max(25, splitVerticalRatio - 5);
      } else if (event.key === "ArrowDown") {
        event.preventDefault();
        splitVerticalRatio = Math.min(75, splitVerticalRatio + 5);
      } else if (event.key === "Home") {
        event.preventDefault();
        splitVerticalRatio = 25;
      } else if (event.key === "End") {
        event.preventDefault();
        splitVerticalRatio = 75;
      }
    }
  }
</script>

{#snippet modeSwitch()}
  <div class="gp-segmented" role="group" aria-label="SVG view mode" data-svg-modes={layout.toolbar}>
    <button
      type="button"
      onclick={() => setMode("preview")}
      aria-pressed={viewMode === "preview"}
      aria-label="Preview"
      class={modeButtonClass}
      data-active={viewMode === "preview"}
      data-testid="svg-mode-preview"
      title="Rendered SVG preview"
    >
      <Eye size={12} />
      <span class={modeLabelClass}>Preview</span>
    </button>

    <button
      type="button"
      onclick={() => setMode("split")}
      aria-pressed={viewMode === "split"}
      aria-label="Code and Preview"
      class={modeButtonClass}
      data-active={viewMode === "split"}
      data-testid="svg-mode-split"
      title={effectiveSplitDirection === "vertical"
        ? "Split view: SVG code above rendered preview"
        : "Split view: SVG code and rendered preview side by side"}
    >
      {#if effectiveSplitDirection === "vertical"}
        <Rows2 size={12} />
      {:else}
        <Columns size={12} />
      {/if}
      <span class={modeLabelClass}>Split</span>
    </button>

    <button
      type="button"
      onclick={() => setMode("code")}
      aria-pressed={viewMode === "code"}
      aria-label="Code"
      class={modeButtonClass}
      data-active={viewMode === "code"}
      data-testid="svg-mode-code"
      title="SVG source editor with line numbers and find"
    >
      <Code size={12} />
      <span class={modeLabelClass}>Code</span>
    </button>
  </div>
{/snippet}

{#snippet previewFrame()}
  <div
    class="flex-1 flex flex-col min-h-0 min-w-0 overflow-hidden bg-background relative"
    data-svg-preview-pane
  >
    <!-- Preview Controls Bar (fit, 1:1, zoom, bounds, canvas surface) -->
    <div class="relative shrink-0 select-none" data-tip-place="above" data-svg-controls-bar>
      <div bind:this={subBar} class="gp-header-scroll">
        <div class="flex items-center justify-between gap-2 px-2.5 py-1 min-w-max w-full">
          <div class="flex items-center gap-1.5 shrink-0">
            <button
              type="button"
              onclick={() => (fitMode = true)}
              aria-pressed={fitMode}
              aria-label="Fit SVG to preview pane"
              title="Scale SVG to fit the preview pane (0)"
              data-testid="svg-zoom-fit"
              class="gp-btn px-1.5! py-0.5! text-micro flex items-center gap-1 {fitMode
                ? 'border-accent/65 bg-accent/15 text-accent font-medium'
                : ''}"
              data-size="sm"
            >
              <Maximize2 size={11} />
              <span>Fit</span>
            </button>

            <button
              type="button"
              onclick={resetZoomActual}
              aria-pressed={!fitMode && zoom === 100}
              aria-label="Actual 1:1 size"
              title="Display SVG at intrinsic 1:1 size (1)"
              data-testid="svg-zoom-actual"
              class="gp-btn px-1.5! py-0.5! text-micro flex items-center gap-1 {!fitMode && zoom === 100
                ? 'border-accent/65 bg-accent/15 text-accent font-medium'
                : ''}"
              data-size="sm"
            >
              <span>1:1</span>
            </button>

            <div class="flex items-center rounded-full border border-edge bg-surface px-1 gap-0.5" role="group" aria-label="SVG zoom">
              <button
                type="button"
                onclick={() => handleStepZoom(-1)}
                disabled={!fitMode && zoom <= SVG_ZOOM_MIN}
                aria-label="Zoom out SVG"
                title="Zoom out (-)"
                data-testid="svg-zoom-out"
                class="gp-icon-btn text-xs"
                data-size="sm"
              >−</button>
              <button
                type="button"
                onclick={resetZoomActual}
                aria-label="Reset SVG zoom to 100%"
                title="Reset zoom to 100% (1)"
                data-testid="svg-zoom-reset"
                class="text-micro font-mono text-textMuted hover:text-textPrimary min-w-10 text-center tabular-nums"
              >{shownPercent}%</button>
              <button
                type="button"
                onclick={() => handleStepZoom(1)}
                disabled={!fitMode && zoom >= SVG_ZOOM_MAX}
                aria-label="Zoom in SVG"
                title="Zoom in (+)"
                data-testid="svg-zoom-in"
                class="gp-icon-btn text-xs"
                data-size="sm"
              >+</button>
            </div>

            <span class="text-micro font-mono text-textMuted tabular-nums shrink-0" data-testid="svg-dimensions">
              {formatDimension(prepared.stats.intrinsicWidth)} × {formatDimension(prepared.stats.intrinsicHeight)} px
            </span>
          </div>

          <div class="flex items-center gap-1.5 shrink-0">
            <button
              type="button"
              onclick={toggleBounds}
              aria-pressed={showBounds}
              aria-label="{showBounds ? 'Hide' : 'Show'} SVG bounding box"
              title={showBounds ? "Hide SVG artboard bounds (B)" : "Show SVG artboard bounds (B)"}
              data-testid="svg-bounds-toggle"
              class="gp-btn px-1.5! py-0.5! text-micro flex items-center gap-1 {showBounds
                ? 'border-accent/65 bg-accent/15 text-accent font-medium'
                : ''}"
              data-size="sm"
            >
              <Scan size={11} />
              <span>Bounds</span>
            </button>

            <div class="flex items-center rounded-full border border-edge bg-surface p-0.5 gap-0.5" role="group" aria-label="SVG canvas surface">
              <button
                type="button"
                onclick={() => setCanvasSurface("light")}
                aria-pressed={canvasSurface === "light"}
                aria-label="Light SVG canvas"
                title="Light paper canvas"
                data-testid="svg-canvas-light"
                class="gp-icon-btn {canvasSurface === 'light' ? 'text-accent bg-accent/15' : ''}"
                data-size="sm"
              >
                <Sun size={11} />
              </button>
              <button
                type="button"
                onclick={() => setCanvasSurface("dark")}
                aria-pressed={canvasSurface === "dark"}
                aria-label="Dark SVG canvas"
                title="Dark canvas"
                data-testid="svg-canvas-dark"
                class="gp-icon-btn {canvasSurface === 'dark' ? 'text-accent bg-accent/15' : ''}"
                data-size="sm"
              >
                <Moon size={11} />
              </button>
              <button
                type="button"
                onclick={() => setCanvasSurface("grid")}
                aria-pressed={canvasSurface === "grid"}
                aria-label="Light transparency grid canvas"
                title="Light transparency grid canvas"
                data-testid="svg-canvas-grid"
                class="gp-icon-btn {canvasSurface === 'grid' ? 'text-accent bg-accent/15' : ''}"
                data-size="sm"
              >
                <Grid3x3 size={11} />
              </button>
              <button
                type="button"
                onclick={() => setCanvasSurface("dark-grid")}
                aria-pressed={canvasSurface === "dark-grid"}
                aria-label="Dark transparency grid canvas"
                title="Dark transparency grid canvas"
                data-testid="svg-canvas-dark-grid"
                class="gp-icon-btn {canvasSurface === 'dark-grid' ? 'text-accent bg-accent/15' : ''}"
                data-size="sm"
              >
                <Grid2x2 size={11} />
              </button>
            </div>

            {#if sanitizedCount > 0}
              <button
                type="button"
                onclick={() => (showInspector = !showInspector)}
                class="gp-chip font-mono cursor-pointer"
                data-tone="warn"
                data-testid="svg-sanitized-badge"
                title="Active content was stripped in preview for safety. Click for details."
              >
                <Shield size={10} />
                <span>{sanitizedCount} {sanitizedCount === 1 ? "item" : "items"} stripped</span>
              </button>
            {/if}
          </div>
        </div>
      </div>
      <ScrollCue target={subBar} axis="x" />
    </div>

    {#if hasPendingPausedEdit}
      <div class="shrink-0 mx-2.5 mb-1.5 gp-notice text-meta flex items-center justify-between gap-2" data-tone="warn" role="status" data-testid="svg-paused-banner">
        <span>Live preview is paused — edits are not yet reflected.</span>
        <div class="flex items-center gap-1 shrink-0">
          <button type="button" class="gp-btn text-micro" data-size="sm" data-testid="svg-paused-refresh" onclick={refreshPreview}>
            Refresh now
          </button>
          <button type="button" class="gp-btn text-micro" data-variant="primary" data-size="sm" data-testid="svg-paused-resume" onclick={toggleLiveUpdates}>
            Resume live
          </button>
        </div>
      </div>
    {/if}

    <!-- Preview Stage (`m-auto` inside `overflow-auto` keeps top-left reachable when zoomed) -->
    <div
      bind:this={stageEl}
      tabindex="-1"
      class="flex-1 min-h-0 min-w-0 overflow-auto gp-scroll flex p-6 bg-background/80 focus:outline-none {isPanning
        ? 'cursor-grabbing select-none'
        : canPan
          ? 'cursor-grab'
          : ''}"
      data-svg-stage
      data-svg-pannable={canPan ? "true" : "false"}
      data-svg-panning={isPanning ? "true" : "false"}
    >
      {#if prepared.tooLarge}
        <div
          class="m-auto flex flex-col items-center justify-center p-6 text-center gap-3 max-w-md"
          data-testid="svg-too-large"
        >
          <AlertTriangle size={24} class="text-warn" />
          <div class="space-y-1">
            <p class="text-sm font-semibold text-textPrimary">This SVG file is too large for inline live preview</p>
            <p class="text-meta text-textMuted">
              File size is {formatBytes(prepared.stats.byteCount)} (limit {formatBytes(MAX_SVG_PREVIEW_BYTES)}).
              Inspect it in Code view or open it in an external application.
            </p>
          </div>
          <div class="flex items-center gap-2">
            <button type="button" class="gp-btn text-meta" data-size="sm" onclick={() => setMode("code")}>
              <Code size={12} />
              <span>Open Code</span>
            </button>
            <button type="button" class="gp-btn text-meta" data-variant="primary" data-size="sm" onclick={openExternally}>
              <ExternalLink size={12} />
              <span>Open in app</span>
            </button>
          </div>
        </div>
      {:else if effectiveParseError}
        <div
          class="m-auto flex flex-col items-center justify-center p-6 text-center gap-3 max-w-md"
          role="alert"
          data-testid="svg-parse-error"
        >
          <AlertTriangle size={24} class="text-danger" />
          <div class="space-y-1">
            <p class="text-sm font-semibold text-textPrimary">Couldn't render SVG markup</p>
            <p class="text-meta font-mono text-textMuted wrap-break-word">{effectiveParseError}</p>
          </div>
          {#if viewMode === "preview"}
            <div class="flex items-center gap-2">
              <button type="button" class="gp-btn text-meta" data-size="sm" onclick={() => setMode("split")}>
                <Columns size={12} />
                <span>Edit in Split</span>
              </button>
              <button type="button" class="gp-btn text-meta" data-size="sm" onclick={() => setMode("code")}>
                <Code size={12} />
                <span>Open Code</span>
              </button>
            </div>
          {/if}
        </div>
      {:else if prepared.empty}
        <div
          class="m-auto flex flex-col items-center justify-center p-6 text-center gap-3 max-w-md"
          data-testid="svg-empty-state"
        >
          <FileImage size={24} class="text-textMuted" />
          <div class="space-y-1">
            <p class="text-sm font-semibold text-textPrimary">Empty SVG document</p>
            <p class="text-meta text-textMuted">
              Add an <code class="font-mono">&lt;svg&gt;</code> element in Code or Split view to render a live preview.
            </p>
          </div>
          {#if viewMode === "preview"}
            <button type="button" class="gp-btn text-meta" data-variant="primary" data-size="sm" onclick={() => setMode("split")}>
              <Columns size={12} />
              <span>Open Split</span>
            </button>
          {/if}
        </div>
      {:else}
        <div
          class="m-auto shrink-0 rounded-xl border border-edgeStrong shadow-card p-3 flex items-center justify-center svg-canvas-surface {canvasSurface === 'grid'
            ? 'svg-grid-surface'
            : canvasSurface === 'dark-grid'
              ? 'svg-grid-dark-surface'
              : ''}"
          data-svg-canvas={canvasSurface}
          data-svg-bounds={showBounds ? "true" : "false"}
        >
          <div
            class="svg-artboard-box flex items-center justify-center"
            data-bounds={showBounds ? "true" : "false"}
            style="width: {displaySize.width}px; height: {displaySize.height}px;"
          >
            {#key imgKey}
              <img
                bind:this={previewImgEl}
                src={prepared.dataUrl}
                alt={filePath}
                width={displaySize.width}
                height={displaySize.height}
                draggable="false"
                data-testid="svg-preview-img"
                class="block select-none {isDraggingSplit || isPanning ? 'pointer-events-none' : ''}"
                style="width: {displaySize.width}px; height: {displaySize.height}px;"
                onload={() => {
                  imgLoadError = null;
                }}
                onerror={() => {
                  imgLoadError = "The browser couldn't decode this SVG image.";
                }}
              />
            {/key}
          </div>
        </div>
      {/if}
    </div>
  </div>
{/snippet}

<div
  bind:this={viewerRootEl}
  use:fileSearchControl={previewSearchController}
  class="@container/svgview flex-1 flex flex-col h-full w-full bg-background font-sans text-xs min-h-0 min-w-0 overflow-hidden select-text relative"
  data-svg-viewer-layout={layout.mode}
  data-testid="svg-preview-viewer"
>
  <!-- Top Toolbar (sits on page ground, matches HtmlPreviewViewer) -->
  <div class="relative shrink-0 select-none" data-tip-place="above" data-svg-toolbar>
    <div bind:this={svgBar} class="gp-header-scroll">
      <div class="flex items-center justify-between gap-1.5 @md/svgview:gap-2 px-2 @md/svgview:px-3 py-1.5 min-w-max w-full">
        <!-- Left: SVG Brand, Title & Details Toggle -->
        <div class="flex items-center gap-1.5 @md/svgview:gap-2 shrink-0">
          <div class="hidden @3xs/svgview:flex items-center gap-1.5 py-0.5 px-2 rounded-full bg-surface border border-edge shadow-xs">
            <LanguageLogo {filePath} size={13} class="shrink-0" />
            <span class="hidden @2xl/svgview:inline font-semibold text-textPrimary text-meta">SVG Preview</span>
            <span class="hidden @3xl/svgview:inline-flex gp-chip font-mono" data-tone="accent" data-kind="count">.SVG</span>
            {#if viewMode !== "code"}
              <span
                class="w-1.5 h-1.5 rounded-full shrink-0 {liveAutoUpdate ? 'bg-ok' : 'bg-warn'}"
                title={liveAutoUpdate ? "Live auto-update active" : "Live auto-update paused"}
                aria-hidden="true"
              ></span>
            {/if}
          </div>

          {#if prepared.stats.title}
            <span
              class="hidden @4xl/svgview:inline text-meta text-textMuted max-w-44 truncate"
              title={`SVG title: ${prepared.stats.title}`}
            >{prepared.stats.title}</span>
          {/if}

          <button
            type="button"
            onclick={() => (showInspector = !showInspector)}
            aria-pressed={showInspector}
            aria-label="{showInspector ? 'Hide' : 'Show'} SVG document details"
            title="Inspect SVG dimensions, elements, inlined images, and safety status"
            data-testid="svg-inspector-toggle"
            class="gp-btn text-meta {showInspector
              ? 'border-accent/65 bg-accent/15 text-accent font-semibold'
              : inspectorNoticeCount > 0
                ? 'text-warn'
                : ''}"
            data-size="sm"
          >
            <Info size={12} />
            <span class="hidden @xl/svgview:inline">Details</span>
            {#if inspectorNoticeCount > 0}
              <span class="text-micro font-mono tabular-nums">({inspectorNoticeCount})</span>
            {/if}
          </button>
        </div>

        <!-- Center: Mode Switcher when wide enough -->
        {#if layout.toolbar === "inline"}
          {@render modeSwitch()}
        {/if}

        <!-- Right: Split Orientation, Live Toggle, Refresh, Copy & Open Externally -->
        <div class="flex items-center gap-1 @md/svgview:gap-1.5 shrink-0">
          {#if viewMode === "split" && canToggleSplitDirection}
            <button
              type="button"
              onclick={toggleSplitDirection}
              aria-pressed={effectiveSplitDirection === "vertical"}
              aria-label={effectiveSplitDirection === "horizontal"
                ? "Stack split panes vertically (code above preview)"
                : "Place split panes side by side (code beside preview)"}
              title={effectiveSplitDirection === "horizontal"
                ? "Stack panes vertically for a full-width preview"
                : "Place code and preview side by side"}
              data-testid="svg-split-orientation-toggle"
              class="gp-icon-btn {effectiveSplitDirection === 'vertical' ? 'text-accent bg-accent/15' : ''}"
              data-size="sm"
            >
              {#if effectiveSplitDirection === "horizontal"}
                <Rows2 size={12} />
              {:else}
                <Columns size={12} />
              {/if}
            </button>
          {/if}

          {#if viewMode !== "code"}
            <button
              type="button"
              onclick={toggleLiveUpdates}
              aria-pressed={liveAutoUpdate}
              aria-label={liveAutoUpdate ? "Pause live preview updates" : "Resume live preview updates"}
              title={liveAutoUpdate ? "Pause live updates while editing" : "Resume live updates while editing"}
              data-testid="svg-live-toggle"
              class="gp-icon-btn {liveAutoUpdate ? 'text-ok bg-ok/10' : 'text-warn bg-warn/10'}"
              data-size="sm"
            >
              {#if liveAutoUpdate}
                <Pause size={12} />
              {:else}
                <Play size={12} />
              {/if}
            </button>

            <button
              type="button"
              onclick={refreshPreview}
              aria-label="Refresh SVG preview and local assets"
              title="Refresh preview and re-read local image assets from disk"
              data-testid="svg-refresh-btn"
              class="gp-icon-btn"
              data-size="sm"
            >
              <RotateCw size={12} class={isRefreshing ? "animate-spin" : ""} />
            </button>
          {/if}

          <button
            type="button"
            onclick={handleCopySource}
            aria-label="Copy SVG source"
            title="Copy raw SVG XML source"
            data-testid="svg-copy-btn"
            class="gp-btn text-meta"
            data-size="sm"
          >
            {#if copiedKind === "source"}
              <Check size={11} class="text-ok" />
              <span class="hidden @xl/svgview:inline text-ok font-semibold">Copied</span>
            {:else}
              <Copy size={11} />
              <span class="hidden @xl/svgview:inline">Copy</span>
            {/if}
          </button>

          <button
            type="button"
            onclick={handleCopyDataUri}
            disabled={!canRenderStageImage}
            aria-label="Copy SVG data URI"
            title="Copy sanitized base64 SVG data URI"
            data-testid="svg-copy-data-uri"
            class="gp-btn text-meta"
            data-size="sm"
          >
            {#if copiedKind === "data-uri"}
              <Check size={11} class="text-ok" />
              <span class="hidden @2xl/svgview:inline text-ok font-semibold">Copied URI</span>
            {:else}
              <Link2 size={11} />
              <span class="hidden @2xl/svgview:inline">Data URI</span>
            {/if}
          </button>

          <button
            type="button"
            onclick={handleCopyPng}
            disabled={!canRenderStageImage}
            aria-label="Copy SVG as PNG"
            title="Rasterize SVG and copy PNG image to clipboard"
            data-testid="svg-copy-png"
            class="gp-btn text-meta"
            data-size="sm"
          >
            {#if copiedKind === "png"}
              <Check size={11} class="text-ok" />
              <span class="hidden @2xl/svgview:inline text-ok font-semibold">Copied PNG</span>
            {:else}
              <ImageDown size={11} />
              <span class="hidden @2xl/svgview:inline">PNG</span>
            {/if}
          </button>

          <button
            type="button"
            onclick={openExternally}
            aria-label="Open in external app"
            title="Open SVG file in default application"
            data-testid="svg-open-external"
            class="gp-btn text-meta"
            data-variant="primary"
            data-size="sm"
          >
            <ExternalLink size={11} />
            <span class="hidden @3xl/svgview:inline">Open in app</span>
          </button>
        </div>
      </div>
    </div>
    <ScrollCue target={svgBar} axis="x" />
    {#if layout.toolbar === "stacked"}
      <div class="grid px-2 pb-1.5" data-svg-mode-row>
        {@render modeSwitch()}
      </div>
    {/if}
  </div>

  <!-- Collapsible SVG Document & Safety Inspector -->
  {#if showInspector}
    <div
      class="shrink-0 mx-2.5 mb-2 gp-card p-2.5 text-meta space-y-1.5 select-text"
      data-testid="svg-inspector-panel"
    >
      <div class="flex flex-wrap items-center gap-2">
        <span class="gp-chip font-mono" data-tone="neutral">{formatBytes(prepared.stats.byteCount)}</span>
        <span class="gp-chip font-mono" data-tone="neutral">{prepared.stats.lineCount} lines</span>
        <span class="gp-chip font-mono" data-tone="accent">
          {formatDimension(prepared.stats.intrinsicWidth)} × {formatDimension(prepared.stats.intrinsicHeight)} px{prepared.stats.dimensionSource !== "attributes" ? ` (${prepared.stats.dimensionSource})` : ""}
        </span>
        {#if prepared.stats.viewBox}
          <span class="gp-chip font-mono" data-tone="neutral">viewBox {prepared.stats.viewBox.raw}</span>
        {/if}
        <span class="gp-chip font-mono" data-tone="neutral">{prepared.stats.elementCount} {prepared.stats.elementCount === 1 ? "element" : "elements"}</span>
        {#if prepared.stats.pathCount > 0}
          <span class="gp-chip font-mono" data-tone="neutral">{prepared.stats.pathCount} {prepared.stats.pathCount === 1 ? "path" : "paths"}</span>
        {/if}
        {#if prepared.stats.shapeCount > 0}
          <span class="gp-chip font-mono" data-tone="neutral">{prepared.stats.shapeCount} {prepared.stats.shapeCount === 1 ? "shape" : "shapes"}</span>
        {/if}
        {#if prepared.stats.textCount > 0}
          <span class="gp-chip font-mono" data-tone="neutral">{prepared.stats.textCount} text</span>
        {/if}
        {#if prepared.stats.groupCount > 0}
          <span class="gp-chip font-mono" data-tone="neutral">{prepared.stats.groupCount} {prepared.stats.groupCount === 1 ? "group" : "groups"}</span>
        {/if}
        {#if prepared.stats.imageCount > 0}
          <span class="gp-chip font-mono" data-tone="neutral">{prepared.stats.imageCount} {prepared.stats.imageCount === 1 ? "image" : "images"}</span>
        {/if}
        {#if prepared.stats.animationCount > 0}
          <span class="gp-chip font-mono" data-tone="neutral">{prepared.stats.animationCount} {prepared.stats.animationCount === 1 ? "animation" : "animations"}</span>
        {/if}
        {#if prepared.stats.defsCount > 0}
          <span class="gp-chip font-mono" data-tone="neutral">{prepared.stats.defsCount} defs</span>
        {/if}
        {#if prepared.stats.foreignObjectCount > 0}
          <span class="gp-chip font-mono" data-tone="warn">{prepared.stats.foreignObjectCount} foreignObject</span>
        {/if}
        {#if prepared.stats.usesCurrentColor}
          <span class="gp-chip font-mono" data-tone="accent">currentColor</span>
        {/if}
        {#if totalInlinedImages > 0}
          <span class="gp-chip font-mono" data-tone="accent">{totalInlinedImages} local {totalInlinedImages === 1 ? "image" : "images"} inlined</span>
        {/if}
      </div>

      {#if prepared.stats.description}
        <p class="text-micro text-textMuted" data-testid="svg-inspector-desc">
          <span class="font-medium text-textPrimary">Description:</span> {prepared.stats.description}
        </p>
      {/if}

      {#if prepared.synthesizedViewBox}
        <p class="text-micro text-textMuted" data-testid="svg-inspector-viewbox-note">
          No <code class="font-mono">viewBox</code> declared — preview synthesized <code class="font-mono">0 0 {formatDimension(prepared.stats.intrinsicWidth)} {formatDimension(prepared.stats.intrinsicHeight)}</code> for scaling.
        </p>
      {/if}

      {#if prepared.addedXmlns}
        <p class="text-micro text-textMuted" data-testid="svg-inspector-xmlns-note">
          Missing SVG <code class="font-mono">xmlns</code> declaration — auto-added in preview for standalone rendering.
        </p>
      {/if}

      {#if sanitizedCount > 0}
        <p class="text-micro text-warn flex items-center gap-1.5">
          <Shield size={11} class="shrink-0" />
          <span>
            Stripped in preview for safety:
            {prepared.stats.scriptCount} {prepared.stats.scriptCount === 1 ? "script" : "scripts"},
            {prepared.stats.eventHandlerCount} event {prepared.stats.eventHandlerCount === 1 ? "handler" : "handlers"},
            {prepared.stats.unsafeHrefCount} unsafe {prepared.stats.unsafeHrefCount === 1 ? "link" : "links"}{prepared.stats.unsafeSmilCount > 0 ? `, ${prepared.stats.unsafeSmilCount} unsafe ${prepared.stats.unsafeSmilCount === 1 ? "animation target" : "animation targets"}` : ""}{prepared.stats.strippedStylesheetPiCount > 0 ? `, ${prepared.stats.strippedStylesheetPiCount} xml-stylesheet ${prepared.stats.strippedStylesheetPiCount === 1 ? "instruction" : "instructions"}` : ""}{prepared.stats.strippedDtd ? ", DTD declarations" : ""}.
          </span>
        </p>
      {/if}

      {#if prepared.stats.externalRefCount > 0}
        <p class="text-micro text-textMuted">
          {prepared.stats.externalRefCount} external resource {prepared.stats.externalRefCount === 1 ? "reference is" : "references are"} blocked in isolated SVG preview.
        </p>
      {/if}

      {#if inlinedAssets.missingAssets.length > 0}
        <p class="text-micro text-warn">
          Couldn't inline {inlinedAssets.missingAssets.length} local {inlinedAssets.missingAssets.length === 1 ? "asset" : "assets"}:
          <span class="font-mono">{inlinedAssets.missingAssets.slice(0, 4).join(", ")}{inlinedAssets.missingAssets.length > 4 ? "…" : ""}</span>
        </p>
      {/if}

      {#if inlinedAssets.skippedCapCount > 0}
        <p class="text-micro text-textMuted">
          {inlinedAssets.skippedCapCount} additional local {inlinedAssets.skippedCapCount === 1 ? "asset exceeded" : "assets exceeded"} the inline preview cap.
        </p>
      {/if}

      {#if effectiveParseError}
        <p class="text-micro text-danger font-mono">{effectiveParseError}</p>
      {/if}
    </div>
  {/if}

  <!-- Main Workspace -->
  <div
    bind:this={viewportEl}
    class="flex-1 flex flex-row min-h-0 min-w-0 overflow-hidden relative"
  >
    {#if viewMode === "preview"}
      {@render previewFrame()}
    {:else if viewMode === "code"}
      <div class="grid grid-rows-[minmax(0,1fr)] grid-cols-[minmax(0,1fr)] flex-1 min-h-0 min-w-0 overflow-hidden">
        <CodeViewer
          {filePath}
          content={sourceContent}
          eol={blob.eol ?? null}
          invalidUtf8Bytes={blob.invalid_utf8_bytes ?? 0}
          diskBase={blob.disk_base ?? null}
          readOnly={extracted.fromBase64}
          {draftContent}
          {dirty}
          {onSave}
          {onDraftChange}
          {onRequestDiscard}
          {revealTarget}
          {onSendSelectionToAgent}
        />
      </div>
    {:else}
      <!-- Split View: Side-by-side or stacked with draggable split sash -->
      <div
        bind:this={splitHostEl}
        class="flex-1 flex {effectiveSplitDirection === 'vertical'
          ? 'flex-col overflow-hidden'
          : 'flex-row overflow-hidden'} min-h-0 min-w-0"
        data-svg-split={effectiveSplitDirection}
      >
        <div
          class="grid grid-rows-[minmax(0,1fr)] grid-cols-[minmax(0,1fr)] overflow-hidden {effectiveSplitDirection === 'vertical'
            ? 'w-full min-w-0 min-h-0 shrink-0'
            : 'h-full min-h-0 min-w-0 shrink-0'}"
          style={effectiveSplitDirection === "horizontal"
            ? `width: ${splitRatio}%;`
            : `height: ${splitVerticalRatio}%;`}
          data-svg-split-code
        >
          <CodeViewer
            {filePath}
            content={sourceContent}
            eol={blob.eol ?? null}
            invalidUtf8Bytes={blob.invalid_utf8_bytes ?? 0}
            diskBase={blob.disk_base ?? null}
            readOnly={extracted.fromBase64}
            {draftContent}
            {dirty}
            {onSave}
            {onDraftChange}
            {onRequestDiscard}
            {revealTarget}
            {onSendSelectionToAgent}
          />
        </div>

        {#if effectiveSplitDirection === "horizontal"}
          <!-- Justified: WAI-ARIA 1.2 Window Splitter pattern requires a focusable separator with arrow-key resizing. -->
          <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
          <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
          <div
            role="separator"
            tabindex="0"
            aria-orientation="vertical"
            aria-label="Resize SVG code and preview panes (double-click to reset)"
            aria-valuenow={Math.round(splitRatio)}
            aria-valuemin={25}
            aria-valuemax={75}
            data-testid="svg-split-sash"
            class="w-1.5 shrink-0 flex items-center justify-center cursor-col-resize select-none group"
            onpointerdown={(e) => startSplitDrag(e, "horizontal")}
            ondblclick={() => (splitRatio = 50)}
            onkeydown={(e) => handleSplitSashKeydown(e, "horizontal")}
          >
            <span class="pointer-events-none w-px h-full transition-colors bg-border group-hover:bg-accent/70 group-hover:w-0.5 group-focus-visible:bg-accent"></span>
          </div>
        {:else}
          <!-- Justified: WAI-ARIA 1.2 Window Splitter pattern requires a focusable separator with arrow-key resizing. -->
          <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
          <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
          <div
            role="separator"
            tabindex="0"
            aria-orientation="horizontal"
            aria-label="Resize SVG code and preview panes vertically (double-click to reset)"
            aria-valuenow={Math.round(splitVerticalRatio)}
            aria-valuemin={25}
            aria-valuemax={75}
            data-testid="svg-split-sash"
            class="h-1.5 shrink-0 flex items-center justify-center cursor-row-resize select-none group"
            onpointerdown={(e) => startSplitDrag(e, "vertical")}
            ondblclick={() => (splitVerticalRatio = 50)}
            onkeydown={(e) => handleSplitSashKeydown(e, "vertical")}
          >
            <span class="pointer-events-none h-px w-full transition-colors bg-border group-hover:bg-accent/70 group-hover:h-0.5 group-focus-visible:bg-accent"></span>
          </div>
        {/if}

        <div
          class="flex flex-col overflow-hidden {effectiveSplitDirection === 'vertical'
            ? 'flex-1 w-full min-w-0 min-h-0'
            : 'flex-1 h-full min-h-0 min-w-0'}"
          data-svg-split-preview
        >
          {@render previewFrame()}
        </div>
      </div>
    {/if}
  </div>
</div>

<style>
  .svg-canvas-surface[data-svg-canvas="light"] {
    color-scheme: light;
    background-color: Canvas;
  }
  .svg-canvas-surface[data-svg-canvas="dark"] {
    color-scheme: dark;
    background-color: Canvas;
  }
  .svg-grid-surface {
    background-color: Canvas;
    color-scheme: light;
    background-image:
      linear-gradient(45deg, color-mix(in srgb, CanvasText 9%, transparent) 25%, transparent 25%),
      linear-gradient(-45deg, color-mix(in srgb, CanvasText 9%, transparent) 25%, transparent 25%),
      linear-gradient(45deg, transparent 75%, color-mix(in srgb, CanvasText 9%, transparent) 75%),
      linear-gradient(-45deg, transparent 75%, color-mix(in srgb, CanvasText 9%, transparent) 75%);
    background-size: 16px 16px;
    background-position:
      0 0,
      0 8px,
      8px -8px,
      -8px 0;
  }
  .svg-grid-dark-surface {
    background-color: Canvas;
    color-scheme: dark;
    background-image:
      linear-gradient(45deg, color-mix(in srgb, CanvasText 12%, transparent) 25%, transparent 25%),
      linear-gradient(-45deg, color-mix(in srgb, CanvasText 12%, transparent) 25%, transparent 25%),
      linear-gradient(45deg, transparent 75%, color-mix(in srgb, CanvasText 12%, transparent) 75%),
      linear-gradient(-45deg, transparent 75%, color-mix(in srgb, CanvasText 12%, transparent) 75%);
    background-size: 16px 16px;
    background-position:
      0 0,
      0 8px,
      8px -8px,
      -8px 0;
  }
  .svg-artboard-box[data-bounds="true"] {
    outline: 1px dashed rgb(var(--c-accent) / 0.65);
    outline-offset: 2px;
  }
</style>
