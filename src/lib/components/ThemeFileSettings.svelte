<script lang="ts">
  import { Download, FileUp, Copy, X } from "@lucide/svelte";
  import { interfaceStore } from "../stores/interfaceStore";
  import { themeStore } from "../stores/themeStore";
  import { copyText } from "../desktop/clipboard";
  import { describeFailure } from "../ui/copy/describeFailure";
  import {
    MAX_THEME_FILE_BYTES,
    exportCurrentTheme,
    importThemeText,
    themeToPatch,
  } from "../ui/appearance/themeFile";

  /**
   * Theme import and export (`gitpulse-theme/1`, design language §11).
   *
   * Import is a file pick whose text goes through the theme validator before
   * anything is applied; a file that fails shows what was wrong and changes
   * nothing. Export writes the colors the window is using right now, after the
   * contrast guard, so the file round-trips.
   */
  let picker = $state<HTMLInputElement>();
  let exportName = $state("My theme");
  let failure = $state<{ headline: string; detail?: string } | null>(null);
  let status = $state("");

  const theme = $derived($themeStore);
  const active = $derived($interfaceStore.customTheme);

  function fail(verb: string, object: string, reason: string) {
    const copy = describeFailure({ verb, object }, new Error(reason));
    failure = { headline: copy.headline, detail: copy.detail };
    status = "";
  }

  async function onPick(event: Event & { currentTarget: HTMLInputElement }) {
    const input = event.currentTarget;
    const file = input.files?.[0];
    // Let the same file be picked again after a fix.
    input.value = "";
    if (!file) return;
    failure = null;
    status = "";
    if (file.size > MAX_THEME_FILE_BYTES) {
      fail("import", "the theme", `The file is larger than ${MAX_THEME_FILE_BYTES / 1024} KB, which is more than a theme needs.`);
      return;
    }
    let text: string;
    try {
      text = await file.text();
    } catch (error) {
      failure = describeFailure({ verb: "read", object: "the file" }, error);
      return;
    }
    const result = importThemeText(text);
    if (!result.ok) {
      fail("import", "the theme", result.reason);
      return;
    }
    interfaceStore.patchAppearance(themeToPatch(result.theme));
    exportName = result.theme.name;
    status =
      result.ignoredKeys.length > 0
        ? `Imported “${result.theme.name}”. Ignored ${result.ignoredKeys.length} unknown ${result.ignoredKeys.length === 1 ? "key" : "keys"}: ${result.ignoredKeys.slice(0, 3).join(", ")}.`
        : `Imported “${result.theme.name}”.`;
  }

  function currentExport() {
    return exportCurrentTheme($interfaceStore, theme, exportName.trim() === "" ? "My theme" : exportName);
  }

  function exportFile() {
    failure = null;
    const result = currentExport();
    if (!result.ok) {
      fail("export", "the theme", result.reason);
      return;
    }
    try {
      const url = URL.createObjectURL(new Blob([result.json], { type: "application/json" }));
      const link = document.createElement("a");
      link.href = url;
      link.download = result.fileName;
      document.body.append(link);
      link.click();
      link.remove();
      setTimeout(() => URL.revokeObjectURL(url), 1000);
      // A download request is all this can know. The webview decides whether a file
      // lands, so the status does not claim one did; Copy JSON is the sure path.
      status = `Requested a download of ${result.fileName}. If no file appears, use Copy JSON.`;
    } catch (error) {
      failure = describeFailure({ verb: "export", object: "the theme" }, error);
    }
  }

  async function copyJson() {
    failure = null;
    const result = currentExport();
    if (!result.ok) {
      fail("export", "the theme", result.reason);
      return;
    }
    if (await copyText(result.json)) status = "Copied the theme JSON.";
    else failure = describeFailure({ verb: "copy", object: "the theme" }, new Error("The clipboard refused the text."));
  }

  function removeTheme() {
    failure = null;
    interfaceStore.setCustomTheme(null);
    status = "Back to the built-in palette.";
  }
</script>

<div class="flex flex-col gap-2" data-theme-file>
  <div class="flex flex-wrap items-center gap-2">
    <input
      bind:this={picker}
      type="file"
      accept="application/json,.json"
      hidden
      aria-label="Theme file"
      data-theme-file-input
      onchange={onPick}
    />
    <button type="button" class="gp-btn" data-variant="secondary" data-size="sm" onclick={() => picker?.click()}>
      <FileUp size={12} />
      Import theme…
    </button>
    <button type="button" class="gp-btn" data-variant="secondary" data-size="sm" onclick={exportFile}>
      <Download size={12} />
      Export theme
    </button>
    <button type="button" class="gp-btn" data-variant="ghost" data-size="sm" onclick={copyJson}>
      <Copy size={12} />
      Copy JSON
    </button>
  </div>

  <label class="flex items-center gap-2 text-meta text-textMuted">
    <span class="shrink-0">Name in the file</span>
    <input
      type="text"
      class="gp-field min-w-0 flex-1"
      bind:value={exportName}
      maxlength="64"
      aria-label="Theme name for export"
    />
  </label>

  {#if active}
    <div class="flex items-center justify-between gap-2 text-meta" data-theme-active>
      <span class="min-w-0 text-textPrimary">
        Using “{active.name}”
        <span class="text-textMuted">
          — draws in {active.appearance} mode{active.appearance === theme ? "" : ", so this window uses the built-in palette"}
        </span>
      </span>
      <button type="button" class="gp-btn shrink-0" data-variant="ghost" data-size="sm" onclick={removeTheme}>
        <X size={12} />
        Remove
      </button>
    </div>
  {/if}

  {#if failure}
    <div class="gp-notice" data-tone="danger" role="alert" data-theme-failure>
      <div class="min-w-0 flex-1">
        <div class="font-medium">{failure.headline}</div>
        {#if failure.detail}<div class="text-textMuted">{failure.detail}</div>{/if}
      </div>
    </div>
  {:else if status}
    <p class="text-meta text-textMuted" role="status" data-theme-status>{status}</p>
  {/if}

  <p class="text-micro text-textMuted">
    A theme is a small JSON file: colors, an optional corner scale, background field and font names. Anything else
    in a file, including CSS, URLs, filters and layering, is refused.
  </p>
</div>
