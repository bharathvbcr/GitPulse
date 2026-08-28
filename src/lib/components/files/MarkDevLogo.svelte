<script lang="ts">
  let { size = 16, class: className = "" }: { size?: number; class?: string } = $props();
</script>

<svg
  width={size}
  height={size}
  viewBox="0 0 100 100"
  fill="none"
  xmlns="http://www.w3.org/2000/svg"
  class="shrink-0 {className}"
  aria-hidden="true"
>
  <defs>
    <!-- Plate Gradient -->
    <linearGradient id="mdPlateGrad" x1="50" y1="10" x2="50" y2="90" gradientUnits="userSpaceOnUse">
      <stop offset="0%" stop-color="#39406e" />
      <stop offset="100%" stop-color="#171a33" />
    </linearGradient>

    <!-- Chevron Lit Face Gradient -->
    <linearGradient id="mdFaceGrad" x1="20" y1="35" x2="80" y2="70" gradientUnits="userSpaceOnUse">
      <stop offset="0%" stop-color="#6febd6" />
      <stop offset="100%" stop-color="#27b5c9" />
    </linearGradient>

    <!-- Side Depth Gradient -->
    <linearGradient id="mdSideGrad" x1="0%" y1="0%" x2="100%" y2="100%">
      <stop offset="0%" stop-color="#12798f" />
      <stop offset="100%" stop-color="#083c4d" />
    </linearGradient>

    <!-- Rim Light Filter -->
    <radialGradient id="mdGlow" cx="30" cy="25" r="70" gradientUnits="userSpaceOnUse">
      <stop offset="0%" stop-color="#ffffff" stop-opacity="0.25" />
      <stop offset="100%" stop-color="#ffffff" stop-opacity="0" />
    </radialGradient>
  </defs>

  <!-- Squircle Base Plate -->
  <rect
    x="10"
    y="10"
    width="80"
    height="80"
    rx="22"
    fill="url(#mdPlateGrad)"
    stroke="rgba(255,255,255,0.18)"
    stroke-width="1.5"
  />

  <!-- Top-left Soft Lighting -->
  <rect x="10" y="10" width="80" height="80" rx="22" fill="url(#mdGlow)" />

  <!-- Extruded 3D Chevron Side / Shadow -->
  <path
    d="M 28 42 L 53 67 L 78 42 L 81 46 L 55 72 L 25 46 Z"
    fill="url(#mdSideGrad)"
  />

  <!-- Front Chevron Stroke -->
  <path
    d="M 27 40 L 50 63 L 73 40"
    stroke="url(#mdFaceGrad)"
    stroke-width="13"
    stroke-linecap="round"
    stroke-linejoin="round"
  />
</svg>
