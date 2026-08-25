<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { fade, scale } from "svelte/transition";
  import { repoStore } from "../stores/repoStore";
  import { fadeParams, scaleParams } from "../motion/easing";
  import { guardedDismiss } from "./modalGuard";
  import { trapFocus } from "../ui/focusTrap";
  import { LAYERS } from "../ui/layers";
  import { formatError } from "../ui/formatError";
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
      errorMsg = formatError(err);
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
      errorMsg = formatError(err);
    } finally {
      isCloning = false;
    }
  }

  // Backdrop, Escape, and Cancel share one gate: while a clone runs, none of
  // them may close the dialog the Cancel button is already guarding.
  function requestClose() {
    guardedDismiss(isCloning, onClose);
  }
</script>

{#if isOpen}
  <div
    role="dialog"
    aria-modal="true"
    tabindex="-1"
    onclick={requestClose}
    onkeydown={(e) => e.key === "Escape" && requestClose()}
    transition:fade={fadeParams()}
    class="fixed inset-0 bg-black/40 backdrop-blur-sm flex items-center justify-center p-4 select-none gp-gpu"
    style="z-index: {LAYERS.MODAL}"
  >
    <!-- svelte-ignore a11y_click_events_have_key_events -->
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div
      use:trapFocus
      onclick={(e) => e.stopPropagation()}
      in:scale={scaleParams()}
      out:scale={scaleParams()}
      class="w-full max-w-md gp-card shadow-float rounded-2xl overflow-hidden flex flex-col font-sans text-xs gp-gpu"
    >
      <div class="p-4 border-b border-border/60 flex items-center justify-between">
        <div class="flex items-center gap-2 text-sm font-semibold text-textPrimary">
          <Download size={16} class="text-accent" />
          <span>Clone Git Repository</span>
        </div>
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
