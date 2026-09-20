<script lang="ts">
  /**
   * The impact surfaces, rendered at real widths.
   *
   * Unit tests pin the wording and the honesty ladder; SSR tests pin what is
   * in the markup. Neither can see the thing that actually broke this screen:
   * an unbounded `<p>` inside a single-line flex row, which wraps to two lines
   * and drags every sibling in the row down with it. That is geometry, and
   * only a real browser settles it.
   *
   * The header below is a faithful stand-in for DiffViewer's identity row —
   * same chips, same order, same `ml-auto` split — so the rung control is
   * measured in the row it actually has to survive.
   */
  import BlastRadiusPanel from "../src/lib/components/BlastRadiusPanel.svelte";
  import RungFilterControl from "../src/lib/components/RungFilterControl.svelte";
  import { hedgedCount } from "../src/lib/codeintel/blastGlance";
  import { fileGlance, markerForHonesty } from "../src/lib/codeintel/previewSummary";
  import type { PreviewFileHonesty } from "../src/lib/codeintel/previewSummary";
  import type { ComposedBlastRadius } from "../src/lib/codeintel/blastCompose";
  import type { CodeintelRungHistogram } from "../src/lib/codeintel/types";
  import { formatPathParts } from "../src/lib/files/formatPath";
  import { themeStore } from "../src/lib/stores/themeStore";

  let {
    blast,
    histogram,
    previewFiles,
    width,
    setMode,
  }: {
    blast: ComposedBlastRadius | null;
    histogram: CodeintelRungHistogram | null;
    previewFiles: PreviewFileHonesty[];
    width: number;
    setMode: (mode: string) => void;
  } = $props();

  let minRung = $state<"all">("all");
  const impactEdges = 2636;
</script>

<main class="flex h-screen flex-col gap-3 bg-background p-3 text-textPrimary">
  <div class="flex shrink-0 flex-wrap gap-2">
    <button class="gp-btn" onclick={() => setMode("floor")}>Lower bound</button>
    <button class="gp-btn" onclick={() => setMode("exact")}>Complete</button>
    <button class="gp-btn" onclick={() => setMode("unavailable")}>Unavailable</button>
    <button class="gp-btn" onclick={() => setMode("interrupted")}>Interrupted</button>
    <button class="gp-btn" onclick={() => setMode("narrow")}>Toggle narrow</button>
    <button
      class="gp-btn"
      onclick={() => themeStore.setTheme($themeStore === "dark" ? "light" : "dark")}
      >Toggle theme</button
    >
  </div>

  <div
    class="flex min-h-0 flex-1 flex-col gap-3 overflow-hidden rounded border border-border"
    style:width={`${width}px`}
    data-impact-stage
  >
    <!-- DiffViewer's identity row, same shape. -->
    <div
      data-impact-header
      class="flex shrink-0 select-none flex-wrap items-center gap-x-2 gap-y-1 border-b border-border/60 bg-surface/60 px-3 py-1.5 font-sans text-xs"
    >
      <span class="min-w-0 truncate font-medium text-textPrimary"
        >Sources/ExpanderEngine/Engine/AXContextChecker.swift</span
      >
      <span class="shrink-0 rounded bg-amber-500/15 px-1 font-mono text-[10px] text-amber-600">M</span>
      <span class="shrink-0 font-mono text-[10px] tabular-nums text-textMuted">+44 −3</span>
      <span class="shrink-0 text-[10px] text-textMuted">77 lines</span>
      <span class="shrink-0 rounded-full border border-border/70 px-1.5 text-[10px] text-textMuted"
        >unstaged</span
      >
      <span
        data-impact-chip
        class="shrink-0 rounded-full border border-accent/30 bg-accent/15 px-2 py-0.5 text-[10px] text-accent"
      >
        {hedgedCount(impactEdges, true, "caller")}
        <span class="opacity-70">· this file</span>
      </span>
      <div data-impact-rung>
        <RungFilterControl bind:minRung {histogram} layeredImpactActive={false} />
      </div>
      <div class="ml-auto flex shrink-0 items-center gap-2">
        <button class="gp-btn-primary py-1!">Stage File</button>
      </div>
    </div>

    <div class="shrink-0 px-3" data-impact-panel>
      <BlastRadiusPanel {blast} title="Blast radius · all 7 changed files" />
    </div>

    <!-- The commit sidebar's per-file preview rows, at sidebar width. -->
    <div class="shrink-0 px-3" data-impact-preview style="max-width:300px">
      <ul class="flex flex-col gap-px">
        {#each previewFiles as file (file.file_path)}
          {@const marker = markerForHonesty(file)}
          <li
            data-impact-preview-row
            class="flex items-baseline gap-1.5 rounded px-1 py-0.5 text-[10px] leading-snug"
            title={marker.title}
          >
            <span
              data-impact-preview-dot={marker.kind}
              class="mt-px size-1.5 shrink-0 rounded-full {marker.kind === 'breaks'
                ? 'bg-rose-500'
                : marker.kind === 'clean'
                  ? 'bg-emerald-500'
                  : 'bg-amber-500'}"
            ></span>
            <span class="min-w-0 shrink truncate font-mono text-textSecondary"
              >{formatPathParts(file.file_path).name}</span
            >
            <span class="min-w-0 flex-1 truncate text-right text-textMuted"
              >{fileGlance(file)}</span
            >
          </li>
        {/each}
      </ul>

      <!-- CommitComposer's footer row, same classes. "Include unstaged" used
           to break across two lines inside its own label at sidebar width. -->
      <div
        data-impact-commit-footer
        class="mt-2 flex flex-wrap items-center justify-between gap-x-2 gap-y-1.5"
      >
        <div class="flex min-w-0 flex-wrap items-center gap-x-3 gap-y-1.5">
          <label
            data-impact-toggle
            class="flex shrink-0 cursor-pointer items-center gap-1.5 whitespace-nowrap text-[11px] text-textMuted"
          >
            <input type="checkbox" class="rounded accent-accent" />
            <span>Amend</span>
          </label>
          <label
            data-impact-toggle
            class="flex shrink-0 cursor-pointer items-center gap-1.5 whitespace-nowrap text-[11px] text-textMuted"
          >
            <input type="checkbox" class="rounded accent-accent" />
            <span>Include unstaged</span>
          </label>
        </div>
        <button class="gp-btn-primary shrink-0 whitespace-nowrap">Commit (2)</button>
      </div>
    </div>
  </div>
</main>
