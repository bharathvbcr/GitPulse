<script lang="ts">
  /**
   * Chip editor for task labels. Comma paste and Enter commit a chip;
   * Backspace on an empty field removes the last chip.
   */
  let {
    value = $bindable<string[]>([]),
    disabled = false,
    placeholder = "Add a label",
    maxlength = 64,
  }: {
    value?: string[];
    disabled?: boolean;
    placeholder?: string;
    maxlength?: number;
  } = $props();

  let draft = $state("");

  function commit(raw: string) {
    const next = raw.split(",").map((part) => part.trim()).filter(Boolean);
    if (!next.length) return;
    const merged = [...value];
    for (const label of next) {
      const clipped = label.slice(0, maxlength);
      if (clipped && !merged.includes(clipped)) merged.push(clipped);
    }
    value = merged;
    draft = "";
  }

  function remove(index: number) {
    value = value.filter((_, i) => i !== index);
  }

  function onKeydown(event: KeyboardEvent) {
    if (event.key === "Enter" || event.key === ",") {
      event.preventDefault();
      commit(draft);
      return;
    }
    if (event.key === "Backspace" && !draft && value.length) {
      event.preventDefault();
      value = value.slice(0, -1);
    }
  }
</script>

<div class="label-input" class:disabled aria-label="Labels">
  {#each value as label, index (label + index)}
    <span class="chip">
      {label}
      <button type="button" class="remove" aria-label={`Remove ${label}`} {disabled} onclick={() => remove(index)}>×</button>
    </span>
  {/each}
  <input
    class="gp-field draft"
    value={draft}
    {disabled}
    {placeholder}
    maxlength={maxlength}
    oninput={(event) => { draft = event.currentTarget.value; }}
    onkeydown={onKeydown}
    onblur={() => commit(draft)}
    onpaste={(event) => {
      const text = event.clipboardData?.getData("text") ?? "";
      if (text.includes(",")) {
        event.preventDefault();
        commit(`${draft}${text}`);
      }
    }}
  />
</div>

<style>
  .label-input{display:flex;flex-wrap:wrap;gap:6px;align-items:center;min-height:34px;padding:4px;border:1px solid rgb(var(--c-border));border-radius:7px;background:rgb(var(--c-bg) / 0.6)}
  .label-input.disabled{opacity:.5}
  .chip{display:inline-flex;align-items:center;gap:4px;padding:2px 6px;border-radius:999px;background:rgb(var(--c-surface-hover) / 0.85);font-size:11px}
  .remove{border:0;background:transparent;color:inherit;cursor:pointer;padding:0 2px;line-height:1}
  .draft{flex:1;min-width:7rem;border:0 !important;background:transparent !important;padding:4px !important;margin:0}
</style>
