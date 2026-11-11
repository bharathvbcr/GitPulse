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
    <span class="gp-chip" data-kind="removable" data-tone="neutral">
      {label}
      <button type="button" class="gp-icon-btn" data-size="sm" aria-label={`Remove ${label}`} {disabled} onclick={() => remove(index)}>×</button>
    </span>
  {/each}
  <input
    class="draft"
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
  .label-input{display:flex;flex-wrap:wrap;gap:6px;align-items:center;min-height:34px;padding:4px;border:1px solid var(--gp-edge);border-radius:var(--gp-r-md);background:rgb(var(--c-bg) / 0.6)}
  .label-input.disabled{opacity:.5}
  .draft{flex:1;min-width:7rem;border:0;background:transparent;padding:4px;margin:0;color:rgb(var(--c-text));-webkit-text-fill-color:rgb(var(--c-text));font-size:var(--text-xs);outline:none}.draft::placeholder{-webkit-text-fill-color:rgb(var(--c-text-faint))}.label-input:focus-within{border-color:rgb(var(--c-accent) / 0.65);box-shadow:var(--ring-focus)}
</style>
