<script lang="ts">
  import type { ViewTab } from "../repos/persist";

  /**
   * Mini schematic of a destination, so a new user can tell the panes apart
   * before opening them. Themed via CSS variables — not a screenshot, which
   * would go stale the next time a pane moved.
   */

  let {
    view,
    section = null,
  }: {
    view: ViewTab;
    section?: string | null;
  } = $props();
</script>

<svg
  viewBox="0 0 256 96"
  class="h-[76px] w-full"
  role="img"
  aria-hidden="true"
>
  <rect width="256" height="96" rx="10" fill="rgb(var(--c-bg))" />
  <rect
    x="0.5"
    y="0.5"
    width="255"
    height="95"
    rx="10"
    fill="none"
    stroke="rgb(var(--c-border))"
    stroke-opacity="0.45"
  />
  <circle cx="14" cy="12" r="2.4" fill="rgb(var(--c-status-rose))" fill-opacity="0.7" />
  <circle cx="22" cy="12" r="2.4" fill="rgb(var(--c-status-amber))" fill-opacity="0.7" />
  <circle cx="30" cy="12" r="2.4" fill="rgb(var(--c-status-emerald))" fill-opacity="0.7" />
  <rect x="40" y="9.5" width="52" height="5" rx="2.5" fill="rgb(var(--c-border))" fill-opacity="0.35" />

  {#if view === "work" && section === "resolve"}
    <rect x="10" y="24" width="114" height="62" rx="6" fill="rgb(var(--c-surface))" />
    <rect x="132" y="24" width="114" height="62" rx="6" fill="rgb(var(--c-surface))" />
    <rect x="16" y="32" width="44" height="4" rx="2" fill="rgb(var(--c-status-rose))" fill-opacity="0.7" />
    <rect x="16" y="42" width="90" height="3.5" rx="1.75" fill="rgb(var(--c-status-rose))" fill-opacity="0.35" />
    <rect x="16" y="50" width="72" height="3.5" rx="1.75" fill="rgb(var(--c-status-rose))" fill-opacity="0.28" />
    <rect x="16" y="58" width="84" height="3.5" rx="1.75" fill="rgb(var(--c-status-rose))" fill-opacity="0.22" />
    <rect x="138" y="32" width="44" height="4" rx="2" fill="rgb(var(--c-status-emerald))" fill-opacity="0.7" />
    <rect x="138" y="42" width="90" height="3.5" rx="1.75" fill="rgb(var(--c-status-emerald))" fill-opacity="0.35" />
    <rect x="138" y="50" width="64" height="3.5" rx="1.75" fill="rgb(var(--c-status-emerald))" fill-opacity="0.28" />
    <rect x="138" y="58" width="80" height="3.5" rx="1.75" fill="rgb(var(--c-status-emerald))" fill-opacity="0.22" />
    <rect x="126" y="24" width="4" height="62" fill="rgb(var(--c-border))" fill-opacity="0.35" />
  {:else if view === "work" && section === "remote"}
    {#each [0, 1, 2] as row (row)}
      <rect x="10" y={24 + row * 22} width="236" height="18" rx="5" fill="rgb(var(--c-surface))" />
      <rect x="18" y={29 + row * 22} width="18" height="8" rx="4" fill="rgb(var(--c-accent))" fill-opacity="0.35" />
      <rect x="42" y={30 + row * 22} width={90 - row * 12} height="6" rx="3" fill="rgb(var(--c-text-muted))" fill-opacity="0.45" />
      <circle cx="228" cy={33 + row * 22} r="3.2" fill={row === 1 ? "rgb(var(--c-status-amber))" : "rgb(var(--c-status-emerald))"} fill-opacity="0.8" />
    {/each}
  {:else if view === "work" && section === "stack"}
    <rect x="28" y="28" width="160" height="20" rx="5" fill="rgb(var(--c-surface))" stroke="rgb(var(--c-accent))" stroke-opacity="0.45" />
    <rect x="44" y="50" width="160" height="20" rx="5" fill="rgb(var(--c-surface))" />
    <rect x="60" y="72" width="160" height="16" rx="5" fill="rgb(var(--c-surface))" fill-opacity="0.7" />
    <rect x="36" y="34" width="70" height="8" rx="4" fill="rgb(var(--c-accent))" fill-opacity="0.35" />
    <rect x="52" y="56" width="56" height="8" rx="4" fill="rgb(var(--c-text-muted))" fill-opacity="0.35" />
    <path d="M196 38h18M214 38l-4-4M214 38l-4 4" fill="none" stroke="rgb(var(--c-accent))" stroke-width="1.4" />
  {:else if view === "work" && section === "policy"}
    {#each [0, 1, 2, 3] as row (row)}
      <rect x="16" y={26 + row * 16} width="224" height="13" rx="4" fill="rgb(var(--c-surface))" />
      <circle
        cx="28"
        cy={32.5 + row * 16}
        r="4"
        fill={row === 2 ? "rgb(var(--c-status-rose))" : "rgb(var(--c-status-emerald))"}
        fill-opacity="0.75"
      />
      <rect x="40" y={29 + row * 16} width={100 - row * 10} height="6" rx="3" fill="rgb(var(--c-text-muted))" fill-opacity="0.4" />
    {/each}
  {:else if view === "work"}
    {#each [0, 1, 2] as row (row)}
      <rect x="10" y={24 + row * 22} width="236" height="18" rx="6" fill="rgb(var(--c-surface))" />
      <rect
        x="10"
        y={24 + row * 22}
        width="4"
        height="18"
        rx="2"
        fill={row === 0 ? "rgb(var(--c-status-rose))" : row === 1 ? "rgb(var(--c-status-amber))" : "rgb(var(--c-status-emerald))"}
        fill-opacity="0.8"
      />
      <rect x="24" y={30 + row * 22} width={88 - row * 8} height="6" rx="3" fill="rgb(var(--c-text))" fill-opacity="0.35" />
      <rect x="188" y={29 + row * 22} width="48" height="8" rx="4" fill="rgb(var(--c-accent))" fill-opacity={row === 0 ? 0.28 : 0.12} />
    {/each}
  {:else if view === "code" && section === "blame"}
    <rect x="10" y="24" width="236" height="62" rx="6" fill="rgb(var(--c-surface))" />
    <rect x="10" y="24" width="34" height="62" rx="6" fill="rgb(var(--c-bg))" />
    {#each [0, 1, 2, 3, 4] as row (row)}
      <rect
        x="16"
        y={30 + row * 10}
        width="22"
        height="6"
        rx="2"
        fill={row < 2 ? "rgb(var(--c-accent))" : row < 4 ? "rgb(var(--c-status-amber))" : "rgb(var(--c-status-sky))"}
        fill-opacity="0.55"
      />
      <rect x="52" y={31 + row * 10} width={140 - row * 14} height="4.5" rx="2" fill="rgb(var(--c-text-muted))" fill-opacity="0.35" />
    {/each}
  {:else if view === "code" && section === "map"}
    <rect x="10" y="24" width="72" height="62" rx="6" fill="rgb(var(--c-surface))" />
    <rect x="90" y="24" width="156" height="62" rx="6" fill="rgb(var(--c-surface))" />
    {#each [0, 1, 2, 3] as row (row)}
      <rect
        x="16"
        y={30 + row * 12}
        width={row === 1 ? 52 : 44}
        height="6"
        rx="2"
        fill={row === 1 ? "rgb(var(--c-accent))" : "rgb(var(--c-text-muted))"}
        fill-opacity={row === 1 ? 0.45 : 0.3}
      />
    {/each}
    <rect x="98" y="32" width="60" height="5" rx="2" fill="rgb(var(--c-text))" fill-opacity="0.4" />
    <rect x="98" y="44" width="120" height="4" rx="2" fill="rgb(var(--c-text-muted))" fill-opacity="0.3" />
    <rect x="98" y="54" width="96" height="4" rx="2" fill="rgb(var(--c-text-muted))" fill-opacity="0.28" />
    <rect x="98" y="64" width="108" height="4" rx="2" fill="rgb(var(--c-accent))" fill-opacity="0.25" />
  {:else if view === "code"}
    <rect x="10" y="24" width="78" height="62" rx="6" fill="rgb(var(--c-surface))" />
    <rect x="94" y="24" width="152" height="62" rx="6" fill="rgb(var(--c-surface))" />
    <rect x="18" y="32" width="40" height="5" rx="2.5" fill="rgb(var(--c-text-muted))" fill-opacity="0.4" />
    <rect x="22" y="44" width="52" height="4" rx="2" fill="rgb(var(--c-accent))" fill-opacity="0.45" />
    <rect x="22" y="54" width="44" height="4" rx="2" fill="rgb(var(--c-text-muted))" fill-opacity="0.3" />
    <rect x="22" y="64" width="48" height="4" rx="2" fill="rgb(var(--c-text-muted))" fill-opacity="0.3" />
    <rect x="102" y="32" width="46" height="10" rx="3" fill="rgb(var(--c-accent))" fill-opacity="0.2" />
    <rect x="102" y="50" width="120" height="4" rx="2" fill="rgb(var(--c-text-muted))" fill-opacity="0.35" />
    <rect x="102" y="60" width="96" height="4" rx="2" fill="rgb(var(--c-text-muted))" fill-opacity="0.28" />
    <rect x="102" y="70" width="108" height="4" rx="2" fill="rgb(var(--c-text-muted))" fill-opacity="0.22" />
  {:else if view === "history" && section === "diff"}
    <rect x="10" y="24" width="236" height="62" rx="6" fill="rgb(var(--c-surface))" />
    <rect x="10" y="36" width="236" height="12" fill="rgb(var(--c-status-rose))" fill-opacity="0.18" />
    <rect x="10" y="48" width="236" height="12" fill="rgb(var(--c-status-emerald))" fill-opacity="0.18" />
    <rect x="10" y="60" width="236" height="12" fill="rgb(var(--c-status-emerald))" fill-opacity="0.12" />
    <rect x="20" y="28" width="72" height="4" rx="2" fill="rgb(var(--c-text-muted))" fill-opacity="0.35" />
    <rect x="20" y="40" width="88" height="4" rx="2" fill="rgb(var(--c-status-rose))" fill-opacity="0.7" />
    <rect x="20" y="52" width="104" height="4" rx="2" fill="rgb(var(--c-status-emerald))" fill-opacity="0.7" />
    <rect x="20" y="64" width="80" height="4" rx="2" fill="rgb(var(--c-status-emerald))" fill-opacity="0.55" />
  {:else if view === "history" && section === "reflog"}
    {#each [0, 1, 2, 3] as row (row)}
      <rect x="16" y={26 + row * 16} width="224" height="13" rx="4" fill="rgb(var(--c-surface))" />
      <rect x="24" y={30 + row * 16} width="28" height="5" rx="2.5" fill="rgb(var(--c-text-muted))" fill-opacity="0.4" />
      <rect x="58" y={30 + row * 16} width={90 - row * 8} height="5" rx="2.5" fill="rgb(var(--c-text))" fill-opacity="0.3" />
      {#if row === 0}
        <rect x="200" y={28 + row * 16} width="32" height="9" rx="4.5" fill="rgb(var(--c-accent))" fill-opacity="0.3" />
      {/if}
    {/each}
  {:else if view === "history"}
    <rect x="10" y="24" width="236" height="62" rx="6" fill="rgb(var(--c-surface))" />
    <path
      d="M36 78 V42 C36 34 48 34 48 42 V78"
      fill="none"
      stroke="rgb(var(--c-accent))"
      stroke-width="2"
      stroke-linecap="round"
    />
    <path
      d="M48 50 C64 50 64 36 80 36 V78"
      fill="none"
      stroke="rgb(var(--c-status-amber))"
      stroke-width="2"
      stroke-linecap="round"
    />
    <circle cx="36" cy="78" r="3.2" fill="rgb(var(--c-accent))" />
    <circle cx="36" cy="58" r="3.2" fill="rgb(var(--c-accent))" />
    <circle cx="48" cy="42" r="3.2" fill="rgb(var(--c-accent))" />
    <circle cx="80" cy="36" r="3.2" fill="rgb(var(--c-status-amber))" />
    <circle cx="80" cy="58" r="3.2" fill="rgb(var(--c-status-amber))" />
    <rect x="102" y="32" width="110" height="6" rx="3" fill="rgb(var(--c-text))" fill-opacity="0.35" />
    <rect x="102" y="48" width="88" height="5" rx="2.5" fill="rgb(var(--c-text-muted))" fill-opacity="0.3" />
    <rect x="102" y="62" width="96" height="5" rx="2.5" fill="rgb(var(--c-text-muted))" fill-opacity="0.25" />
  {:else if view === "insights" && section === "coverage"}
    {#each [0, 1, 2, 3] as row (row)}
      <rect x="16" y={26 + row * 16} width="224" height="13" rx="4" fill="rgb(var(--c-surface))" />
      <rect x="24" y={30 + row * 16} width="52" height="5" rx="2.5" fill="rgb(var(--c-text-muted))" fill-opacity="0.4" />
      <rect x="120" y={31 + row * 16} width="110" height="5" rx="2.5" fill="rgb(var(--c-border))" fill-opacity="0.35" />
      <rect
        x="120"
        y={31 + row * 16}
        width={110 * (0.85 - row * 0.14)}
        height="5"
        rx="2.5"
        fill={row === 3 ? "rgb(var(--c-status-rose))" : "rgb(var(--c-status-emerald))"}
        fill-opacity="0.7"
      />
    {/each}
  {:else if view === "insights" && section === "health"}
    <rect x="10" y="24" width="236" height="62" rx="6" fill="rgb(var(--c-surface))" />
    <path
      d="M40 36 l12 -6 12 6 v14 c0 10 -8 16 -12 18 -4 -2 -12 -8 -12 -18 z"
      fill="rgb(var(--c-accent))"
      fill-opacity="0.25"
      stroke="rgb(var(--c-accent))"
      stroke-width="1.2"
    />
    {#each [0, 1, 2] as row (row)}
      <rect x="78" y={32 + row * 16} width="150" height="12" rx="4" fill="rgb(var(--c-bg))" />
      <circle
        cx="90"
        cy={38 + row * 16}
        r="3"
        fill={row === 0 ? "rgb(var(--c-status-rose))" : row === 1 ? "rgb(var(--c-status-amber))" : "rgb(var(--c-status-emerald))"}
      />
      <rect x="100" y={35 + row * 16} width={70 - row * 8} height="5" rx="2.5" fill="rgb(var(--c-text-muted))" fill-opacity="0.4" />
    {/each}
  {:else if view === "insights" && section === "storage"}
    <rect x="16" y="28" width="224" height="14" rx="7" fill="rgb(var(--c-border))" fill-opacity="0.25" />
    <rect x="16" y="28" width="96" height="14" rx="7" fill="rgb(var(--c-accent))" fill-opacity="0.55" />
    <rect x="112" y="28" width="52" height="14" fill="rgb(var(--c-status-amber))" fill-opacity="0.55" />
    {#each [0, 1, 2] as row (row)}
      <rect x="16" y={50 + row * 14} width="224" height="11" rx="4" fill="rgb(var(--c-surface))" />
      <rect x="24" y={53 + row * 14} width={80 - row * 10} height="5" rx="2.5" fill="rgb(var(--c-text-muted))" fill-opacity="0.4" />
      <rect x="200" y={53 + row * 14} width="28" height="5" rx="2.5" fill="rgb(var(--c-text))" fill-opacity="0.3" />
    {/each}
  {:else}
    <rect x="12" y="26" width="56" height="28" rx="6" fill="rgb(var(--c-surface))" />
    <rect x="76" y="26" width="56" height="28" rx="6" fill="rgb(var(--c-surface))" />
    <rect x="140" y="26" width="56" height="28" rx="6" fill="rgb(var(--c-surface))" />
    <rect x="24" y="34" width="32" height="6" rx="3" fill="rgb(var(--c-accent))" fill-opacity="0.45" />
    <rect x="88" y="34" width="32" height="6" rx="3" fill="rgb(var(--c-text-muted))" fill-opacity="0.4" />
    <rect x="152" y="34" width="32" height="6" rx="3" fill="rgb(var(--c-text-muted))" fill-opacity="0.4" />
    {#each [0, 1, 2, 3, 4, 5, 6] as col (col)}
      {#each [0, 1, 2] as row (`${col}-${row}`)}
        <rect
          x={16 + col * 14}
          y={62 + row * 8}
          width="11"
          height="6"
          rx="1.5"
          fill="rgb(var(--c-accent))"
          fill-opacity={0.15 + ((col + row) % 4) * 0.18}
        />
      {/each}
    {/each}
  {/if}
</svg>
