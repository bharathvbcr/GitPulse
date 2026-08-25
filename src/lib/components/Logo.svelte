<script lang="ts">
  /**
   * The GitPulse mark: a commit lane that spikes into a heartbeat and ends on a
   * branch node.
   *
   * Two variants, because one drawing cannot do both jobs. `badge` is the app
   * icon — the same rounded square that sits in the dock — and is what belongs
   * anywhere the logo stands for the product: the title bar, the welcome
   * screen, an about box. `mark` is the bare stroke, for places that already
   * have a container of their own.
   *
   * The geometry is tuned for the small end rather than the large one. At 20px
   * a hairline branch and a 2px node disappear, so the strokes are heavy
   * relative to the box and the node is oversized; at 96px that reads as
   * confident rather than crude, which is the trade a mark should make.
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
    variant === "badge" ? "translate(4.6 4.3) scale(0.71)" : "translate(1.6 1.3) scale(0.9)"
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
    <linearGradient id="{uid}-branch" x1="19" y1="22" x2="30" y2="8" gradientUnits="userSpaceOnUse">
      <stop offset="0%" stop-color="#6ea8ff" />
      <stop offset="100%" stop-color="#a371f7" />
    </linearGradient>
    {#if variant === "badge"}
      <linearGradient id="{uid}-badge" x1="0" y1="0" x2="32" y2="32" gradientUnits="userSpaceOnUse">
        <stop offset="0%" stop-color="#151d2b" />
        <stop offset="100%" stop-color="#0a0d14" />
      </linearGradient>
    {/if}
  </defs>

  {#if variant === "badge"}
    <rect width="32" height="32" rx="10" fill="url(#{uid}-badge)" />
    <rect
      x="0.5"
      y="0.5"
      width="31"
      height="31"
      rx="9.7"
      fill="none"
      stroke="#ffffff"
      stroke-opacity="0.1"
    />
  {/if}

  <g transform={artTransform}>
    <!-- The branch peeling off the trunk, drawn behind it. -->
    <path
      d="M20.8 20.4C24.6 20.4 24.8 12.8 27.4 10.4"
      stroke="url(#{uid}-branch)"
      stroke-width="3.2"
      stroke-linecap="round"
    />

    <!-- Trunk: lane in, pulse, lane out. -->
    <path
      d="M3.6 20.4H9.2L12.6 9.6L16.4 26.4L19.2 20.4H20.8"
      stroke="url(#{uid}-stroke)"
      stroke-width="3.4"
      stroke-linecap="round"
      stroke-linejoin="round"
      class={animated ? "gitpulse-trace" : ""}
    />

    <!-- The commit the branch ends on. Deliberately large: at 20px this is the
         one element that says "git" rather than "waveform". -->
    <circle cx="28" cy="9.4" r="3.6" fill="url(#{uid}-branch)" />
  </g>
</svg>

<style>
  /* The pulse traces itself once, then rests: a loop in a title bar is a
     distraction, a single trace is a greeting. */
  .gitpulse-trace {
    stroke-dasharray: 64;
    stroke-dashoffset: 64;
    animation: gitpulse-draw 1s cubic-bezier(0.4, 0, 0.2, 1) forwards;
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
