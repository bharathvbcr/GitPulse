<script lang="ts">
  import { keyedList } from "../../ui/eachKeys";

  /**
   * The Health view's table shell.
   *
   * Five tables — Dependabot alerts, code scanning alerts, vulnerabilities,
   * outdated packages, dead-code candidates — each hand-rolled the same
   * border, radius, overflow clip, shadow and `<thead>`. Five copies of one
   * decision drift: they already disagreed about width, and none of them had
   * an accessible name, so a screen reader announced five unlabelled tables
   * in a row.
   *
   * `caption` is required rather than optional. An optional accessible name
   * is one a later table will be written without.
   */
  let {
    /**
     * The table's accessible name. Visually hidden — the section heading
     * above it is the visible label — but announced, so the five tables are
     * distinguishable without sight of the headings.
     */
    caption,
    columns,
    children,
  }: {
    caption: string;
    /**
     * Header cells, in order. `width` is an optional Tailwind class for the
     * narrow action column that would otherwise take an equal share.
     *
     * `hideLabel` keeps the label out of the visible header but still in the
     * accessibility tree. The action column used to be an empty `<th>`, which
     * announces a column with no name — the row's link button then had no
     * column context at all.
     */
    columns: readonly { label: string; width?: string; hideLabel?: boolean }[];
    children: import("svelte").Snippet;
  } = $props();
</script>

<!-- `overflow-wrap: anywhere`, not `break-words`, and set here rather than per
     cell. Package names, file paths, rule ids and symbol names are all
     identifiers with no break opportunity, and a table column's width is
     driven by its longest unbreakable word: a 300-character dependency name
     widened the whole page and gave the scroll container a horizontal bar.
     `break-word` does not shrink min-content width, so it does not fix that;
     `anywhere` does. It inherits, so it reaches cells the caller renders. -->
<div class="border border-border/70 rounded-2xl overflow-hidden shadow-card">
  <table class="w-full text-left [overflow-wrap:anywhere]">
    <caption class="sr-only">{caption}</caption>
    <thead class="bg-surface text-[10px] uppercase text-textMuted">
      <tr>
        {#each keyedList(columns, (column) => column.label) as { item: column, key } (key)}
          <th scope="col" class="px-3 py-2 font-medium {column.width ?? ''}">
            {#if column.hideLabel}
              <span class="sr-only">{column.label}</span>
            {:else}
              {column.label}
            {/if}
          </th>
        {/each}
      </tr>
    </thead>
    <tbody>
      {@render children()}
    </tbody>
  </table>
</div>
