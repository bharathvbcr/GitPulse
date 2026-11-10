<script lang="ts">
  import { conflictComparisonRows } from "../../diff/conflictPresentation";

  let {
    path,
    disk,
    draft,
    busy = false,
    unbased = false,
    overwriteNote = null,
    onReload,
    onOverwrite,
  }: {
    path: string;
    /** The file as it is on disk now, as text. */
    disk: string;
    /** The unsaved draft, as the textarea holds it (LF). */
    draft: string;
    busy?: boolean;
    /** The draft has no recorded disk version, so a save cannot say whether the file changed. */
    unbased?: boolean;
    /** Why Overwrite writes the draft exactly as typed (the disk file is not faithfully editable), or null. */
    overwriteNote?: string | null;
    onReload: () => void;
    onOverwrite: () => void;
  } = $props();

  let comparing = $state(false);
  // The textarea hands back LF, so the disk side is shown the same way or every
  // line of a CRLF file would read as changed.
  const diskText = $derived(disk.replace(/\r\n/g, "\n"));
  const rows = $derived(comparing ? conflictComparisonRows(diskText, draft) : null);
</script>

<div
  class="shrink-0 bg-warn/15 px-3 py-2 text-xs"
  role="alert"
  data-testid="editor-conflict"
>
  <div class="flex flex-wrap items-center gap-2">
    <span class="font-semibold text-warn">{unbased ? "Version unknown" : "Changed on disk"}</span>
    <span class="min-w-0 flex-1 text-textMuted wrap-break-word">
      {#if unbased}
        {path} has no recorded disk version, so a save cannot tell whether it changed.
      {:else}
        {path} was changed after you started editing.
      {/if}
      Your draft is kept and nothing was written.
    </span>
    <button
      type="button"
      class="gp-btn"
      data-size="sm"
      title="Read the file from disk again and discard your draft"
      disabled={busy}
      onclick={onReload}>Refresh</button
    >
    <button
      type="button"
      class="gp-btn"
      data-size="sm"
      disabled={busy}
      aria-pressed={comparing}
      onclick={() => (comparing = !comparing)}
    >
      Compare
    </button>
    <button type="button" class="gp-btn" data-variant="danger" data-size="sm" disabled={busy} onclick={onOverwrite}>
      Overwrite
    </button>
  </div>
  {#if overwriteNote}
    <p class="mt-1 text-textMuted" data-testid="editor-conflict-overwrite-note">{overwriteNote}</p>
  {/if}
  {#if comparing}
    <div class="mt-2 grid grid-cols-2 gap-2 max-sm:grid-cols-1" data-testid="editor-conflict-compare">
      {#each [true, false] as onDisk (onDisk)}
        <div class="min-w-0">
          <div class="gp-label mb-1">
            {onDisk ? "On disk" : "Your draft"}
          </div>
          <div
            class="max-h-64 overflow-auto rounded border border-edgeSoft bg-black/20 p-2 font-mono text-meta leading-5 whitespace-pre select-text"
            tabindex="0"
            role="textbox"
            aria-readonly="true"
            aria-multiline="true"
            aria-label={onDisk ? "File on disk" : "Your unsaved draft"}
          >
            {#if rows}
              {#each rows as row, index (index)}
                <div>{#each onDisk ? row.ours : row.theirs as part, partIndex (partIndex)}<span class={part.kind !== "Equal" ? "bg-warn/25 rounded-sm" : ""}>{part.text}</span>{/each}</div>
              {/each}
            {:else}
              {onDisk ? diskText : draft}
            {/if}
          </div>
        </div>
      {/each}
    </div>
    {#if !rows}
      <p class="mt-1 text-micro text-textMuted">Complete plain text. Word highlighting is limited to smaller files.</p>
    {/if}
  {/if}
</div>
