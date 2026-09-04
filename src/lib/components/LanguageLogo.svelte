<script lang="ts">
  import { themeStore } from "../stores/themeStore";
  import {
    resolveLanguageIconKey,
    getLanguageIconColor,
    getLanguageInkColor,
    getLanguageDisplayName,
    type LanguageIconKey,
  } from "../language/languageLogos";

  /**
   * File-type marks.
   *
   * Every glyph obeys three rules, and `LanguageLogo.test.ts` enforces all
   * three because the set that came before broke each of them somewhere:
   *
   *  1. **Absolute coordinates, inside the box.** The old Git and C++ marks
   *     carried relative arcs that walked past x=24, so the browser clipped
   *     them mid-stroke — the "artifact" was geometry leaving the viewBox.
   *     Absolute commands mean every number in a `d` is a real point, and a
   *     Bézier never escapes the hull of its own control points, so a numeric
   *     bounds sweep is a sound proof rather than an approximation.
   *  2. **Detail is painted, never knocked out.** Ruby's gem and Python's
   *     snakes used to stack overlapping subpaths in one `d`, where the
   *     nonzero rule punched holes through the middle of the shape. Details
   *     are separate elements in the ink colour, layered on top, so no fill
   *     rule is ever consulted. The one exception is Rust's cog, whose hole is
   *     strictly concentric, and it says `fill-rule="evenodd"` out loud.
   *  3. **No `<text>`.** JSON drew its braces with a `<text font-family=
   *     "monospace">`, which renders at the mercy of whatever font the host
   *     resolves and does not scale with the glyph.
   *
   * Colour comes from `getLanguageIconColor`, which walks the brand hue until
   * it clears 3:1 against the theme's worst-case row surface — the reason Lua
   * is no longer navy-on-navy and the image tint is no longer white-on-white.
   */

  let {
    language = "",
    filePath = "",
    iconKey,
    size = 14,
    class: className = "",
    title,
  }: {
    language?: string;
    filePath?: string;
    iconKey?: LanguageIconKey;
    size?: number;
    class?: string;
    title?: string;
  } = $props();

  let resolvedKey = $derived(iconKey ?? resolveLanguageIconKey(language || filePath || ""));
  let body = $derived(getLanguageIconColor(resolvedKey, $themeStore));
  let ink = $derived(getLanguageInkColor(resolvedKey, $themeStore));
  let tip = $derived(title ?? getLanguageDisplayName(resolvedKey));
</script>

<span
  class="inline-flex items-center justify-center shrink-0 select-none {className}"
  title={tip}
  aria-label={tip}
  role="img"
