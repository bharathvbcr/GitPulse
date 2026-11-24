<script lang="ts">
  import { Check, Copy, Download, FileUp, Save, X } from "@lucide/svelte";
  import { interfaceStore } from "../stores/interfaceStore";
  import { themeStore } from "../stores/themeStore";
  import { copyText } from "../desktop/clipboard";
  import { saveExport } from "../desktop/saveFile";
  import { describeFailure } from "../ui/copy/describeFailure";
  import { sameTheme } from "../ui/appearance/presets";
  import { MAX_SAVED_THEMES } from "../ui/appearance/savedThemes";
  import {
    exportCurrentTheme,
    importThemeFile,
    themeToPatch,
  } from "../ui/appearance/themeFile";
  import { MAX_VSCODE_JSON_BYTES, MAX_VSIX_BYTES } from "../ui/appearance/vscodeTheme";
  import type { GitPulseTheme } from "../ui/appearance/themeSchema";
  import ThemeTile from "./ThemeTile.svelte";

  /**
   * My themes: the reader's shelf of saved and imported themes, and the
   * `gitpulse-theme/1` file in and out (design language §11).
   *
   * One name serves Save, Export and Copy JSON, and an import fills it in.
   * Import is a file pick whose text goes through the theme validator before
   * anything is applied; a file that fails shows what was wrong and changes
   * nothing. Export writes the colors the window is using right now, after the
   * contrast guard, so the file round-trips. Applying a saved theme leaves the
   * wallpaper alone; removing one from the shelf does not take it out of force
   * (Remove theme above does that).
   */
  let picker = $state<HTMLInputElement>();
  let name = $state("My theme");
  let failure = $state<{ headline: string; detail?: string } | null>(null);
  let status = $state("");

  const prefs = $derived($interfaceStore);
  const theme = $derived($themeStore);
  const active = $derived(prefs.customTheme);
  const chosenName = $derived(name.trim() === "" ? "My theme" : name.trim());

  function say(text: string) {
    failure = null;
    status = text;
  }

  function fail(verb: string, object: string, reason: string) {
    const copy = describeFailure({ verb, object }, new Error(reason));
    failure = { headline: copy.headline, detail: copy.detail };
    status = "";
  }

  function applySaved(saved: GitPulseTheme) {
    interfaceStore.applyTheme(saved);
    say(`Applied “${saved.name}”.`);
  }

  function removeSaved(saved: GitPulseTheme) {
    interfaceStore.removeSavedTheme(saved);
    say(`Removed “${saved.name}” from your themes.`);
  }

  function saveCurrent() {
    const result = exportCurrentTheme(prefs, theme, chosenName);
    if (!result.ok) {
      fail("save", "the theme", result.reason);
      return;
    }
    const outcome = interfaceStore.saveTheme(result.theme);
    if (outcome === "full") {
      fail("save", "the theme", `You already have ${MAX_SAVED_THEMES} saved themes. Remove one to save another.`);
      return;
    }
    say(outcome === "replaced" ? `Updated “${result.theme.name}”.` : `Saved “${result.theme.name}” to your themes.`);
  }

  async function onPick(event: Event & { currentTarget: HTMLInputElement }) {
    const input = event.currentTarget;
    const file = input.files?.[0];
    // Let the same file be picked again after a fix.
    input.value = "";
    if (!file) return;
    failure = null;
    status = "";
    if (/\.vsix$/i.test(file.name) && file.size > MAX_VSIX_BYTES) {
      fail("import", "the theme", `The .vsix file is larger than ${MAX_VSIX_BYTES / (1024 * 1024)} MB.`);
      return;
    }
    if (!/\.vsix$/i.test(file.name) && file.size > MAX_VSCODE_JSON_BYTES) {
      fail("import", "the theme", `The file is larger than ${MAX_VSCODE_JSON_BYTES / 1024} KB, which is more than a theme needs.`);
      return;
    }
    let result;
    try {
      result = await importThemeFile(file, { preferredAppearance: theme });
    } catch (error) {
      failure = describeFailure({ verb: "read", object: "the file" }, error);
      return;
    }
    if (!result.ok) {
      fail("import", "the theme", result.reason);
      return;
    }
    interfaceStore.patchAppearance(themeToPatch(result.theme));
    themeStore.setTheme(result.theme.appearance);
    // Kept on the shelf too, so it can be switched back to after trying another.
    const shelved = interfaceStore.saveTheme(result.theme);
    name = result.theme.name;
    const kept = shelved === "full" ? " Your themes are full, so it wasn't added to them." : "";
    status =
      result.ignoredKeys.length > 0
        ? `Imported “${result.theme.name}”. Ignored ${result.ignoredKeys.length} unknown ${result.ignoredKeys.length === 1 ? "key" : "keys"}: ${result.ignoredKeys.slice(0, 3).join(", ")}.${kept}`
        : `Imported “${result.theme.name}”.${kept}`;
  }

  async function exportFile() {
    failure = null;
    status = "";
    const result = exportCurrentTheme(prefs, theme, chosenName);
    if (!result.ok) {
      fail("export", "the theme", result.reason);
      return;
    }
    try {
      const outcome = await saveExport({ kind: "theme", fileName: result.fileName, data: result.json });
      // A cancelled save panel is the person's choice, not a failure: say nothing.
      if (outcome.status === "saved") status = `Saved ${outcome.fileName}.`;
      // Without a native shell a download request is all this can know, so the
      // status does not claim a file landed; Copy JSON is the sure path.
      else if (outcome.status === "requested")
        status = `Requested a download of ${outcome.fileName}. If no file appears, use Copy JSON.`;
    } catch (error) {
      failure = describeFailure({ verb: "save", object: "the theme" }, error);
    }
  }

  async function copyJson() {
    failure = null;
    const result = exportCurrentTheme(prefs, theme, chosenName);
    if (!result.ok) {
      fail("export", "the theme", result.reason);
      return;
    }
    if (await copyText(result.json)) status = "Copied the theme JSON.";
    else failure = describeFailure({ verb: "copy", object: "the theme" }, new Error("The clipboard refused the text."));
  }
