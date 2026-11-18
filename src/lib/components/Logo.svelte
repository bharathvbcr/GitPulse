<script lang="ts">
  /**
   * The GitPulse mark: a commit ring that opens into a G, whose crossbar is a
   * heartbeat and whose tip is a branch node.
   *
   * Two variants, because one drawing cannot do both jobs. `badge` is the app
   * icon — the same rounded square that sits in the dock — and is what belongs
   * anywhere the logo stands for the product: the title bar, the welcome
   * screen, an about box. `mark` is the bare stroke, for places that already
   * have a container of their own.
   *
   * The geometry is tuned for the small end rather than the large one. At 20px
   * a hairline ring and a 2px node disappear, so the stroke is heavy relative
   * to the box and the pulse has only one beat; at 96px that reads as
   * confident rather than crude, which is the trade a mark should make.
   *
   * The same drawing is written out in index.html (the boot screen) and in
   * public/gitpulse-mark.svg and public/gitpulse-icon.svg, none of which can
   * import this component. Logo.contract.test.ts holds the four together.
   */
  let {
    size = 28,
    variant = "mark",
    animated = false,
    title = "GitPulse",
  }: {
    size?: number;
    variant?: "mark" | "badge";
    animated?: boolean;
    title?: string;
  } = $props();

  // Gradient ids must be unique per instance: two logos on a page would
  // otherwise share one definition, and the first to unmount would take the
  // other's paint with it.
  const uid = `gp-${Math.random().toString(36).slice(2, 9)}`;

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
    <linearGradient id="{uid}-stroke" x1="3" y1="27" x2="29" y2="6" gradientUnits="userSpaceOnUse">
      <stop offset="0%" stop-color="#22d3ee" />
      <stop offset="52%" stop-color="#58a6ff" />
      <stop offset="100%" stop-color="#a371f7" />
    </linearGradient>
    {#if variant === "badge"}
      <linearGradient id="{uid}-badge" x1="0" y1="0" x2="32" y2="32" gradientUnits="userSpaceOnUse">
        <stop offset="0%" stop-color="#151d2b" />
        <stop offset="100%" stop-color="#0a0d14" />
      </linearGradient>
      <!-- A soft light behind the mark, so the badge has a centre rather than
           being a flat plate. A gradient, not a filter: it costs no extra
           paint when a list draws several. -->
      <radialGradient id="{uid}-glow" cx="16" cy="16" r="15" gradientUnits="userSpaceOnUse">
        <stop offset="0%" stop-color="#58a6ff" stop-opacity="0.24" />
        <stop offset="100%" stop-color="#58a6ff" stop-opacity="0" />
      </radialGradient>
    {/if}
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
    <!-- One stroke: the ring from its tip round to three o'clock, then the
         crossbar inward as a single beat. `pathLength` makes the trace's dash
         a hundred units whatever the geometry is, so redrawing the mark cannot
         leave the animation measuring the old one. -->
    <path
      d="M24.04 9.25A10.5 10.5 0 1 0 26.5 16H22.6L20.2 21L16.8 9.9L14.4 16H11.8"
      pathLength="100"
      stroke="url(#{uid}-stroke)"
      stroke-width="3.4"
      stroke-linecap="round"
      stroke-linejoin="round"
      class={animated ? "gitpulse-trace" : ""}
    />

    <!-- The commit the ring's tip ends on. Larger than the stroke on purpose:
         at 20px this is the one element that says "git" rather than "letter". -->
    <circle cx="24.04" cy="9.25" r="2.9" fill="url(#{uid}-stroke)" />
  </g>
</svg>

<style>
  /* The mark traces itself once, then rests: a loop in a title bar is a
     distraction, a single trace is a greeting. */
  .gitpulse-trace {
    stroke-dasharray: 100;
    stroke-dashoffset: 100;
    animation: gitpulse-draw calc(var(--gp-dur-emphasis) * 2) var(--gp-ease-effect) forwards;
  }

  @keyframes gitpulse-draw {
    to {
      stroke-dashoffset: 0;
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .gitpulse-trace {
      animation: none;
      stroke-dashoffset: 0;
    }
  }
</style>
