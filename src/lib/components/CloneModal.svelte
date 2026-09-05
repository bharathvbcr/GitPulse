<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
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
  import { Download, FolderOpen, Check } from "lucide-svelte";

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

  async function pickTargetDir() {
    try {
      const folder = await invoke<string | null>("cmd_pick_folder");
      if (folder) targetDir = folder;
    } catch (err) {
      errorMsg = reportPanelError("clone", err);
    }
  }

  async function handleClone() {
    if (!url.trim() || !targetDir.trim()) return;
    isCloning = true;
    errorMsg = null;
    try {
      const clonedPath = await invoke<string>("cmd_clone_repo", {
        url: url.trim(),
        targetDir: targetDir.trim(),
      });
      await repoStore.openRepo(clonedPath);
      onClose?.();
    } catch (err: unknown) {
      errorMsg = reportPanelError("clone", err);
    } finally {
      isCloning = false;
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
    class="fixed inset-0 bg-black/40 backdrop-blur-sm flex items-center justify-center p-4 select-none gp-gpu"
    style="z-index: {LAYERS.MODAL}"
  >
    <div
      use:trapFocus
      in:scale={cardScale()}
      out:scale={cardScaleOut()}
      class="w-full max-w-md gp-card shadow-float rounded-2xl overflow-hidden flex flex-col font-sans text-xs gp-gpu"
    >
      <div class="p-4 border-b border-border/60 flex items-center justify-between">
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
              class="gp-btn !px-2.5 !py-1.5 shrink-0"
            >
              <FolderOpen size={14} />
            </button>
          </div>
        </div>
      </div>

      <div class="p-4 border-t border-border/60 bg-surfaceHover/30 flex justify-end gap-2">
        <button onclick={requestClose} disabled={isCloning} class="gp-btn disabled:opacity-40 disabled:cursor-not-allowed">Cancel</button>
        <button
          onclick={handleClone}
          disabled={!url.trim() || !targetDir.trim() || isCloning}
          class="gp-btn-primary"
        >
          <Check size={14} />
          <span>{isCloning ? "Cloning..." : "Clone Repository"}</span>
        </button>
      </div>
    </div>
  </div>
{/if}