>
  <svg
    xmlns="http://www.w3.org/2000/svg"
    viewBox="0 0 24 24"
    width={size}
    height={size}
    class="block"
  >
    <title>{tip}</title>
    {#if resolvedKey === "rust"}
      <!-- Cog: teeth ring with a strictly concentric hole. -->
      <path
        fill={body}
        fill-rule="evenodd"
        d="M10.24 2.36 L13.76 2.36 L13.62 4.47 L16.18 5.53 L17.57 3.94 L20.06 6.43 L18.47 7.82 L19.53 10.38 L21.64 10.24 L21.64 13.76 L19.53 13.62 L18.47 16.18 L20.06 17.57 L17.57 20.06 L16.18 18.47 L13.62 19.53 L13.76 21.64 L10.24 21.64 L10.38 19.53 L7.82 18.47 L6.43 20.06 L3.94 17.57 L5.53 16.18 L4.47 13.62 L2.36 13.76 L2.36 10.24 L4.47 10.38 L5.53 7.82 L3.94 6.43 L6.43 3.94 L7.82 5.53 L10.38 4.47 Z M7.7 12 A4.3 4.3 0 0 0 16.3 12 A4.3 4.3 0 0 0 7.7 12 Z"
      />

    {:else if resolvedKey === "typescript" || resolvedKey === "javascript"}
      <!-- The two marks that genuinely are badges keep their square; the
           monogram is a stroke, so it can never spill past the corner radius
           the way the old filled letterforms did. -->
      <path fill={body} d="M7 2 L17 2 A5 5 0 0 1 22 7 L22 17 A5 5 0 0 1 17 22 L7 22 A5 5 0 0 1 2 17 L2 7 A5 5 0 0 1 7 2 Z" />
      <g fill="none" stroke={ink} stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
        {#if resolvedKey === "typescript"}
          <path d="M5.7 9.1 L11.1 9.1 M8.4 9.1 L8.4 16.6" />
        {:else}
          <path d="M10.6 8.6 L10.6 14.3 C10.6 15.8 9.7 16.7 8.3 16.7 C7.1 16.7 6.2 16.1 5.9 15.1" />
        {/if}
        <path
          d="M18.4 10.4 C18.4 9.2 17.4 8.5 16 8.5 C14.6 8.5 13.5 9.2 13.5 10.4 C13.5 12.8 18.6 11.9 18.6 14.5 C18.6 15.8 17.5 16.7 15.9 16.7 C14.5 16.7 13.5 16.1 13.1 15.2"
        />
      </g>

    {:else if resolvedKey === "python"}
      <!-- Two interlocking hooks with the eye dots, drawn as strokes so the
           halves cannot punch holes in each other. -->
      <g fill="none" stroke={body} stroke-width="3.6" stroke-linecap="round" stroke-linejoin="round">
        <path d="M15.8 4.4 L10.4 4.4 A3.6 3.6 0 0 0 6.8 8 L6.8 12" />
        <path d="M8.2 19.6 L13.6 19.6 A3.6 3.6 0 0 0 17.2 16 L17.2 12" />
      </g>
      <circle cx="12.9" cy="6.2" r="1.05" fill={ink} />
      <circle cx="11.1" cy="17.8" r="1.05" fill={ink} />

    {:else if resolvedKey === "go"}
      <!-- Gopher head. The eyes are painted in ink over the body, never cut
           out of it, so the ears may overlap the skull without consequence. -->
      <circle cx="6.6" cy="5.7" r="2.7" fill={body} />
      <circle cx="17.4" cy="5.7" r="2.7" fill={body} />
      <ellipse cx="12" cy="13" rx="8.4" ry="7.7" fill={body} />
      <ellipse cx="8.7" cy="11.3" rx="3.1" ry="3.4" fill={ink} />
      <ellipse cx="15.3" cy="11.3" rx="3.1" ry="3.4" fill={ink} />
      <circle cx="9.6" cy="11.5" r="1.3" fill={body} />
      <circle cx="16.2" cy="11.5" r="1.3" fill={body} />
      <ellipse cx="12" cy="17.2" rx="2.3" ry="1.6" fill={ink} />

    {:else if resolvedKey === "svelte"}
      <path
        fill="none"
        stroke={body}
        stroke-width="3.4"
        stroke-linecap="round"
        d="M16.4 7.4 C16.4 5.5 14.5 4.3 12.1 4.3 C9.6 4.3 7.7 5.6 7.7 7.5 C7.7 11.6 16.6 10 16.6 14.9 C16.6 17 14.6 18.4 12 18.4 C9.7 18.4 7.9 17.4 7.4 15.8"
      />

    {:else if resolvedKey === "html" || resolvedKey === "css"}
      <!-- One shield, two payloads: brackets for markup, rules for styles. -->
      <path fill={body} d="M3.1 2.2 L20.9 2.2 L19.3 19.9 L12 22 L4.7 19.9 Z" />
      <g fill="none" stroke={ink} stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
        {#if resolvedKey === "html"}
          <path d="M9.5 8.8 L6.9 11.8 L9.5 14.8 M14.5 8.8 L17.1 11.8 L14.5 14.8" />
        {:else}
          <path d="M7.9 9 L16.1 9 M7.9 13 L14 13 M7.9 17 L11.4 17" />
        {/if}
      </g>

    {:else if resolvedKey === "c" || resolvedKey === "cpp" || resolvedKey === "csharp"}
      <circle cx="12" cy="12" r="9.7" fill={body} />
      <g fill="none" stroke={ink} stroke-linecap="round" stroke-linejoin="round">
        {#if resolvedKey === "c"}
          <path
            stroke-width="2.3"
            d="M16 8.6 C15.1 7.7 13.8 7.2 12.4 7.2 C9.6 7.2 7.6 9.2 7.6 12 C7.6 14.8 9.6 16.8 12.4 16.8 C13.8 16.8 15.1 16.3 16 15.4"
          />
        {:else}
          <path
            stroke-width="2.1"
            d="M13.1 9 C12.4 8.3 11.4 7.9 10.3 7.9 C8.1 7.9 6.5 9.6 6.5 12 C6.5 14.4 8.1 16.1 10.3 16.1 C11.4 16.1 12.4 15.7 13.1 15"
          />
          {#if resolvedKey === "cpp"}
            <path stroke-width="1.7" d="M16 10.2 L16 13.4 M14.4 11.8 L17.6 11.8 M19.4 10.2 L19.4 13.4 M17.8 11.8 L21 11.8" />
          {:else}
            <path stroke-width="1.5" d="M16.4 9.4 L15.6 14.6 M19.2 9.4 L18.4 14.6 M14.8 11.2 L19.8 11.2 M14.5 12.9 L19.5 12.9" />
          {/if}
        {/if}
      </g>

    {:else if resolvedKey === "java"}
      <path fill={body} d="M4.6 9.4 L15.4 9.4 L15.4 15.2 A4.2 4.2 0 0 1 11.2 19.4 L8.8 19.4 A4.2 4.2 0 0 1 4.6 15.2 Z" />
      <g fill="none" stroke={body} stroke-width="1.9" stroke-linecap="round">
        <path d="M15.4 10.8 L17.2 10.8 A2.6 2.6 0 0 1 17.2 16 L15.4 16" />
        <path d="M3.4 21.4 L17.4 21.4" />
        <path d="M8.6 2.8 C7.4 4.4 9 5.4 7.8 7 M12.4 2.6 C11.2 4.2 12.8 5.2 11.6 6.8" />
      </g>

    {:else if resolvedKey === "ruby"}
      <path fill={body} d="M7.4 3.4 L16.6 3.4 L21.4 9.2 L12 21 L2.6 9.2 Z" />
      <g fill="none" stroke={ink} stroke-width="1.3" stroke-linejoin="round">
        <path d="M7.4 3.4 L9.6 9.2 M16.6 3.4 L14.4 9.2 M2.6 9.2 L21.4 9.2 M9.6 9.2 L12 21 M14.4 9.2 L12 21" />
      </g>

    {:else if resolvedKey === "php"}
      <ellipse cx="12" cy="12" rx="9.8" ry="6.6" fill={body} />
      <path
        fill="none"
        stroke={ink}
        stroke-width="2"
        stroke-linecap="round"
        stroke-linejoin="round"
        d="M8.6 15.8 L8.6 8.4 L12 8.4 A2.3 2.3 0 0 1 12 13 L8.6 13"
      />

    {:else if resolvedKey === "swift"}
      <path
        fill={body}
        d="M4.2 3.2 C9.6 6.4 14.6 10.4 18.2 15 C17.4 12.6 16.2 9.4 14.4 6.4 C17.4 9.2 19.6 13 20.6 17 C21 18.6 20.6 20.2 19.4 21 C17.2 22 12.8 21.4 9 19.2 C6.4 17.6 4.4 15.4 3.4 13 C5.8 14.8 8.4 16 10.8 16.2 C8 14 5.6 11 4.2 7.6 Z"
      />

    {:else if resolvedKey === "kotlin"}
      <!-- The single-colour Kotlin mark is one path: a square notched to its
           centre from the right edge. -->
      <path fill={body} d="M21.6 21.6 L2.4 21.6 L2.4 2.4 L21.6 2.4 L12 12 Z" />

    {:else if resolvedKey === "dart"}
      <path fill={body} d="M7.2 2.4 L2.4 2.4 L2.4 7.2 L16.8 21.6 L21.6 21.6 L21.6 16.8 Z" />

    {:else if resolvedKey === "shell"}
      <path fill={body} d="M6.4 3.6 L17.6 3.6 A4 4 0 0 1 21.6 7.6 L21.6 16.4 A4 4 0 0 1 17.6 20.4 L6.4 20.4 A4 4 0 0 1 2.4 16.4 L2.4 7.6 A4 4 0 0 1 6.4 3.6 Z" />
      <path
        fill="none"
        stroke={ink}
        stroke-width="2"
        stroke-linecap="round"
        stroke-linejoin="round"
        d="M7 9 L10 12 L7 15 M12.6 15.4 L17 15.4"
      />

    {:else if resolvedKey === "sql"}
      <ellipse cx="12" cy="6" rx="8.4" ry="3.6" fill={body} />
      <path fill={body} d="M3.6 6 L3.6 18 A8.4 3.6 0 0 0 20.4 18 L20.4 6 A8.4 3.6 0 0 1 3.6 6 Z" />
      <g fill="none" stroke={ink} stroke-width="1.4">
        <path d="M3.6 10.8 A8.4 3.6 0 0 0 20.4 10.8 M3.6 14.4 A8.4 3.6 0 0 0 20.4 14.4" />
      </g>

    {:else if resolvedKey === "lua"}
      <circle cx="10.4" cy="13.2" r="7.8" fill={body} />
      <circle cx="14.2" cy="9.4" r="2.6" fill={ink} />
      <circle cx="19" cy="5.2" r="2.8" fill={body} />

    {:else if resolvedKey === "zig"}
      <path fill={body} d="M4 3.2 L20 3.2 L20 6.8 L10.4 17.2 L20 17.2 L20 20.8 L4 20.8 L4 17.2 L13.6 6.8 L4 6.8 Z" />

    {:else if resolvedKey === "json"}
      <g fill="none" stroke={body} stroke-width="2.1" stroke-linecap="round" stroke-linejoin="round">
        <path d="M9.6 3.6 C7.6 3.6 7.6 7 7.6 9.2 C7.6 11 6 12 6 12 C6 12 7.6 13 7.6 14.8 C7.6 17 7.6 20.4 9.6 20.4" />
        <path d="M14.4 3.6 C16.4 3.6 16.4 7 16.4 9.2 C16.4 11 18 12 18 12 C18 12 16.4 13 16.4 14.8 C16.4 17 16.4 20.4 14.4 20.4" />
      </g>

    {:else if resolvedKey === "yaml"}
      <circle cx="5" cy="7.4" r="1.8" fill={body} />
      <circle cx="5" cy="12" r="1.8" fill={body} />
      <circle cx="5" cy="16.6" r="1.8" fill={body} />
      <path
        fill="none"
        stroke={body}
        stroke-width="2.2"
        stroke-linecap="round"
        d="M9.8 7.4 L19.4 7.4 M9.8 12 L19.4 12 M9.8 16.6 L15.2 16.6"
      />

    {:else if resolvedKey === "toml"}
      <path fill={body} d="M5.8 4.4 L18.2 4.4 A3 3 0 0 1 21.2 7.4 L21.2 16.6 A3 3 0 0 1 18.2 19.6 L5.8 19.6 A3 3 0 0 1 2.8 16.6 L2.8 7.4 A3 3 0 0 1 5.8 4.4 Z" />
      <path fill="none" stroke={ink} stroke-width="1.8" d="M2.8 9.8 L21.2 9.8 M10 9.8 L10 19.6" />

    {:else if resolvedKey === "markdown"}
      <path fill={body} d="M5.2 5 L18.8 5 A3.2 3.2 0 0 1 22 8.2 L22 15.8 A3.2 3.2 0 0 1 18.8 19 L5.2 19 A3.2 3.2 0 0 1 2 15.8 L2 8.2 A3.2 3.2 0 0 1 5.2 5 Z" />
      <g fill="none" stroke={ink} stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round">
        <path d="M5.6 15.6 L5.6 8.8 L8.6 12 L11.6 8.8 L11.6 15.6" />
        <path d="M15.8 8.8 L15.8 15.2 M13.6 12.9 L15.8 15.2 L18 12.9" />
      </g>

    {:else if resolvedKey === "xml"}
      <path
        fill="none"
        stroke={body}
        stroke-width="2.2"
        stroke-linecap="round"
        stroke-linejoin="round"
        d="M8.6 6.6 L3.4 12 L8.6 17.4 M15.4 6.6 L20.6 12 L15.4 17.4 M13.6 4.8 L10.4 19.2"
      />

    {:else if resolvedKey === "svg"}
      <path fill="none" stroke={body} stroke-width="2.2" stroke-linecap="round" d="M5 17.2 C5 9.4 12 6.8 19 6.8" />
      <path fill={body} d="M3.4 14.8 L6.6 14.8 A1.2 1.2 0 0 1 7.8 16 L7.8 19.2 A1.2 1.2 0 0 1 6.6 20.4 L3.4 20.4 A1.2 1.2 0 0 1 2.2 19.2 L2.2 16 A1.2 1.2 0 0 1 3.4 14.8 Z" />
      <path fill={body} d="M17.4 3.6 L20.6 3.6 A1.2 1.2 0 0 1 21.8 4.8 L21.8 8 A1.2 1.2 0 0 1 20.6 9.2 L17.4 9.2 A1.2 1.2 0 0 1 16.2 8 L16.2 4.8 A1.2 1.2 0 0 1 17.4 3.6 Z" />

    {:else if resolvedKey === "image"}
      <path fill={body} d="M5.6 4.4 L18.4 4.4 A3.2 3.2 0 0 1 21.6 7.6 L21.6 16.4 A3.2 3.2 0 0 1 18.4 19.6 L5.6 19.6 A3.2 3.2 0 0 1 2.4 16.4 L2.4 7.6 A3.2 3.2 0 0 1 5.6 4.4 Z" />
      <circle cx="8.2" cy="9.4" r="1.9" fill={ink} />
      <path fill={ink} d="M4.4 17.6 L9.8 12.2 L13 15.4 L16.2 12.6 L19.8 17.6 Z" />

    {:else if resolvedKey === "docker"}
      <g fill={body}>
        <path d="M4.6 9.2 L6.6 9.2 A0.5 0.5 0 0 1 7.1 9.7 L7.1 11.7 A0.5 0.5 0 0 1 6.6 12.2 L4.6 12.2 A0.5 0.5 0 0 1 4.1 11.7 L4.1 9.7 A0.5 0.5 0 0 1 4.6 9.2 Z" />
        <path d="M7.9 9.2 L9.9 9.2 A0.5 0.5 0 0 1 10.4 9.7 L10.4 11.7 A0.5 0.5 0 0 1 9.9 12.2 L7.9 12.2 A0.5 0.5 0 0 1 7.4 11.7 L7.4 9.7 A0.5 0.5 0 0 1 7.9 9.2 Z" />
        <path d="M11.2 9.2 L13.2 9.2 A0.5 0.5 0 0 1 13.7 9.7 L13.7 11.7 A0.5 0.5 0 0 1 13.2 12.2 L11.2 12.2 A0.5 0.5 0 0 1 10.7 11.7 L10.7 9.7 A0.5 0.5 0 0 1 11.2 9.2 Z" />
        <path d="M7.9 6 L9.9 6 A0.5 0.5 0 0 1 10.4 6.5 L10.4 8.5 A0.5 0.5 0 0 1 9.9 9 L7.9 9 A0.5 0.5 0 0 1 7.4 8.5 L7.4 6.5 A0.5 0.5 0 0 1 7.9 6 Z" />
        <path d="M11.2 6 L13.2 6 A0.5 0.5 0 0 1 13.7 6.5 L13.7 8.5 A0.5 0.5 0 0 1 13.2 9 L11.2 9 A0.5 0.5 0 0 1 10.7 8.5 L10.7 6.5 A0.5 0.5 0 0 1 11.2 6 Z" />
        <path d="M14.5 9.2 L16.5 9.2 A0.5 0.5 0 0 1 17 9.7 L17 11.7 A0.5 0.5 0 0 1 16.5 12.2 L14.5 12.2 A0.5 0.5 0 0 1 14 11.7 L14 9.7 A0.5 0.5 0 0 1 14.5 9.2 Z" />
        <path d="M1.9 13.2 L21.4 13.2 C21.4 18.2 17.6 21 12.4 21 C7.2 21 3 18.4 1.9 13.2 Z" />
      </g>

    {:else if resolvedKey === "git"}
      <path fill={body} d="M12 2.2 L21.8 12 L12 21.8 L2.2 12 Z" />
      <g stroke={ink} stroke-width="1.5" stroke-linecap="round" fill="none">
        <path d="M8.7 8.7 L15.3 15.3 M12 12 L15.3 8.7" />
      </g>
      <circle cx="8.7" cy="8.7" r="1.6" fill={ink} />
      <circle cx="15.3" cy="15.3" r="1.6" fill={ink} />
      <circle cx="15.3" cy="8.7" r="1.6" fill={ink} />

    {:else if resolvedKey === "lock"}
      <path fill="none" stroke={body} stroke-width="2.4" stroke-linecap="round" d="M7.8 10.4 L7.8 7 A4.2 4.2 0 0 1 16.2 7 L16.2 10.4" />
      <path fill={body} d="M7.2 10 L16.8 10 A3 3 0 0 1 19.8 13 L19.8 18.6 A3 3 0 0 1 16.8 21.6 L7.2 21.6 A3 3 0 0 1 4.2 18.6 L4.2 13 A3 3 0 0 1 7.2 10 Z" />
      <circle cx="12" cy="14.6" r="1.7" fill={ink} />
      <path fill="none" stroke={ink} stroke-width="1.9" stroke-linecap="round" d="M12 15.6 L12 18.2" />

    {:else if resolvedKey === "archive"}
      <path fill={body} d="M4.2 4.2 L19.8 4.2 A1.6 1.6 0 0 1 21.4 5.8 L21.4 7.6 A1.6 1.6 0 0 1 19.8 9.2 L4.2 9.2 A1.6 1.6 0 0 1 2.6 7.6 L2.6 5.8 A1.6 1.6 0 0 1 4.2 4.2 Z" />
      <path fill={body} d="M4.4 10.4 L19.6 10.4 L19.6 19.2 A2.4 2.4 0 0 1 17.2 21.6 L6.8 21.6 A2.4 2.4 0 0 1 4.4 19.2 Z" />
      <path fill="none" stroke={ink} stroke-width="2" stroke-linecap="round" d="M9.8 14.4 L14.2 14.4" />

    {:else if resolvedKey === "config"}
      <path fill="none" stroke={body} stroke-width="2" stroke-linecap="round" d="M3.4 7.4 L20.6 7.4 M3.4 12 L20.6 12 M3.4 16.6 L20.6 16.6" />
      <circle cx="9" cy="7.4" r="2.6" fill={body} />
      <circle cx="15.4" cy="12" r="2.6" fill={body} />
      <circle cx="7.4" cy="16.6" r="2.6" fill={body} />
      <circle cx="9" cy="7.4" r="1" fill={ink} />
      <circle cx="15.4" cy="12" r="1" fill={ink} />
      <circle cx="7.4" cy="16.6" r="1" fill={ink} />

    {:else}
      <!-- Plain document: the deliberate fallback, not a stand-in for a mark
           that failed to draw. -->
      <path fill={body} d="M6.2 2.4 L14.2 2.4 L19.8 8 L19.8 19.6 A2 2 0 0 1 17.8 21.6 L6.2 21.6 A2 2 0 0 1 4.2 19.6 L4.2 4.4 A2 2 0 0 1 6.2 2.4 Z" />
      <path fill="none" stroke={ink} stroke-width="1.6" stroke-linejoin="round" d="M14.2 2.6 L14.2 8 L19.6 8" />
      <path fill="none" stroke={ink} stroke-width="1.6" stroke-linecap="round" d="M8 12.6 L15.6 12.6 M8 16.2 L13.2 16.2" />
    {/if}
  </svg>
</span>
