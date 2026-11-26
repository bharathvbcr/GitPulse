<script lang="ts">
  /**
   * A list of hunks as a unified diff. Line numbers show when the agent
   * reported them (`structuredPatch`, `git diff`); a proposed edit built from
   * a request has none yet, and says so in its header instead of inventing
   * them. Signs are printed, so the diff never relies on colour alone.
   */
  import type { MaviHunk } from "../../mavi/types";

  let {
    hunks,
    numbered = true,
    maxLines = 600,
  }: {
    hunks: readonly MaviHunk[];
    numbered?: boolean;
    maxLines?: number;
  } = $props();

  type Line = { sign: "+" | "-" | " "; text: string; old: number | null; new: number | null };

  let blocks = $derived.by(() => {
    let budget = maxLines;
    let hidden = 0;
    const out: { header: string; lines: Line[] }[] = [];
    for (const hunk of hunks) {
      let o = hunk.oldStart;
      let n = hunk.newStart;
      const lines: Line[] = [];
      for (const raw of hunk.lines) {
        const sign = (raw[0] === "+" || raw[0] === "-" ? raw[0] : " ") as Line["sign"];
        const text = raw.length ? raw.slice(1) : "";
        const line: Line = {
          sign,
          text,
          old: numbered && sign !== "+" ? o : null,
          new: numbered && sign !== "-" ? n : null,
        };
        if (sign !== "+") o += 1;
        if (sign !== "-") n += 1;
        if (budget > 0) {
          lines.push(line);
          budget -= 1;
        } else {
          hidden += 1;
        }
      }
      const header = numbered
        ? `@@ −${hunk.oldStart},${hunk.oldLines} +${hunk.newStart},${hunk.newLines}`
        : "Proposed: line numbers are known once it is applied";
      if (lines.length) out.push({ header, lines });
    }
    return { out, hidden };
  });
</script>

<div class="mavi-hunks font-mono text-meta select-text" role="table" aria-label="Changes">
  {#each blocks.out as block, b (b)}
    <div class="mavi-hunk-head px-2.5 py-1 text-micro text-textMuted" role="row">{block.header}</div>
    {#each block.lines as line, i (i)}
      <div
        class="mavi-line flex"
        class:bg-ok-tint={line.sign === "+"}
        class:bg-danger-tint={line.sign === "-"}
        role="row"
      >
        {#if numbered}
          <span class="mavi-num text-faint tabular-nums" aria-hidden="true">{line.old ?? ""}</span>
          <span class="mavi-num text-faint tabular-nums" aria-hidden="true">{line.new ?? ""}</span>
        {/if}
        <span
          class="mavi-sign"
          class:text-ok={line.sign === "+"}
          class:text-danger={line.sign === "-"}
          aria-label={line.sign === "+" ? "added" : line.sign === "-" ? "removed" : "context"}
        >{line.sign}</span>
        <span class="whitespace-pre-wrap break-all min-w-0 flex-1 text-textPrimary">{line.text || " "}</span>
      </div>
    {/each}
  {/each}
  {#if blocks.hidden > 0}
    <div class="px-2.5 py-1.5 text-micro text-textMuted">{blocks.hidden} more lines not shown</div>
  {/if}
</div>

<style>
  .mavi-hunks {
    border-radius: var(--gp-r-sm);
    overflow: hidden;
    border: 1px solid var(--gp-edge);
  }
  .mavi-hunk-head {
    background: rgb(var(--c-surface-hover) / 0.65);
    border-bottom: 1px solid var(--gp-edge-soft);
  }
  .mavi-line {
    line-height: 1.55;
    padding-right: 8px;
  }
  .mavi-num {
    flex: 0 0 auto;
    width: 3.25em;
    padding-right: 0.5em;
    text-align: right;
    user-select: none;
  }
  .mavi-sign {
    flex: 0 0 auto;
    width: 1.35em;
    text-align: center;
    user-select: none;
  }
  .bg-ok-tint {
    background-color: rgb(var(--c-ok) / 0.1);
  }
  .bg-danger-tint {
    background-color: rgb(var(--c-danger) / 0.1);
  }
</style>
