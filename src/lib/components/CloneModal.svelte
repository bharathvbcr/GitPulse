<script lang="ts">
  import { invoke } from "../ipc/invoke";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { guardedDismiss } from "./modalGuard";
  import { fade, scale } from "svelte/transition";
  import { repoStore } from "../stores/repoStore";
  import {
    backdropFade,
    backdropFadeOut,
    cardScale,
    cardScaleOut,
  } from "../ui/transitions";
  import { trapFocus } from "../ui/focusTrap";
  import { LAYERS } from "../ui/layers";
  import { reportPanelError } from "../diagnostics/report";
  import { Download, FolderOpen, Check } from "@lucide/svelte";

  let {
    isOpen = false,
    onClose,
  }: {
    isOpen?: boolean;
    onClose?: () => void;
  } = $props();

  let url = $state("");
  let targetDir = $state("");
  let isCloning = $state(false);
  let errorMsg = $state<string | null>(null);
  let showOptions = $state(false);
  let branch = $state("");
  let depthText = $state("");
  let recurseSubmodules = $state(false);
  let progress = $state<{ phase: string; percent: number | null } | null>(null);

  const MAX_CLONE_DEPTH = 1_000_000;
  /** Empty is a full clone; anything else must be a whole number in range. */
  let depth = $derived.by((): number | null | "invalid" => {
    const text = depthText.trim();
    if (!text) return null;
    const value = Number(text);
    return Number.isInteger(value) && value >= 1 && value <= MAX_CLONE_DEPTH ? value : "invalid";
  });

  interface CloneProgressEvent {
    id: string;
    phase: string;
    percent: number | null;
  }

  async function pickTargetDir() {
    try {
      const folder = await invoke<string | null>("cmd_pick_folder");
      if (folder) targetDir = folder;
    } catch (err) {
      errorMsg = reportPanelError("clone", err);
    }
  }

  async function handleClone() {
    if (!url.trim() || !targetDir.trim() || depth === "invalid") return;
    isCloning = true;
    errorMsg = null;
    progress = null;
    // The id ties progress events to this clone, not another window's.
    const progressId = `clone-${Date.now()}-${Math.random().toString(36).slice(2)}`;
    let unlisten: UnlistenFn | null = null;
    try {
      unlisten = await listen<CloneProgressEvent>("clone-progress", (event) => {
        if (event.payload.id !== progressId) return;
        progress = { phase: event.payload.phase, percent: event.payload.percent };
      });
    } catch {
      // Progress is a courtesy; a clone without it still runs.
      unlisten = null;
    }
    try {
      const clonedPath = await invoke<string>("cmd_clone_repo", {
        url: url.trim(),
        targetDir: targetDir.trim(),
        options: {
          branch: branch.trim() || null,
          depth,
          recurse_submodules: recurseSubmodules,
        },
        progressId,
      });
      await repoStore.openRepo(clonedPath);
      onClose?.();
    } catch (err: unknown) {
      errorMsg = reportPanelError("clone", err);
    } finally {
      unlisten?.();
      isCloning = false;
      progress = null;
    }
  }

  /** Mid-clone dismissal hides progress and completes invisibly later. */
  function requestClose() {
    guardedDismiss(isCloning, onClose);
  }
</script>

