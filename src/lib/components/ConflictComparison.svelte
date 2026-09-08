<script lang="ts">
  import type { ConflictChunk, ConflictResolutionChoice } from "../diff/conflict";
  import { conflictComparisonRows } from "../diff/conflictPresentation";
  let { chunk, disabled, choose }: { chunk: ConflictChunk; disabled: boolean; choose: (choice: ConflictResolutionChoice) => void } = $props();
  const rows = $derived(conflictComparisonRows(chunk.ours_content, chunk.theirs_content));
  let oursPane = $state<HTMLDivElement>();
  let theirsPane = $state<HTMLDivElement>();
  function synchronize(from: HTMLDivElement, to?: HTMLDivElement) {
    if (!to) return;
    const ratio = from.scrollTop / Math.max(1, from.scrollHeight - from.clientHeight);
    const top = ratio * Math.max(0, to.scrollHeight - to.clientHeight);
    if (Math.abs(to.scrollTop - top) > 1) to.scrollTop = top;
    if (Math.abs(to.scrollLeft - from.scrollLeft) > 1) to.scrollLeft = from.scrollLeft;
  }
</script>

<div class="comparison">
  {#each [true, false] as ours (ours)}
    {@const content = ours ? chunk.ours_content : chunk.theirs_content}
    {@const label = ours ? chunk.ours_label : chunk.theirs_label}
    {@const accept = ours ? "AcceptOurs" : "AcceptTheirs"}
    {@const both = ours ? "AcceptBothOursFirst" : "AcceptBothTheirsFirst"}
    <div class="side" class:incoming={!ours}>
      <div class="side-heading"><strong>{ours ? "Current" : "Incoming"} <span>{ours ? "Ours" : "Theirs"}</span></strong><code title={label}>{label || (ours ? "HEAD" : "Incoming")}</code></div>
      {#snippet source()}
        {#if content === "" && !(ours ? chunk.ours_crlf.length : chunk.theirs_crlf.length)}<pre class="empty-side">(No content on this side)</pre>
        {:else if rows}<div class="code-lines">{#each rows as row, index (index)}
          <div class="code-line"><span class="line-number" aria-hidden="true">{index + 1}</span><code>{#each (ours ? row.ours : row.theirs) as part, partIndex (partIndex)}<span class:changed={part.kind !== "Equal"}>{part.text}</span>{/each}{"\n"}</code></div>
        {/each}</div>
        {:else}<pre class="plain-code">{content}</pre>{/if}
      {/snippet}
      {#if ours}<div class="source-scroll" bind:this={oursPane} onscroll={(event) => synchronize(event.currentTarget, theirsPane)} tabindex="0" role="textbox" aria-readonly="true" aria-multiline="true" aria-label="Current conflict source">{@render source()}</div>
      {:else}<div class="source-scroll" bind:this={theirsPane} onscroll={(event) => synchronize(event.currentTarget, oursPane)} tabindex="0" role="textbox" aria-readonly="true" aria-multiline="true" aria-label="Incoming conflict source">{@render source()}</div>{/if}
      {#if !rows}<small class="plain-note">Complete plain text · {content.split("\n").length} lines. Word highlighting is limited to smaller blocks.</small>{/if}
      <div class="side-actions"><button {disabled} aria-pressed={chunk.resolution === accept} onclick={() => choose(accept)}>Accept {ours ? "Ours" : "Theirs"}</button><button {disabled} aria-pressed={chunk.resolution === both} onclick={() => choose(both)}>Both ({ours ? "Ours" : "Theirs"} First)</button></div>
    </div>
  {/each}
</div>

<style>
  .comparison { display: grid; grid-template-columns: minmax(0, 1fr) minmax(0, 1fr); }
  .side {
    --side-color: var(--c-accent);
    min-width: 0; display: flex; flex-direction: column;
  }
  .side.incoming {
    --side-color: 161 112 211;
    border-left: 1px solid rgb(var(--c-border) / .45);
  }
  .side-heading { display: flex; flex-wrap: wrap; justify-content: space-between; gap: 6px; padding: 11px 12px 8px; color: rgb(var(--side-color)); }
  .side-heading strong { font-size: 11px; font-weight: 600; }
  .side-heading strong span { font-size: 10px; opacity: .8; font-weight: 400; margin-left: 4px; }
  .side-heading code { max-width: 100%; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-size: 10px; color: rgb(var(--c-text-muted)); }
  .source-scroll { flex: 1; overflow: auto; min-height: 75px; max-height: 320px; background: rgb(var(--side-color) / .045); user-select: text; }
  .code-lines, .plain-code, .empty-side { min-width: max-content; margin: 0; padding: 10px 0; font: 11px/1.8 ui-monospace, SFMono-Regular, Menlo, monospace; white-space: pre; }
  .plain-code, .empty-side { padding: 10px 12px; }
  .code-line { display: flex; min-height: 1.8em; }
  .line-number { display: inline-block; min-width: 35px; text-align: right; padding-right: 12px; user-select: none; color: rgb(var(--c-text-muted) / .65); }
  .changed { background: rgb(var(--side-color) / .18); border-radius: 2px; }
  .side-actions { display: flex; gap: 5px; flex-wrap: wrap; padding: 9px; }
  button { cursor: pointer; border: 1px solid rgb(var(--c-border) / .7); border-radius: 6px; padding: 5px 7px; color: rgb(var(--c-text-muted)); font-size: 10px; }
  button[aria-pressed="true"] { border-color: rgb(var(--side-color) / .8); background: rgb(var(--side-color) / .15); color: rgb(var(--c-text)); }
  button:disabled { opacity: .4; cursor: default; }
  button:focus-visible, .source-scroll:focus-visible { outline: 2px solid rgb(var(--c-accent)); outline-offset: -2px; }
  .plain-note { padding: 6px 10px; color: rgb(var(--c-text-muted)); font-size: 10px; }
  @media (max-width: 650px) { .comparison { grid-template-columns: 1fr; } .side.incoming { border-left: 0; border-top: 1px solid rgb(var(--c-border) / .45); } }
</style>
