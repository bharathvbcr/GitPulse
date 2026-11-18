<script lang="ts" module>
  /**
   * The vendors' own marks for the agent CLIs a terminal tab can run.
   *
   * Claude's path is the one simple-icons publishes (CC0, v13.21.0); Codex,
   * Grok and Antigravity are Lobe Icons' monochrome marks (MIT,
   * @lobehub/icons-static-svg 1.102.0). The marks themselves belong to
   * Anthropic, OpenAI, xAI and Google. Each is one path drawn in
   * `currentColor`, so the caller sets the tint, and none carries ids,
   * gradients or filters that would collide or cost paint when a list draws
   * dozens of them.
   *
   * `shell` is GitPulse's own: a prompt whose cursor is the heartbeat from the
   * app's logo, for a plain shell tab, kept to the same one-path contract.
   *
   * `manvi` is Manvi's published mark, not a one-path redraw: a Red700 tile
   * with a pixel M knocked out in Red050, on the 7x7 module grid that Manvi's
   * `manvi/ui/logo/logo.go` defines and `svg.go` renders into
   * `assets/manvi-mark.svg` (the same file its README and website show). It
   * is drawn in its own two colors, like an app icon, so it ignores the
   * caller's tint and reads the same on light and dark surfaces.
   */
  export type BrandMarkKind = "claude" | "codex" | "grok" | "agy" | "shell" | "manvi";

  /** Manvi's brand ramp: Red700 fills the tile, Red050 is the M (manvi/ui/brand/brand.go). */
  const MANVI_TILE = "#7a1626";
  const MANVI_GLYPH = "#fdf2f3";
  /** The M: Manvi's logo grid in 12-unit modules, as svg.go emits it, one subpath per run. */
  const MANVI_M =
    "M12 12h12v12h-12z" +
    "M60 12h12v12h-12z" +
    "M12 24h24v12h-24z" +
    "M48 24h24v12h-24z" +
    "M12 36h12v12h-12z" +
    "M36 36h12v12h-12z" +
    "M60 36h12v12h-12z" +
    "M12 48h12v12h-12z" +
    "M60 48h12v12h-12z" +
    "M12 60h12v12h-12z" +
    "M60 60h12v12h-12z";

  interface MarkPath {
    d: string;
    evenodd: boolean;
  }

  const MARKS: Readonly<Record<Exclude<BrandMarkKind, "manvi">, MarkPath>> = {
    claude: {
      evenodd: false,
      d: "m4.7144 15.9555 4.7174-2.6471.079-.2307-.079-.1275h-.2307l-.7893-.0486-2.6956-.0729-2.3375-.0971-2.2646-.1214-.5707-.1215-.5343-.7042.0546-.3522.4797-.3218.686.0608 1.5179.1032 2.2767.1578 1.6514.0972 2.4468.255h.3886l.0546-.1579-.1336-.0971-.1032-.0972L6.973 9.8356l-2.55-1.6879-1.3356-.9714-.7225-.4918-.3643-.4614-.1578-1.0078.6557-.7225.8803.0607.2246.0607.8925.686 1.9064 1.4754 2.4893 1.8336.3643.3035.1457-.1032.0182-.0728-.164-.2733-1.3539-2.4467-1.445-2.4893-.6435-1.032-.17-.6194c-.0607-.255-.1032-.4674-.1032-.7285L6.287.1335 6.6997 0l.9957.1336.419.3642.6192 1.4147 1.0018 2.2282 1.5543 3.0296.4553.8985.2429.8318.091.255h.1579v-.1457l.1275-1.706.2368-2.0947.2307-2.6957.0789-.7589.3764-.9107.7468-.4918.5828.2793.4797.686-.0668.4433-.2853 1.8517-.5586 2.9021-.3643 1.9429h.2125l.2429-.2429.9835-1.3053 1.6514-2.0643.7286-.8196.85-.9046.5464-.4311h1.0321l.759 1.1293-.34 1.1657-1.0625 1.3478-.8804 1.1414-1.2628 1.7-.7893 1.36.0729.1093.1882-.0183 2.8535-.607 1.5421-.2794 1.8396-.3157.8318.3886.091.3946-.3278.8075-1.967.4857-2.3072.4614-3.4364.8136-.0425.0304.0486.0607 1.5482.1457.6618.0364h1.621l3.0175.2247.7892.522.4736.6376-.079.4857-1.2142.6193-1.6393-.3886-3.825-.9107-1.3113-.3279h-.1822v.1093l1.0929 1.0686 2.0035 1.8092 2.5075 2.3314.1275.5768-.3218.4554-.34-.0486-2.2039-1.6575-.85-.7468-1.9246-1.621h-.1275v.17l.4432.6496 2.3436 3.5214.1214 1.0807-.17.3521-.6071.2125-.6679-.1214-1.3721-1.9246L14.38 17.959l-1.1414-1.9428-.1397.079-.674 7.2552-.3156.3703-.7286.2793-.6071-.4614-.3218-.7468.3218-1.4753.3886-1.9246.3157-1.53.2853-1.9004.17-.6314-.0121-.0425-.1397.0182-1.4328 1.9672-2.1796 2.9446-1.7243 1.8456-.4128.164-.7164-.3704.0667-.6618.4008-.5889 2.386-3.0357 1.4389-1.882.929-1.0868-.0062-.1579h-.0546l-6.3385 4.1164-1.1293.1457-.4857-.4554.0608-.7467.2307-.2429 1.9064-1.3114Z",
    },
    codex: {
      evenodd: true,
      d: "M8.086.457a6.105 6.105 0 013.046-.415c1.333.153 2.521.72 3.564 1.7a.117.117 0 00.107.029c1.408-.346 2.762-.224 4.061.366l.063.03.154.076c1.357.703 2.33 1.77 2.918 3.198.278.679.418 1.388.421 2.126a5.655 5.655 0 01-.18 1.631.167.167 0 00.04.155 5.982 5.982 0 011.578 2.891c.385 1.901-.01 3.615-1.183 5.14l-.182.22a6.063 6.063 0 01-2.934 1.851.162.162 0 00-.108.102c-.255.736-.511 1.364-.987 1.992-1.199 1.582-2.962 2.462-4.948 2.451-1.583-.008-2.986-.587-4.21-1.736a.145.145 0 00-.14-.032c-.518.167-1.04.191-1.604.185a5.924 5.924 0 01-2.595-.622 6.058 6.058 0 01-2.146-1.781c-.203-.269-.404-.522-.551-.821a7.74 7.74 0 01-.495-1.283 6.11 6.11 0 01-.017-3.064.166.166 0 00.008-.074.115.115 0 00-.037-.064 5.958 5.958 0 01-1.38-2.202 5.196 5.196 0 01-.333-1.589 6.915 6.915 0 01.188-2.132c.45-1.484 1.309-2.648 2.577-3.493.282-.188.55-.334.802-.438.286-.12.573-.22.861-.304a.129.129 0 00.087-.087A6.016 6.016 0 015.635 2.31C6.315 1.464 7.132.846 8.086.457zm-.804 7.85a.848.848 0 00-1.473.842l1.694 2.965-1.688 2.848a.849.849 0 001.46.864l1.94-3.272a.849.849 0 00.007-.854l-1.94-3.393zm5.446 6.24a.849.849 0 000 1.695h4.848a.849.849 0 000-1.696h-4.848z",
    },
    grok: {
      evenodd: true,
      d: "M9.27 15.29l7.978-5.897c.391-.29.95-.177 1.137.272.98 2.369.542 5.215-1.41 7.169-1.951 1.954-4.667 2.382-7.149 1.406l-2.711 1.257c3.889 2.661 8.611 2.003 11.562-.953 2.341-2.344 3.066-5.539 2.388-8.42l.006.007c-.983-4.232.242-5.924 2.75-9.383.06-.082.12-.164.179-.248l-3.301 3.305v-.01L9.267 15.292M7.623 16.723c-2.792-2.67-2.31-6.801.071-9.184 1.761-1.763 4.647-2.483 7.166-1.425l2.705-1.25a7.808 7.808 0 00-1.829-1A8.975 8.975 0 005.984 5.83c-2.533 2.536-3.33 6.436-1.962 9.764 1.022 2.487-.653 4.246-2.34 6.022-.599.63-1.199 1.259-1.682 1.925l7.62-6.815",
    },
    agy: {
      evenodd: true,
      d: "M21.751 22.607c1.34 1.005 3.35.335 1.508-1.508C17.73 15.74 18.904 1 12.037 1 5.17 1 6.342 15.74.815 21.1c-2.01 2.009.167 2.511 1.507 1.506 5.192-3.517 4.857-9.714 9.715-9.714 4.857 0 4.522 6.197 9.714 9.715z",
    },
    // A chevron and a one-beat pulse, each the outline of a 2.3-unit round
    // stroke, so the glyph is still a single filled path.
    shell: {
      evenodd: false,
      d: "M2.2 7.23L7.14 12L2.2 16.77A1.15 1.15 0 0 0 3.8 18.43L9.6 12.83A1.15 1.15 0 0 0 9.6 11.17L3.8 5.57A1.15 1.15 0 0 0 2.2 7.23ZM12 13.15L13.4 13.15A1.15 1.15 0 0 0 14.49 12.35L15.16 10.28L16.87 18.44A1.15 1.15 0 0 0 19.11 18.49L20.49 13.15L21 13.15A1.15 1.15 0 0 0 21 10.85L19.6 10.85A1.15 1.15 0 0 0 18.49 11.71L18.12 13.15L16.53 5.56A1.15 1.15 0 0 0 14.31 5.45L12.56 10.85L12 10.85A1.15 1.15 0 0 0 12 13.15Z",
    },
  };
</script>

<script lang="ts">
  /** Decorative: every call site names the session's kind in text as well. */
  interface Props {
    mark: BrandMarkKind;
    size?: number | string;
    class?: string;
  }
  let { mark, size = 12, class: className = "" }: Props = $props();

  const path = $derived(mark === "manvi" ? null : MARKS[mark]);
</script>

{#if path === null}
  <!-- Manvi's tile: square modules stay crisp at 12-16px; only the tile's
       rounded corner is anti-aliased. -->
  <svg
    class={className}
    width={size}
    height={size}
    viewBox="0 0 84 84"
    aria-hidden="true"
    focusable="false"
    data-brand-mark="manvi"
  >
    <rect width="84" height="84" rx="12" fill={MANVI_TILE} />
    <path d={MANVI_M} fill={MANVI_GLYPH} shape-rendering="crispEdges" />
  </svg>
{:else}
<svg
  class={className}
  width={size}
  height={size}
  viewBox="0 0 24 24"
  fill="currentColor"
  fill-rule={path.evenodd ? "evenodd" : undefined}
  clip-rule={path.evenodd ? "evenodd" : undefined}
  aria-hidden="true"
  focusable="false"
  data-brand-mark={mark}
>
  <path d={path.d} />
</svg>
{/if}