{#if isOpen}
  <div
    role="dialog"
    aria-modal="true"
    aria-labelledby="clone-modal-title"
    tabindex="-1"
    onclick={(e) => e.target === e.currentTarget && requestClose()}
    onkeydown={(e) => e.key === "Escape" && requestClose()}
    in:fade={backdropFade()}
    out:fade={backdropFadeOut()}
    class="gp-scrim bg-black/40 flex items-center justify-center p-4 select-none gp-gpu"
    style="z-index: {LAYERS.MODAL}"
  >
    <div
      use:trapFocus
      in:scale={cardScale()}
      out:scale={cardScaleOut()}
      class="w-full max-w-md gp-card shadow-float rounded-2xl overflow-hidden flex flex-col font-sans text-xs gp-gpu"
    >
      <div class="p-4 border-b border-border/60 gp-section-edge flex items-center justify-between">
        <h2 id="clone-modal-title" class="flex items-center gap-2 text-sm font-semibold text-textPrimary">
          <Download size={16} class="text-accent" />
          <span>Clone Git Repository</span>
        </h2>
      </div>

      <div class="p-4 space-y-3">
        {#if errorMsg}
          <div class="p-2 bg-rose-500/10 border border-rose-500/30 rounded-xl text-rose-400 text-xs">
            {errorMsg}
          </div>
        {/if}

        <div>
          <label for="clone-url" class="block text-textMuted text-[11px] mb-1.5">Repository URL</label>
          <input
            id="clone-url"
            type="text"
            bind:value={url}
            placeholder="https://github.com/owner/repo.git or git@github.com:..."
            class="gp-field w-full font-mono"
          />
        </div>

        <div>
          <label for="clone-dest" class="block text-textMuted text-[11px] mb-1.5">Destination Directory</label>
          <div class="flex items-center gap-2">
            <input
              id="clone-dest"
              type="text"
              bind:value={targetDir}
              placeholder="/path/to/folder"
              class="gp-field flex-1 min-w-0 font-mono"
            />
            <button
              onclick={pickTargetDir}
              title="Choose directory"
              class="gp-btn px-2.5! py-1.5! shrink-0"
            >
              <FolderOpen size={14} />
            </button>
          </div>
        </div>

        <details bind:open={showOptions} class="text-[11px]">
          <summary class="cursor-pointer text-textMuted select-none">Options</summary>
          <div class="mt-2 space-y-2">
            <div>
              <label for="clone-branch" class="block text-textMuted text-[11px] mb-1">Branch or tag (optional)</label>
              <input id="clone-branch" type="text" bind:value={branch} disabled={isCloning} placeholder="Remote default" class="gp-field w-full font-mono" />
            </div>
            <div>
              <label for="clone-depth" class="block text-textMuted text-[11px] mb-1">History depth (optional)</label>
              <input id="clone-depth" type="text" inputmode="numeric" bind:value={depthText} disabled={isCloning} placeholder="Full history" class="gp-field w-full font-mono" />
              {#if depth === "invalid"}<p class="mt-1 text-[10px] text-amber-400">Enter a whole number from 1 to {MAX_CLONE_DEPTH.toLocaleString()}, or leave empty for full history.</p>
              {:else if depth !== null}<p class="mt-1 text-[10px] text-textMuted">Shallow clone: only the last {depth} {depth === 1 ? "commit" : "commits"}.</p>{/if}
            </div>
            <label class="inline-flex items-center gap-1.5"><input type="checkbox" bind:checked={recurseSubmodules} disabled={isCloning} />Also clone submodules</label>
          </div>
        </details>

        {#if isCloning}
          <div role="status" aria-live="polite" class="space-y-1">
            <div class="flex justify-between text-[11px] text-textMuted">
              <span>{progress?.phase ?? "Starting clone…"}</span>
              {#if progress?.percent != null}<span>{progress.percent}%</span>{/if}
            </div>
            <div class="h-1.5 rounded-full bg-surfaceHover overflow-hidden">
              {#if progress?.percent != null}
                <div class="h-full bg-accent transition-[width]" style="width: {progress.percent}%"></div>
              {:else}
                <div class="h-full w-1/3 bg-accent/60 animate-pulse"></div>
              {/if}
            </div>
          </div>
        {/if}
      </div>

      <div class="p-4 border-t border-border/60 gp-section-edge bg-surfaceHover/30 flex justify-end gap-2">
        <button onclick={requestClose} disabled={isCloning} class="gp-btn disabled:opacity-40 disabled:cursor-not-allowed">Cancel</button>
        <button
          onclick={handleClone}
          disabled={!url.trim() || !targetDir.trim() || isCloning || depth === "invalid"}
          class="gp-btn-primary"
        >
          <Check size={14} />
          <span>{isCloning ? "Cloning..." : "Clone Repository"}</span>
        </button>
      </div>
    </div>
  </div>
{/if}
