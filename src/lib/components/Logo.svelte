<script lang="ts">
  import {
    brandLogoPaletteSpec,
    type BrandLogoPalette,
    type BrandLogoStyle,
  } from "../ui/appearance/brandLogo";
  import { brandLogoStore } from "../ui/appearance/brandLogoStore";

  /**
   * The Waylode mark: defaults to Concept 2 (Way-Arc) in the Cobalt palette,
   * and supports runtime-switchable mark styles and palettes from Settings -> Appearance.
   *
   * Two variants, because one drawing cannot do both jobs. `badge` is the app
   * icon — the same rounded square that sits in the dock — and is what belongs
   * anywhere the logo stands for the product: the title bar, the welcome
   * screen, an about box. `mark` is the bare stroke, for places that already
   * have a container of their own.
   *
   * The same default drawing is written out in index.html (the boot screen) and
   * in public/waylode-mark.svg and public/waylode-icon.svg, none of which can
   * import this component. Logo.contract.test.ts holds the four together.
   */
  let {
    size = 28,
    variant = "mark",
    animated = false,
    title = "Waylode",
    logoStyle,
    logoPalette,
  }: {
    size?: number;
    variant?: "mark" | "badge";
    animated?: boolean;
    title?: string;
    logoStyle?: BrandLogoStyle;
    logoPalette?: BrandLogoPalette;
  } = $props();

  // Gradient ids must be unique per instance: two logos on a page would
  // otherwise share one definition, and the first to unmount would take the
  // other's paint with it.
  const uid = `wl-${Math.random().toString(36).slice(2, 9)}`;

  const activeStyle = $derived(logoStyle ?? $brandLogoStore.logoStyle);
  const activePaletteId = $derived(logoPalette ?? $brandLogoStore.logoPalette);
  const palette = $derived(brandLogoPaletteSpec(activePaletteId));

  // The badge insets the artwork to leave the icon's rounded corners clear.
  // Derived, not computed once: a `const` here would freeze the transform at
  // whatever variant the component first mounted with.
  const artTransform = $derived(
    variant === "badge" ? "translate(3.04 3.04) scale(0.81)" : "translate(-2.4 -2.4) scale(1.15)"
  );
</script>

<svg
  width={size}
  height={size}
  viewBox="0 0 32 32"
  fill="none"
  xmlns="http://www.w3.org/2000/svg"
  role="img"
  aria-label={title}
  class="shrink-0"
