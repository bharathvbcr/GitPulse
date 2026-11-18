<script lang="ts" module>
  import { isCaseInsensitiveFs } from "../repos/paths";

  const pathOptions = { caseInsensitive: isCaseInsensitiveFs() };
</script>

<script lang="ts">
  import { logoInitials, monogramColor } from "../repos/repoLogo";
  import { logoSubject, repoLogos, tabForLogo, type LogoTab } from "../repos/repoLogos";
  import { lookupGroupColor, normalizeTabColor, TAB_COLOR_INK, type TabColor } from "../repos/tabColors";
  import { normalizeGroupName } from "../repos/tabGroups";
  import { repoStore } from "../stores/repoStore";

  /**
   * One repository's mark: its logo when the repository has one, else its
   * initials on a tile of its tab color. The initials are the base layer and
   * the logo is drawn over them, so a repository is never left without a
   * mark while its logo loads, or when its file does not decode.
   *
   * Decorative: every call site names the repository in text beside it.
   */
  interface Props {
    /** A checkout of the repository. */
    path: string;
    /** Its open tab, when the caller has it; otherwise looked up by path. */
    tab?: LogoTab | null;
    /** What to call it, for the initials; defaults to the repository's own name. */
    name?: string;
    /** The tile color; defaults to the tab's own color, else its group's, as the strip paints it. */
    color?: TabColor | null;
    /** Edge length in CSS pixels. */
    size?: number;
    /** Look this logo up ahead of the others (the tab in front). */
    priority?: boolean;
    /** Draw a second tile behind: several checkouts folded into one. */
    stacked?: boolean;
    /** A corner badge for a pinned tab, which the pin glyph used to mark. */
    pinned?: boolean;
    /** The repository could not be read: a danger ring, as the folder icon's tint was. */
    error?: boolean;
    /** Dimmed, for a parked tab. */
    muted?: boolean;
    class?: string;
  }
  let {
    path,
    tab = undefined,
    name = undefined,
    color = undefined,
    size = 16,
    priority = false,
    stacked = false,
    pinned = false,
    error = false,
    muted = false,
    class: className = "",
  }: Props = $props();

  const resolvedTab = $derived(tab === undefined ? tabForLogo(path, $repoStore.openTabs, pathOptions) : tab);
  const subject = $derived(logoSubject(path, resolvedTab, pathOptions, name));
  const effectiveColor = $derived.by((): TabColor | null => {
    if (color !== undefined) return color;
    const own = normalizeTabColor(resolvedTab?.color ?? null);
    if (own) return own;
    return lookupGroupColor($repoStore.groupColors, normalizeGroupName(resolvedTab?.group ?? null));
  });
  const tile = $derived(monogramColor(subject.name, effectiveColor));
  const initials = $derived(logoInitials(subject.name));
  const logo = $derived($repoLogos[subject.target.key]);
  const url = $derived(logo?.status === "ready" ? logo.url : null);
  let failedUrl = $state<string | null>(null);
  const showImage = $derived(url !== null && failedUrl !== url);

  // Off-screen marks (a strip scrolled past, two hundred restored tabs) ask
  // for nothing until they are seen; the tab in front asks at once.
  let seen = $state(false);

  function whenSeen(node: HTMLElement) {
    if (typeof IntersectionObserver === "undefined") {
      seen = true;
      return {};
    }
    const observer = new IntersectionObserver((entries) => {
      if (entries.some((entry) => entry.isIntersecting)) {
        seen = true;
        observer.disconnect();
      }
    });
    observer.observe(node);
    return { destroy: () => observer.disconnect() };
  }

  $effect(() => {
    if ((!seen && !priority) || !subject.settled) return;
    repoLogos.request(subject.target, { priority });
  });

  const radius = $derived(Math.max(3, Math.round(size * 0.26)));
  const fontSize = $derived(Math.max(7, Math.round(size * (initials.length > 1 ? 0.46 : 0.58))));
</script>

<span
  use:whenSeen
  class="repo-logo relative inline-grid shrink-0 place-items-center {muted ? 'opacity-60' : ''} {className}"
  style:width="{size}px"
  style:height="{size}px"
  aria-hidden="true"
  data-repo-logo
  data-logo-state={showImage ? "image" : "initials"}
  data-logo-key={subject.target.key}
>
  {#if stacked}
    <span
      class="repo-logo-behind absolute"
      style:inset="0"
      style:border-radius="{radius}px"
      style:transform="translate({Math.max(2, Math.round(size * 0.16))}px, {-Math.max(2, Math.round(size * 0.16))}px)"
      style:background-color={TAB_COLOR_INK[tile]}
    ></span>
  {/if}
  <span
    class="repo-logo-tile relative grid h-full w-full place-items-center overflow-hidden font-semibold leading-none text-white select-none {error ? 'ring-1 ring-danger' : ''}"
    style:border-radius="{radius}px"
    style:background-color={showImage ? "transparent" : TAB_COLOR_INK[tile]}
    style:font-size="{fontSize}px"
    data-logo-initials={initials}
    data-has-image={showImage ? "true" : undefined}
  >
    {initials}
    {#if showImage && url}
      <img
        class="repo-logo-img absolute inset-0 h-full w-full object-contain"
        src={url}
        alt=""
        decoding="async"
        draggable="false"
        onerror={() => {
          failedUrl = url;
          repoLogos.reportBroken(subject.target, url);
        }}
      />
    {/if}
  </span>
  {#if pinned}
    <span class="repo-logo-pin absolute grid place-items-center rounded-full bg-accent" data-logo-pinned>
      <svg viewBox="0 0 10 10" width="6" height="6" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round">
        <path d="M5 6.6V9.2M3 1h4M3.6 1.1l-.5 3.1L1.8 5.6h6.4L6.9 4.2l-.5-3.1" />
      </svg>
    </span>
  {/if}
</span>

<style>
  .repo-logo-behind {
    opacity: 0.35;
  }
  .repo-logo-tile {
    letter-spacing: -0.02em;
    box-shadow: inset 0 0 0 0.5px rgb(var(--c-border) / 0.65);
  }
  /* A drawn logo replaces the tile: the tile's color would show through a
     transparent logo and fight it. The initials stay underneath until the
     image paints, then the image covers them. */
  .repo-logo-tile[data-has-image] {
    box-shadow: none;
    color: transparent;
  }
  /* Dark marks on transparent backgrounds vanish on a dark surface; a hairline
     light halo keeps their outline without putting a white plate in every tab. */
  :global(html.dark) .repo-logo-img {
    filter: drop-shadow(0 0 0.6px rgb(var(--c-text) / 0.7));
  }
  .repo-logo-pin {
    right: -3px;
    bottom: -3px;
    width: 9px;
    height: 9px;
    color: rgb(var(--c-bg));
    box-shadow: 0 0 0 1.5px rgb(var(--c-surface));
  }
</style>
