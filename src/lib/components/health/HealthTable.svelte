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

<!-- `overflow-wrap: anywhere` belongs on the body, not the table. Package
     names, file paths, rule ids and symbol names are identifiers with no
     break opportunity, and a column's min-content width is its longest
     unbreakable word: a 300-character dependency name widened the page.
     `break-word` does not shrink that minimum; `anywhere` does. It inherits,
     so it reaches the cells the caller renders.
     It must not inherit into the header. A header is one word ("Severity",
     "Package"), and `anywhere` lets that word's minimum fall to a single
     character, so a pane narrower than the harness stage splits it —
     SEVER/ITY, PACKA/GE — while the column shrinks to fit. `nowrap` keeps
     each header on one line. It does not size the column; `table-fixed`
     does, so a long cell cannot shrink its neighbours down to that word. -->
<div class="min-w-0 border border-border/70 rounded-2xl overflow-hidden shadow-card">
  <!-- `fixed` so a column's width comes from the table and the header widths,
       not from the longest cell. Without it, one unbreakable package name's
       max-content takes the spare width and the fix column collapses until
       "1.1.0" wraps under a header that still says FIX. An unsized column
       then takes an equal share, so callers set `width` on the short ones
       (severity, fix, the action) and the name columns receive what remains.
       `align-top` is on the body: one wrapping cell used to center the
       severity badge and the version in the middle of that row. -->
  <table class="w-full table-fixed text-left">
    <caption class="sr-only">{caption}</caption>
    <thead class="bg-surface text-[10px] uppercase text-textMuted">
      <tr>
        {#each keyedList(columns, (column) => column.label) as { item: column, key } (key)}
          <th scope="col" class="whitespace-nowrap px-3 py-2 font-medium {column.width ?? ''}">
            {#if column.hideLabel}
              <span class="sr-only">{column.label}</span>
            {:else}
              {column.label}
            {/if}
          </th>
        {/each}
      </tr>
    </thead>
    <tbody class="align-top [overflow-wrap:anywhere]">
      {@render children()}
    </tbody>
  </table>
</div>