>
  <defs>
    <!-- waylode-mark-defs:start -->
    {#if activePaletteId === "cobalt"}
      <linearGradient id="{uid}-stroke" x1="3" y1="27" x2="29" y2="6" gradientUnits="userSpaceOnUse">
        <stop offset="0%" stop-color="#7dd3fc" />
        <stop offset="52%" stop-color="#38bdf8" />
        <stop offset="100%" stop-color="#3b82f6" />
      </linearGradient>
    {:else}
      <linearGradient id="{uid}-stroke" x1="3" y1="27" x2="29" y2="6" gradientUnits="userSpaceOnUse">
        <stop offset="0%" stop-color={palette.lit[0]} />
        <stop offset="52%" stop-color={palette.lit[1]} />
        <stop offset="100%" stop-color={palette.lit[2]} />
      </linearGradient>
    {/if}
    {#if activeStyle !== "way-arc"}
      <linearGradient id="{uid}-shade" x1="60" y1="60" x2="260" y2="260" gradientUnits="userSpaceOnUse">
        <stop offset="0%" stop-color={palette.shade[0]} />
        <stop offset="100%" stop-color={palette.shade[1]} />
      </linearGradient>
    {/if}
    {#if variant === "badge"}
      {#if activePaletteId === "cobalt"}
        <linearGradient id="{uid}-badge" x1="0" y1="0" x2="32" y2="32" gradientUnits="userSpaceOnUse">
          <stop offset="0%" stop-color="#0f172a" />
          <stop offset="100%" stop-color="#060a14" />
        </linearGradient>
        <radialGradient id="{uid}-glow" cx="16" cy="16" r="15" gradientUnits="userSpaceOnUse">
          <stop offset="0%" stop-color="#38bdf8" stop-opacity="0.24" />
          <stop offset="100%" stop-color="#38bdf8" stop-opacity="0" />
        </radialGradient>
      {:else}
        <linearGradient id="{uid}-badge" x1="0" y1="0" x2="32" y2="32" gradientUnits="userSpaceOnUse">
          <stop offset="0%" stop-color={palette.plate[0]} />
          <stop offset="100%" stop-color={palette.plate[1]} />
        </linearGradient>
        <radialGradient id="{uid}-glow" cx="16" cy="16" r="15" gradientUnits="userSpaceOnUse">
          <stop offset="0%" stop-color={palette.glow} stop-opacity="0.24" />
          <stop offset="100%" stop-color={palette.glow} stop-opacity="0" />
        </radialGradient>
      {/if}
    {/if}
    <!-- waylode-mark-defs:end -->
  </defs>

  {#if variant === "badge"}
    <rect width="32" height="32" rx="10" fill="url(#{uid}-badge)" />
    <rect width="32" height="32" rx="10" fill="url(#{uid}-glow)" />
    <rect
      x="0.3"
      y="0.3"
      width="31.4"
      height="31.4"
      rx="9.7"
      fill="none"
      stroke="#ffffff"
      stroke-opacity="0.12"
      stroke-width="0.6"
    />
  {/if}

  <g transform={artTransform}>
    {#if activeStyle === "way-arc"}
      <!-- waylode-mark-geom:start -->
      <path
        d="M23.8 9.0A10.5 10.5 0 1 0 26.5 16.0H21.0L16.2 11.6L11.4 18.8"
        pathLength="100"
        stroke="url(#{uid}-stroke)"
        stroke-width="3.4"
        stroke-linecap="round"
        stroke-linejoin="round"
        class={animated ? "wl-trace" : ""}
      />
      <circle cx="23.8" cy="9.0" r="2.8" fill="url(#{uid}-stroke)" />
      <!-- waylode-mark-geom:end -->
    {:else if activeStyle === "folded-ribbon"}
      <g transform="scale(0.1)">
        <g transform="translate(0 7)" opacity="0.72">
          <path
            d="M58 97L100 223L136 223L160 151L184 223L220 223L262 97L226 97L202 169L178 97L142 97L118 169L94 97Z"
            fill={palette.deep}
          />
        </g>
        <path
          d="M58 97L100 223L136 223L160 151L184 223L220 223L262 97L226 97L202 169L178 97L142 97L118 169L94 97Z"
          fill="url(#{uid}-shade)"
        />
        <path
          d="M100 223L136 223L178 97L142 97ZM184 223L220 223L262 97L226 97Z"
          fill="url(#{uid}-stroke)"
        />
      </g>
    {:else}
      <g transform="translate(0 0.9)" opacity="0.72">
        <g transform="scale(0.1)" fill={palette.deep}>
          <path d="M150 56L54 152L90 188L150 128Z" />
          <path d="M170 264L266 168L230 132L170 192Z" />
          <path d="M108 206L150 248L150 172Z" />
          <path d="M212 114L170 72L170 148Z" />
          <path d="M160 136L184 160L160 184L136 160Z" />
        </g>
      </g>
      <g transform="scale(0.1)">
        <path d="M150 56L54 152L90 188L150 128Z" fill="url(#{uid}-stroke)" />
        <path d="M170 264L266 168L230 132L170 192Z" fill="url(#{uid}-shade)" />
        <path d="M108 206L150 248L150 172Z" fill="url(#{uid}-shade)" />
        <path d="M212 114L170 72L170 148Z" fill="url(#{uid}-stroke)" />
        <path d="M160 136L184 160L160 184L136 160Z" fill={palette.lit[0]} />
        <path d="M150 56L150 128L90 188" stroke="white" stroke-opacity="0.28" stroke-width="5" stroke-linejoin="round" />
        <path d="M160 136L184 160L160 184" stroke="white" stroke-opacity="0.32" stroke-width="4" stroke-linejoin="round" />
      </g>
    {/if}
  </g>
</svg>

<style>
  /* The mark traces itself once, then rests: a loop in a title bar is a
     distraction, a single trace is a greeting. */
  .wl-trace {
    stroke-dasharray: 100;
    stroke-dashoffset: 100;
    animation: wl-draw calc(var(--gp-dur-emphasis) * 2) var(--gp-ease-effect) forwards;
  }

  @keyframes wl-draw {
    to {
      stroke-dashoffset: 0;
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .wl-trace {
      animation: none;
      stroke-dashoffset: 0;
    }
  }
</style>