</script>

<div class="flex flex-col gap-2" data-theme-file data-saved-themes>
  <div class="flex items-center justify-between gap-2">
    <span class="text-meta font-medium text-textPrimary">My themes</span>
    <span class="text-micro text-textMuted">{prefs.savedThemes.length} of {MAX_SAVED_THEMES}</span>
  </div>

  {#if prefs.savedThemes.length === 0}
    <p class="text-micro text-textMuted" data-saved-themes-empty>
      Save the look you have now, or import a theme file. Imported themes are kept here too.
    </p>
  {:else}
    <div class="grid grid-cols-2 gap-2 sm:grid-cols-3" role="group" aria-label="My themes">
      {#each prefs.savedThemes as saved (`${saved.appearance}:${saved.name}`)}
        {@const picked = sameTheme(active, saved)}
        <div
          class="relative flex flex-col gap-1.5 rounded-lg border p-1.5 {picked
            ? 'border-accent bg-accent/10'
            : 'border-edge'}"
          data-saved-theme={saved.name}
        >
          <button
            type="button"
            class="flex flex-col gap-1.5 text-left"
            aria-pressed={picked}
            title="Apply “{saved.name}” ({saved.appearance})"
            onclick={() => applySaved(saved)}
          >
            <ThemeTile theme={saved} />
            <span class="flex items-center gap-1 px-0.5 pr-5">
              <span class="truncate text-meta font-medium text-textPrimary">{saved.name}</span>
              {#if picked}<Check size={12} class="shrink-0 text-accent" />{/if}
            </span>
          </button>
          <button
            type="button"
            class="gp-icon-btn absolute right-1 bottom-1"
            aria-label="Remove “{saved.name}” from my themes"
            title="Remove from my themes"
            onclick={() => removeSaved(saved)}
          >
            <X size={12} />
          </button>
        </div>
      {/each}
    </div>
  {/if}

  <input
    bind:this={picker}
    type="file"
    accept="application/json,.json,.jsonc,.vsix"
    hidden
    aria-label="Theme file"
    data-theme-file-input
    onchange={onPick}
  />
  <div class="flex flex-wrap items-center gap-2">
    <input
      type="text"
      class="gp-field min-w-0 flex-1"
      bind:value={name}
      maxlength="64"
      aria-label="Theme name"
      placeholder="Theme name"
      data-save-theme-name
    />
    <button type="button" class="gp-btn" data-variant="secondary" data-size="sm" onclick={saveCurrent}>
      <Save size={12} />
      Save current look
    </button>
  </div>
  <div class="flex flex-wrap items-center gap-2">
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
    Save, Export and Copy JSON use the name above. Import supports <code>.gitpulse-theme.json</code> and VS Code
    <code>.json</code>/<code>.jsonc</code>/<code>.vsix</code> color themes. CSS, URLs, filters and layering are refused.
  </p>
</div>
