<script lang="ts">
  /**
   * Rendered Markdown in the app's document — the one place that inserts it.
   *
   * The HTML is already sanitized by MarkDev's renderer; what this owns is
   * GitPulse's side of the boundary: ids are namespaced before insertion, and
   * every link is judged on click (`markdownLinks.ts`) so the webview never
   * follows one. A link GitPulse will not open says why, inline.
   */
  import { get } from "svelte/store";
  import { openExternal } from "../desktop/openExternal";
  import { copyText } from "../desktop/clipboard";
  import { repoStore } from "../stores/repoStore";
  import {
    findFragmentTarget,
    handleMarkdownClick,
    prepareRenderedMarkdown,
    type MarkdownNote,
  } from "../files/markdownLinks";

  let {
    html,
    note = null,
    class: className = "",
  }: {
    html: string;
    /** Where the note lives; `null` for text that is not a file. */
    note?: MarkdownNote | null;
    class?: string;
  } = $props();

  let container: HTMLDivElement | undefined = $state();
  let notice = $state<string | null>(null);
  const prepared = $derived(prepareRenderedMarkdown(html, note));

  $effect(() => {
    // A notice describes the document it was raised on.
    void prepared;
    notice = null;
  });

  function openFile(path: string, fragment: string | null) {
    if (!note || !container) return;
    if (path === note.path) {
      const target = fragment ? findFragmentTarget(container, fragment) : null;
      if (fragment && !target) notice = `This document has no section "${fragment}".`;
      (target ?? container).scrollIntoView({ behavior: "smooth", block: "start" });
      return;
    }
    // `selectFilePath` acts on the active repository tab; a note from another
    // checkout must not open a same-named file in this one.
    const state = get(repoStore);
    const activePath = state.openTabs.find((tab) => tab.id === state.activeTabId)?.path ?? "";
    if (activePath !== note.repoPath) {
      notice = "Switch to this note's repository to open its files.";
      return;
    }
    repoStore.selectFilePath(path);
    repoStore.setActiveTab("code");
  }

  function onActivate(event: MouseEvent) {
    // Middle click would open a link in a new webview window; claim it too.
    if (event.type === "auxclick" && event.button !== 1) return;
    if (!container) return;
    notice = null;
    handleMarkdownClick(event, {
      container,
      note,
      openExternal,
      openFile,
      copy: copyText,
      report: (message) => (notice = message),
    });
  }

  $effect(() => {
    const el = container;
    if (!el) return;
    el.addEventListener("click", onActivate);
    el.addEventListener("auxclick", onActivate);
    return () => {
      el.removeEventListener("click", onActivate);
      el.removeEventListener("auxclick", onActivate);
    };
  });
</script>

{#if notice}
  <div class="gp-md-notice" role="status">
    <span>{notice}</span>
    <button type="button" class="gp-md-notice-dismiss" onclick={() => (notice = null)} aria-label="Dismiss">×</button>
  </div>
{/if}
<div bind:this={container} class="gp-markdown {className}">{@html prepared}</div>
