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
    Monitor,
    Tablet,
    Smartphone,
    Maximize2,
    Minimize2,
    Sun,
    Moon,
    Grid3x3,
    Pause,
    Play,
    Info,
    Shield,
    Globe,
    RotateCcw,
    AlertTriangle,
  } from "@lucide/svelte";
  import { invoke } from "../../ipc/invoke";
  import { createAsyncGuard, type AsyncGuard } from "../../async/guard";
  import { observeResize } from "../../dom/observeResize";
  import { openInDefaultApp } from "../../desktop/openInShell";
  import { copyText } from "../../desktop/clipboard";
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
    clearHtmlViewModeIfMatches,
    HTML_VIEWPORT_PRESETS,
    HTML_ZOOM_MAX,
    HTML_ZOOM_MIN,
    HTML_ZOOM_STEP,
    htmlViewModeRequest,
    inlineLocalHtmlAssets,
    matchHtmlViewMode,
    MAX_HTML_PREVIEW_BYTES,
    prepareHtmlPreviewDocument,
    recalledHtmlViewMode,
    rememberHtmlViewMode,
    resolveHtmlPreviewLayout,
    resolveHtmlSplitDirection,
    type HtmlCanvasSurface,
    type HtmlSplitDirection,
    type HtmlViewMode,
    type HtmlViewportPreset,
    type InlinedAssetResult,
    type PreparedHtmlDocument,
  } from "../../files/htmlPreview";
  import CodeViewer from "./CodeViewer.svelte";
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
  const sourceContent = $derived(blob.text || "");
  const rawContent = $derived(draftContent ?? sourceContent);

  const RERENDER_DELAY_MS = 150;
  const EMPTY_ASSETS: InlinedAssetResult = {
    stylesheets: {},
    images: {},
    inlinedStylesheetCount: 0,
    inlinedImageCount: 0,
    missingAssets: [],
    skippedCapCount: 0,
  };

  let lastHandledModeNonce = 0;

  function resolveInitialMode(
    repo: string | null,
    path: string,
  ): { mode: HtmlViewMode; explicit: boolean } {
    const matched = matchHtmlViewMode(get(htmlViewModeRequest), repo, path);
    if (matched) {
      lastHandledModeNonce = matched.nonce;
      rememberHtmlViewMode(repo, path, matched.mode);
      return { mode: matched.mode, explicit: true };
    }
    const recalled = recalledHtmlViewMode(repo, path);
    if (recalled) return { mode: recalled, explicit: false };
    return { mode: "preview", explicit: false };
  }

  const initialChoice = untrack(() => resolveInitialMode(repoPath, filePath));
  let viewMode = $state<HtmlViewMode>(initialChoice.mode);
  let lastHandledRevealSeq = untrack(() =>
    initialChoice.explicit && initialChoice.mode === "preview" ? (revealTarget?.seq ?? 0) : 0,
  );
  let viewportPreset = $state<HtmlViewportPreset>("responsive");
  let landscape = $state(false);
  let zoom = $state(100);
  let canvasSurface = $state<HtmlCanvasSurface>("light");
  let liveAutoUpdate = $state(true);
  let showInspector = $state(false);
  let copiedSource = $state(false);
  let splitRatio = $state(50);
  let splitVerticalRatio = $state(50);
  let preferredSplitDirection = $state<HtmlSplitDirection>("horizontal");
  let isDraggingSplit = $state(false);
  let refreshNonce = $state(0);
  let isRefreshing = $state(false);

  let viewerRootEl: HTMLDivElement | undefined = $state();
  let htmlBar: HTMLDivElement | undefined = $state();
  let subBar: HTMLDivElement | undefined = $state();
  let viewportEl: HTMLDivElement | undefined = $state();
  let splitHostEl: HTMLDivElement | undefined = $state();
  let viewportWidth = $state(0);
  let viewportHeight = $state(0);
  let copiedTimer: ReturnType<typeof setTimeout> | null = null;
  let stopSplitDrag: (() => void) | null = null;

  let previewedContent = $state(untrack(() => rawContent));
  let inlinedAssets = $state<InlinedAssetResult>(EMPTY_ASSETS);
  let shownFileKey: string | null = null;
  let lastAssetKey = "";
  let assetGuard: AsyncGuard | null = null;

  const hasPendingPausedEdit = $derived(!liveAutoUpdate && previewedContent !== rawContent);

  function setMode(next: HtmlViewMode, fromUser = true) {
    viewMode = next;
    rememberHtmlViewMode(repoPath, filePath, next);
    if (fromUser) {
      clearHtmlViewModeIfMatches(repoPath, filePath);
    }
  }

  // Apply one-shot mode overrides (right-click "Open as Preview" / "Open Code & Preview" / "Open in editor")
  // non-destructively via nonce tracking so concurrently mounted hosts (FileViewer + TerminalFilePane)
  // never steal a right-click request from one another.
  $effect(() => {
    const req = $htmlViewModeRequest;
    const repo = repoPath;
    const file = filePath;
    if (!req) return;
    untrack(() => {
      const matched = matchHtmlViewMode(req, repo, file);
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

  // Switching to a different HTML file or repository recalls its last mode (or defaults to preview)
  // unless an explicit mode request is waiting for it.
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
      rememberHtmlViewMode(repo, file, choice.mode);
      lastHandledRevealSeq =
        choice.explicit && choice.mode === "preview" ? (revealTarget?.seq ?? 0) : 0;
    });
  });

  // A fresh line reveal (terminal link with `:line` or search jump) switches out of
  // pure Preview so CodeViewer can scroll to the requested line; Split already mounts it.
  // Stale `revealTarget` objects whose `seq` was already handled or superseded by an
  // explicit "Open as Preview" request do not hijack Preview mode.
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
        clearHtmlViewModeIfMatches(repoPath, file);
        setMode("code", false);
      }
    });
  });

  // Debounce live HTML updates while typing in Split mode; update immediately on
  // file/worktree switch, first load, or manual Refresh. Clear previous file's
  // inlined CSS/images immediately on file switch to prevent cross-file bleed.
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

  // Key the sandboxed `<iframe>` by file identity + `refreshNonce` so clicking
  // Refresh (or switching between two files with identical HTML) always mounts
  // a fresh document context, resetting scroll, form inputs, `<details>`, and
  // `#fragment` state even when `prepared.srcdoc` is unchanged.
  const iframeKey = $derived(`${repoPath ?? ""}\0${filePath}\0${refreshNonce}`);

  // Prepare the sanitized `srcdoc` reactively from `previewedContent`, canvas, and inlined assets.
  const prepared = $derived<PreparedHtmlDocument>(
    prepareHtmlPreviewDocument(previewedContent, {
      canvas: canvasSurface,
      inlinedStylesheets: inlinedAssets.stylesheets,
      inlinedImages: inlinedAssets.images,
    }),
  );

  // Include `blob.disk_base` so saves and disk reloads (e.g. Follow Agent)
  // re-read local CSS & images automatically without re-fetching per keystroke.
  const assetSignature = $derived(
    `${repoPath ?? ""}\0${filePath}\0${blob.disk_base ?? ""}\0${refreshNonce}\0${prepared.stats.localStylesheetRefs.join("|")}\0${prepared.stats.localImageRefs.join("|")}`,
  );

  // Resolve repo-local CSS and images only when their reference set, file, disk revision, or Refresh changes.
  $effect(() => {
    const sig = assetSignature;
    const repo = repoPath;
    const file = filePath;
    const html = previewedContent;
    const hasLocalRefs =
      prepared.stats.localStylesheetRefs.length > 0 || prepared.stats.localImageRefs.length > 0;
    if (!hasLocalRefs || !repo || !file || prepared.tooLarge) {
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
    void inlineLocalHtmlAssets(
      html,
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
    const sync = (w: number, h: number) => {
      viewportWidth = Number.isFinite(w) && w > 0 ? w : 0;
      viewportHeight = Number.isFinite(h) && h > 0 ? h : 0;
    };
    sync(el.clientWidth, el.clientHeight);
    return observeResize(el, (entries) => {
      const rect = entries.find((entry) => entry.target === el)?.contentRect ?? entries[0]?.contentRect;
      if (rect) sync(rect.width, rect.height);
    });
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

  const layout = $derived(resolveHtmlPreviewLayout(viewportWidth));
  const splitChoice = $derived(resolveHtmlSplitDirection(viewportWidth, preferredSplitDirection));
  const effectiveSplitDirection = $derived(splitChoice.direction);
  const canToggleSplitDirection = $derived(splitChoice.canToggle);
  const modeButtonClass = $derived(
    layout.toolbar === "stacked"
      ? "gp-seg-btn flex flex-1 min-w-0 items-center justify-center gap-1 px-2!"
      : "gp-seg-btn flex items-center gap-1 @md/htmlview:gap-1.5 px-2! @md/htmlview:px-2.5!",
  );
  const modeLabelClass = $derived(layout.toolbar === "stacked" ? "hidden @3xs/htmlview:inline truncate" : "");

  const activePresetSpec = $derived(
    HTML_VIEWPORT_PRESETS.find((preset) => preset.id === viewportPreset) ?? HTML_VIEWPORT_PRESETS[0],
  );
  const frameDimensions = $derived.by(() => {
    const w = activePresetSpec.width;
    const h = activePresetSpec.height;
    if (w === null || h === null) return null;
    return landscape ? { width: h, height: w } : { width: w, height: h };
  });

  const availablePreviewWidth = $derived.by(() => {
    if (viewportWidth <= 0) return 0;
    if (viewMode === "split" && effectiveSplitDirection === "horizontal") {
      return Math.max(0, Math.round(viewportWidth * ((100 - splitRatio) / 100)) - 40);
    }
    return Math.max(0, viewportWidth - 40);
  });

  const availablePreviewHeight = $derived.by(() => {
    if (viewportHeight <= 0) return 0;
    const bodyHeight =
      viewMode === "split" && effectiveSplitDirection === "vertical"
        ? Math.round(viewportHeight * ((100 - splitVerticalRatio) / 100))
        : viewportHeight;
    return Math.max(240, bodyHeight - 96);
  });

  const totalInlinedImages = $derived(
    Math.max(prepared.inlinedImageCount, inlinedAssets.inlinedImageCount),
  );

  const sandboxNoticeCount = $derived(
    prepared.stats.scriptCount +
      prepared.stats.externalStylesheetCount +
      inlinedAssets.missingAssets.length +
      inlinedAssets.skippedCapCount,
  );

  function fitPresetToPane() {
    if (!frameDimensions || availablePreviewWidth <= 0) {
      zoom = 100;
      return;
    }
    const fitPct = Math.floor((availablePreviewWidth / frameDimensions.width) * 100);
    zoom = Math.max(HTML_ZOOM_MIN, Math.min(100, fitPct));
  }

  function refreshPreview() {
    previewedContent = rawContent;
    lastAssetKey = "";
    refreshNonce += 1;
  }

  function toggleLiveUpdates() {
    liveAutoUpdate = !liveAutoUpdate;
    if (liveAutoUpdate && previewedContent !== rawContent) {
      previewedContent = rawContent;
    }
  }

  function stepZoom(delta: number) {
    zoom = Math.max(HTML_ZOOM_MIN, Math.min(HTML_ZOOM_MAX, zoom + delta));
  }

  async function handleCopySource() {
    if (!(await copyText(rawContent))) {
      repoStore.setError("Couldn't copy to clipboard");
      return;
    }
    copiedSource = true;
    if (copiedTimer) clearTimeout(copiedTimer);
    copiedTimer = setTimeout(() => {
      copiedTimer = null;
      copiedSource = false;
    }, 1800);
  }

  async function openInBrowser() {
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
  <div class="gp-segmented" role="group" aria-label="HTML view mode" data-html-modes={layout.toolbar}>
    <button
      type="button"
      onclick={() => setMode("preview")}
      aria-pressed={viewMode === "preview"}
      aria-label="Preview"
      class={modeButtonClass}
      data-active={viewMode === "preview"}
      data-testid="html-mode-preview"
      title="Live HTML preview"
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
      data-testid="html-mode-split"
      title={effectiveSplitDirection === "vertical"
        ? "Split view: HTML code above live preview"
        : "Split view: HTML code and live preview side by side"}
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
      data-testid="html-mode-code"
      title="HTML source editor with line numbers and find"
    >
      <Code size={12} />
      <span class={modeLabelClass}>Code</span>
    </button>
  </div>
{/snippet}

{#snippet previewFrame()}
  <div
    class="flex-1 flex flex-col min-h-0 min-w-0 overflow-hidden bg-background relative"
    data-html-preview-pane
  >
    <!-- Compact Preview Controls Bar (viewport, zoom, canvas surface) -->
    <div class="relative shrink-0 select-none" data-tip-place="above" data-html-controls-bar>
      <div bind:this={subBar} class="gp-header-scroll">
        <div class="flex items-center justify-between gap-2 px-2.5 py-1 min-w-max w-full">
          <!-- Responsive Device Presets -->
          <div class="flex items-center gap-1" role="group" aria-label="Preview viewport size">
            {#each HTML_VIEWPORT_PRESETS as preset (preset.id)}
              <button
                type="button"
                onclick={() => {
                  viewportPreset = preset.id;
                  if (preset.id === "responsive") landscape = false;
                }}
                aria-pressed={viewportPreset === preset.id}
                aria-label={`${preset.label} viewport${preset.width ? ` (${preset.width}px)` : ""}`}
                title={preset.width ? `${preset.label} (${preset.width} × ${preset.height})` : "Fit available pane width"}
                data-testid={`html-viewport-${preset.id}`}
                class="gp-btn px-1.5! py-0.5! text-micro flex items-center gap-1 {viewportPreset === preset.id
                  ? 'border-accent/65 bg-accent/15 text-accent font-medium'
                  : ''}"
                data-size="sm"
              >
                {#if preset.id === "responsive"}
                  <Maximize2 size={11} />
                {:else if preset.id === "desktop"}
                  <Monitor size={11} />
                {:else if preset.id === "tablet"}
                  <Tablet size={11} />
                {:else}
                  <Smartphone size={11} />
                {/if}
                <span>{preset.label}</span>
              </button>
            {/each}

            {#if frameDimensions}
              <button
                type="button"
                onclick={() => (landscape = !landscape)}
                aria-pressed={landscape}
                aria-label="Rotate device orientation"
                title={landscape ? "Switch to portrait orientation" : "Switch to landscape orientation"}
                data-testid="html-viewport-rotate"
                class="gp-icon-btn {landscape ? 'text-accent bg-accent/15' : ''}"
                data-size="sm"
              >
                <RotateCcw size={11} />
              </button>
              {#if availablePreviewWidth > 0 && frameDimensions.width > availablePreviewWidth}
                <button
                  type="button"
                  onclick={fitPresetToPane}
                  aria-label="Scale device frame to fit pane width"
                  title="Scale device frame to fit this pane"
                  data-testid="html-viewport-fit-scale"
                  class="gp-icon-btn"
                  data-size="sm"
                >
                  <Minimize2 size={11} />
                </button>
              {/if}
            {/if}
          </div>

          <!-- Right: Zoom, Surface, Sandbox Badge -->
          <div class="flex items-center gap-1.5 shrink-0">
            <div class="flex items-center rounded-full border border-edge bg-surface px-1 gap-0.5" role="group" aria-label="Preview zoom">
              <button
                type="button"
                onclick={() => stepZoom(-HTML_ZOOM_STEP)}
                disabled={zoom <= HTML_ZOOM_MIN}
                aria-label="Zoom out preview"
                title="Zoom out"
                data-testid="html-zoom-out"
                class="gp-icon-btn text-xs"
                data-size="sm"
              >−</button>
              <button
                type="button"
                onclick={() => (zoom = 100)}
                aria-label="Reset preview zoom to 100%"
                title="Reset zoom to 100%"
                data-testid="html-zoom-reset"
                class="text-micro font-mono text-textMuted hover:text-textPrimary min-w-9 text-center tabular-nums"
              >{zoom}%</button>
              <button
                type="button"
                onclick={() => stepZoom(HTML_ZOOM_STEP)}
                disabled={zoom >= HTML_ZOOM_MAX}
                aria-label="Zoom in preview"
                title="Zoom in"
                data-testid="html-zoom-in"
                class="gp-icon-btn text-xs"
                data-size="sm"
              >+</button>
            </div>

            <div class="flex items-center rounded-full border border-edge bg-surface p-0.5 gap-0.5" role="group" aria-label="Preview canvas surface">
              <button
                type="button"
                onclick={() => (canvasSurface = "light")}
                aria-pressed={canvasSurface === "light"}
                aria-label="Light preview canvas"
                title="Light paper canvas"
                data-testid="html-canvas-light"
                class="gp-icon-btn {canvasSurface === 'light' ? 'text-accent bg-accent/15' : ''}"
                data-size="sm"
              >
                <Sun size={11} />
              </button>
              <button
                type="button"
                onclick={() => (canvasSurface = "dark")}
                aria-pressed={canvasSurface === "dark"}
                aria-label="Dark preview canvas"
                title="Dark preview canvas"
                data-testid="html-canvas-dark"
                class="gp-icon-btn {canvasSurface === 'dark' ? 'text-accent bg-accent/15' : ''}"
                data-size="sm"
              >
                <Moon size={11} />
              </button>
              <button
                type="button"
                onclick={() => (canvasSurface = "grid")}
                aria-pressed={canvasSurface === "grid"}
                aria-label="Transparency grid canvas"
                title="Transparency grid canvas"
                data-testid="html-canvas-grid"
                class="gp-icon-btn {canvasSurface === 'grid' ? 'text-accent bg-accent/15' : ''}"
                data-size="sm"
              >
                <Grid3x3 size={11} />
              </button>
            </div>

            {#if prepared.stats.scriptCount > 0}
              <button
                type="button"
                onclick={() => (showInspector = !showInspector)}
                class="gp-chip font-mono cursor-pointer"
                data-tone="warn"
                data-testid="html-scripts-sandboxed-badge"
                title="Scripts are disabled in sandboxed preview for safety. Click for details or open in browser."
              >
                <Shield size={10} />
                <span>{prepared.stats.scriptCount} {prepared.stats.scriptCount === 1 ? "script" : "scripts"} off</span>
              </button>
            {/if}
          </div>
        </div>
      </div>
      <ScrollCue target={subBar} axis="x" />
    </div>

    {#if hasPendingPausedEdit}
      <div class="shrink-0 mx-2.5 mb-1.5 gp-notice text-meta flex items-center justify-between gap-2" data-tone="warn" role="status" data-testid="html-paused-banner">
        <span>Live preview is paused — edits are not yet reflected.</span>
        <div class="flex items-center gap-1 shrink-0">
          <button type="button" class="gp-btn text-micro" data-size="sm" data-testid="html-paused-refresh" onclick={refreshPreview}>
            Refresh now
          </button>
          <button type="button" class="gp-btn text-micro" data-variant="primary" data-size="sm" data-testid="html-paused-resume" onclick={toggleLiveUpdates}>
            Resume live
          </button>
        </div>
      </div>
    {/if}

    <!-- Preview Stage -->
    {#if prepared.tooLarge}
      <div class="flex-1 min-h-0 flex flex-col items-center justify-center p-6 text-center gap-3">
        <AlertTriangle size={24} class="text-warn" />
        <div class="space-y-1 max-w-md">
          <p class="text-sm font-semibold text-textPrimary">This HTML file is too large for inline live preview</p>
          <p class="text-meta text-textMuted">
            File size is {formatBytes(prepared.stats.byteCount)} (limit {formatBytes(MAX_HTML_PREVIEW_BYTES)}).
            Inspect it in Code view or open it in your browser.
          </p>
        </div>
        <div class="flex items-center gap-2">
          <button type="button" class="gp-btn text-meta" data-size="sm" onclick={() => setMode("code")}>
            <Code size={12} />
            <span>Open Code</span>
          </button>
          <button type="button" class="gp-btn text-meta" data-variant="primary" data-size="sm" onclick={openInBrowser}>
            <ExternalLink size={12} />
            <span>Open in browser</span>
          </button>
        </div>
      </div>
    {:else if !frameDimensions && zoom === 100}
      <!-- Full-bleed Responsive Fit Stage -->
      <div
        class="flex-1 min-h-0 min-w-0 p-2 overflow-hidden flex flex-col"
        data-html-stage="responsive"
      >
        <div
          class="flex-1 min-h-0 min-w-0 rounded-xl border border-edgeStrong shadow-card overflow-hidden {canvasSurface === 'grid'
            ? 'html-grid-surface'
            : ''}"
          data-html-canvas={canvasSurface}
        >
          {#key iframeKey}
            <iframe
              title={`HTML preview of ${filePath}`}
              srcdoc={prepared.srcdoc}
              sandbox="allow-forms"
              referrerpolicy="no-referrer"
              data-testid="html-preview-iframe"
              class="w-full h-full border-0 block html-preview-frame {isDraggingSplit ? 'pointer-events-none' : ''}"
              data-canvas={canvasSurface}
            ></iframe>
          {/key}
        </div>
      </div>
    {:else}
      <!-- Framed / Zoomed Viewport Stage: `mx-auto` on the card (not `items-center` on
           the scroll container) centers when it fits and keeps `scrollLeft = 0` at the
           card's left edge when a wide preset overflows a narrow pane. -->
      {@const targetW = frameDimensions ? frameDimensions.width : (availablePreviewWidth > 0 ? availablePreviewWidth : 960)}
      {@const targetH = frameDimensions ? frameDimensions.height : (availablePreviewHeight > 0 ? availablePreviewHeight : 640)}
      {@const scale = zoom / 100}
      <div
        class="flex-1 min-h-0 min-w-0 overflow-auto gp-scroll p-4 flex flex-col bg-background/80"
        data-html-stage="framed"
      >
        <div class="mx-auto rounded-xl border border-edgeStrong shadow-card overflow-hidden shrink-0" data-testid="html-device-frame">
          <div class="flex items-center justify-between gap-3 px-3 py-1.5 text-micro font-mono text-textMuted select-none bg-surface">
            <span class="font-medium text-textPrimary">
              {frameDimensions ? `${activePresetSpec.label}${landscape ? " (landscape)" : ""}` : "Responsive"}
            </span>
            <span class="tabular-nums">{targetW} × {targetH} px · {zoom}%</span>
          </div>
          <div
            class="overflow-hidden {canvasSurface === 'grid' ? 'html-grid-surface' : ''}"
            style="width: {Math.round(targetW * scale)}px; height: {Math.round(targetH * scale)}px;"
            data-html-canvas={canvasSurface}
          >
            {#key iframeKey}
              <iframe
                title={`HTML preview of ${filePath}`}
                srcdoc={prepared.srcdoc}
                sandbox="allow-forms"
                referrerpolicy="no-referrer"
                data-testid="html-preview-iframe"
                class="border-0 block origin-top-left html-preview-frame {isDraggingSplit ? 'pointer-events-none' : ''}"
                data-canvas={canvasSurface}
                style="width: {targetW}px; height: {targetH}px; transform: scale({scale});"
              ></iframe>
            {/key}
          </div>
        </div>
      </div>
    {/if}
  </div>
{/snippet}

<div
  bind:this={viewerRootEl}
  use:fileSearchControl={previewSearchController}
  class="@container/htmlview flex-1 flex flex-col h-full w-full bg-background font-sans text-xs min-h-0 min-w-0 overflow-hidden select-text relative"
  data-html-viewer-layout={layout.mode}
  data-testid="html-preview-viewer"
>
  <!-- Top Toolbar (sits on page ground, matches MarkDevViewer) -->
  <div class="relative shrink-0 select-none" data-tip-place="above" data-html-toolbar>
    <div bind:this={htmlBar} class="gp-header-scroll">
      <div class="flex items-center justify-between gap-1.5 @md/htmlview:gap-2 px-2 @md/htmlview:px-3 py-1.5 min-w-max w-full">
        <!-- Left: HTML Live Brand, Document Title & Inspector Toggle -->
        <div class="flex items-center gap-1.5 @md/htmlview:gap-2 shrink-0">
          <div class="hidden @3xs/htmlview:flex items-center gap-1.5 py-0.5 px-2 rounded-full bg-surface border border-edge shadow-xs">
            <Globe size={13} class="text-accent shrink-0" />
            <span class="hidden @2xl/htmlview:inline font-semibold text-textPrimary text-meta">HTML Preview</span>
            <span class="hidden @3xl/htmlview:inline-flex gp-chip font-mono" data-tone="accent" data-kind="count">.HTML</span>
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
              class="hidden @4xl/htmlview:inline text-meta text-textMuted max-w-44 truncate"
              title={`Document title: ${prepared.stats.title}`}
            >{prepared.stats.title}</span>
          {/if}

          <button
            type="button"
            onclick={() => (showInspector = !showInspector)}
            aria-pressed={showInspector}
            aria-label="{showInspector ? 'Hide' : 'Show'} HTML document inspector"
            title="Inspect document metadata, inlined local assets, and sandbox status"
            data-testid="html-inspector-toggle"
            class="gp-btn text-meta {showInspector
              ? 'border-accent/65 bg-accent/15 text-accent font-semibold'
              : sandboxNoticeCount > 0
                ? 'text-warn'
                : ''}"
            data-size="sm"
          >
            <Info size={12} />
            <span class="hidden @xl/htmlview:inline">Details</span>
            {#if sandboxNoticeCount > 0}
              <span class="text-micro font-mono tabular-nums">({sandboxNoticeCount})</span>
            {/if}
          </button>
        </div>

        <!-- Center: Mode Switcher when wide enough -->
        {#if layout.toolbar === "inline"}
          {@render modeSwitch()}
        {/if}

        <!-- Right: Split Orientation, Live Toggle, Refresh, Copy & Open in Browser -->
        <div class="flex items-center gap-1 @md/htmlview:gap-1.5 shrink-0">
          {#if viewMode === "split" && canToggleSplitDirection}
            <button
              type="button"
              onclick={() =>
                (preferredSplitDirection =
                  preferredSplitDirection === "horizontal" ? "vertical" : "horizontal")}
              aria-pressed={effectiveSplitDirection === "vertical"}
              aria-label={effectiveSplitDirection === "horizontal"
                ? "Stack split panes vertically (code above preview)"
                : "Place split panes side by side (code beside preview)"}
              title={effectiveSplitDirection === "horizontal"
                ? "Stack panes vertically for a full-width preview"
                : "Place code and preview side by side"}
              data-testid="html-split-orientation-toggle"
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
              data-testid="html-live-toggle"
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
              aria-label="Refresh HTML preview and local assets"
              title="Refresh preview and re-read local CSS & images from disk"
              data-testid="html-refresh-btn"
              class="gp-icon-btn"
              data-size="sm"
            >
              <RotateCw size={12} class={isRefreshing ? "animate-spin" : ""} />
            </button>
          {/if}

          <button
            type="button"
            onclick={handleCopySource}
            aria-label="Copy HTML source"
            title="Copy raw HTML source"
            class="gp-btn text-meta"
            data-size="sm"
          >
            {#if copiedSource}
              <Check size={11} class="text-ok" />
              <span class="hidden @xl/htmlview:inline text-ok font-semibold">Copied</span>
            {:else}
              <Copy size={11} />
              <span class="hidden @xl/htmlview:inline">Copy</span>
            {/if}
          </button>

          <button
            type="button"
            onclick={openInBrowser}
            aria-label="Open in browser"
            title="Open HTML file in your default web browser"
            data-testid="html-open-browser"
            class="gp-btn text-meta"
            data-variant="primary"
            data-size="sm"
          >
            <ExternalLink size={11} />
            <span class="hidden @3xl/htmlview:inline">Open in browser</span>
          </button>
        </div>
      </div>
    </div>
    <ScrollCue target={htmlBar} axis="x" />
    {#if layout.toolbar === "stacked"}
      <div class="grid px-2 pb-1.5" data-html-mode-row>
        {@render modeSwitch()}
      </div>
    {/if}
  </div>

  <!-- Collapsible Document & Sandbox Inspector -->
  {#if showInspector}
    <div
      class="shrink-0 mx-2.5 mb-2 gp-card p-2.5 text-meta space-y-1.5 select-text"
      data-testid="html-inspector-panel"
    >
      <div class="flex flex-wrap items-center gap-2">
        <span class="gp-chip font-mono" data-tone="neutral">{formatBytes(prepared.stats.byteCount)}</span>
        <span class="gp-chip font-mono" data-tone="neutral">{prepared.stats.lineCount} lines</span>
        {#if prepared.stats.hasDoctype}
          <span class="gp-chip font-mono" data-tone="ok">DOCTYPE</span>
        {/if}
        {#if prepared.inlinedStylesheetCount > 0}
          <span class="gp-chip font-mono" data-tone="accent">{prepared.inlinedStylesheetCount} local CSS inlined</span>
        {/if}
        {#if totalInlinedImages > 0}
          <span class="gp-chip font-mono" data-tone="accent">{totalInlinedImages} local {totalInlinedImages === 1 ? "image" : "images"} inlined</span>
        {/if}
        {#if prepared.stats.inlineStyleCount > 0}
          <span class="gp-chip font-mono" data-tone="neutral">{prepared.stats.inlineStyleCount} inline style {prepared.stats.inlineStyleCount === 1 ? "block" : "blocks"}</span>
        {/if}
      </div>

      {#if prepared.stats.scriptCount > 0}
        <p class="text-micro text-warn flex items-center gap-1.5">
          <Shield size={11} class="shrink-0" />
          <span>
            {prepared.stats.scriptCount} {prepared.stats.scriptCount === 1 ? "script" : "scripts"} disabled in the sandboxed preview for security.
            Use <strong>Open in browser</strong> to run interactive JavaScript.
          </span>
        </p>
      {/if}

      {#if prepared.stats.externalStylesheetCount > 0}
        <p class="text-micro text-textMuted">
          {prepared.stats.externalStylesheetCount} external stylesheet {prepared.stats.externalStylesheetCount === 1 ? "link is" : "links are"} blocked in offline sandboxed preview.
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
      <!-- Split View: Side-by-side or stacked (user-toggleable when wide, stacked when narrow) — both with draggable split sash -->
      <div
        bind:this={splitHostEl}
        class="flex-1 flex {effectiveSplitDirection === 'vertical'
          ? 'flex-col overflow-hidden'
          : 'flex-row overflow-hidden'} min-h-0 min-w-0"
        data-html-split={effectiveSplitDirection}
      >
        <div
          class="grid grid-rows-[minmax(0,1fr)] grid-cols-[minmax(0,1fr)] overflow-hidden {effectiveSplitDirection === 'vertical'
            ? 'w-full min-w-0 min-h-0 shrink-0'
            : 'h-full min-h-0 min-w-0 shrink-0'}"
          style={effectiveSplitDirection === "horizontal"
            ? `width: ${splitRatio}%;`
            : `height: ${splitVerticalRatio}%;`}
          data-html-split-code
        >
          <CodeViewer
            {filePath}
            content={sourceContent}
            eol={blob.eol ?? null}
            invalidUtf8Bytes={blob.invalid_utf8_bytes ?? 0}
            diskBase={blob.disk_base ?? null}
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
            aria-label="Resize HTML code and preview panes (double-click to reset)"
            aria-valuenow={Math.round(splitRatio)}
            aria-valuemin={25}
            aria-valuemax={75}
            data-testid="html-split-sash"
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
            aria-label="Resize HTML code and preview panes vertically (double-click to reset)"
            aria-valuenow={Math.round(splitVerticalRatio)}
            aria-valuemin={25}
            aria-valuemax={75}
            data-testid="html-split-sash"
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
          data-html-split-preview
        >
          {@render previewFrame()}
        </div>
      </div>
    {/if}
  </div>
</div>

<style>
  .html-preview-frame[data-canvas="light"] {
    color-scheme: light;
    background-color: Canvas;
  }
  .html-preview-frame[data-canvas="dark"] {
    color-scheme: dark;
    background-color: Canvas;
  }
  .html-preview-frame[data-canvas="grid"] {
    color-scheme: light;
    background-color: transparent;
  }
  .html-grid-surface {
    background-color: Canvas;
    color-scheme: light;
    background-image:
      linear-gradient(45deg, rgb(var(--c-border) / 0.25) 25%, transparent 25%),
      linear-gradient(-45deg, rgb(var(--c-border) / 0.25) 25%, transparent 25%),
      linear-gradient(45deg, transparent 75%, rgb(var(--c-border) / 0.25) 75%),
      linear-gradient(-45deg, transparent 75%, rgb(var(--c-border) / 0.25) 75%);
    background-size: 16px 16px;
    background-position:
      0 0,
      0 8px,
      8px -8px,
      -8px 0;
  }
</style>
