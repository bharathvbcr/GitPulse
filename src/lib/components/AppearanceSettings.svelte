<script lang="ts">
  import { Check, RotateCcw } from "@lucide/svelte";
  import { themeStore, type ThemePreference } from "../stores/themeStore";
  import { interfaceStore } from "../stores/interfaceStore";
  import { hostPlatform } from "../stores/platformStore";
  import { nativeRenderingBoot, nativeRenderingStatus, type FlagStatus } from "../desktop/nativeRendering";
  import { ACCENTS, accentSwatch } from "../ui/accents";
  import { parseHex } from "../ui/color/oklch";
  import {
    CORNER_STYLES,
    FLOAT_BLUR_MAX,
    MATERIAL_FLOOR,
    appearanceNotes,
    resolveAccent,
    type FieldLevel,
  } from "../ui/appearance/apply";
  import type { Density } from "../ui/appearance/density";
  import { isFamilyInstalled, normalizeFamily } from "../ui/appearance/fonts";
  import { liquidGlassRequirement, shortcutTextLabel } from "../ui/platformCopy";
  import { formatTimestamp, type TimestampStyle } from "../ui/timestampStyle";
  import type { BranchRowLayout } from "../sidebar/metrics";
  import SettingToggle from "./SettingToggle.svelte";
  import SettingSegment from "./SettingSegment.svelte";
  import Slider from "./Slider.svelte";
  import ThemeFileSettings from "./ThemeFileSettings.svelte";

  /**
   * Settings -> Appearance, in the seven groups of the design language (§11):
   * Theme, Colors, Type, Shape, Material, Motion and Density, plus the terminal
   * and interface rows that were already on this page.
   *
   * Every control applies live through `interfaceStore`; the document follows
   * from `ui/appearance/apply.ts`. The Material rows are gated by the host's
   * `native_material` capability, and the group says why when they are absent
   * rather than showing sliders that would do nothing.
   */
  let {
    shown,
    filtering = false,
    themePreference,
    onTheme,
    tipsStatus = "",
    onResetTips,
    onReplayTour,
  }: {
    /** Whether a control survives the search filter and the host's capabilities. */
    shown: (id: string) => boolean;
    /** True while a search narrows the page, so a note about missing hardware stays out of the results. */
    filtering?: boolean;
    themePreference: ThemePreference;
    onTheme: (preference: ThemePreference) => void;
    tipsStatus?: string;
    onResetTips: () => void;
    onReplayTour: () => void;
  } = $props();

  const THEME_OPTIONS: readonly { value: ThemePreference; label: string; title: string }[] = [
    { value: "system", label: "System", title: "Follow the operating system appearance" },
    { value: "light", label: "Light", title: "Always use the light theme" },
    { value: "dark", label: "Dark", title: "Always use the dark theme" },
  ];

  const DENSITY_OPTIONS: readonly { value: Density; label: string; title: string }[] = [
    { value: "compact", label: "Compact", title: "Tighter rows and padding; fits more on screen" },
    { value: "comfortable", label: "Comfortable", title: "The default spacing" },
    { value: "spacious", label: "Spacious", title: "Roomier rows and padding" },
  ];

  const FIELD_OPTIONS: readonly { value: FieldLevel; label: string; title: string }[] = [
    { value: "off", label: "Off", title: "No color behind the window" },
    { value: "subtle", label: "Subtle", title: "Half the shipped color" },
    { value: "vivid", label: "Vivid", title: "The shipped background color" },
  ];

  const TRANSPARENCY_OPTIONS = [
    { value: "auto", label: "Auto", title: "Follow the system's Reduce Transparency setting" },
    { value: "reduced", label: "Reduced", title: "Always draw solid surfaces" },
  ] as const;

  const TERMINAL_SCROLLBACK_OPTIONS: readonly { value: string; label: string; title: string }[] =
    [500, 1000, 2000, 5000, 10000, 20000].map((rows) => ({
      value: String(rows),
      label: rows.toLocaleString("en-US"),
      title: `Keep ${rows.toLocaleString("en-US")} rows of history`,
    }));

  const TIMESTAMP_OPTIONS: readonly { value: TimestampStyle; label: string; title: string }[] = [
    { value: "relative", label: "Relative", title: "How long ago, e.g. 3d ago" },
    {
      value: "absolute",
      label: "Date",
      title: "Calendar date as YYYY-MM-DD — fixed width and the same in every locale",
    },
  ];

  const BRANCH_ROW_OPTIONS: readonly { value: BranchRowLayout; label: string; title: string }[] = [
    {
      value: "three-line",
      label: "Three lines",
      title: "Name on its own line, churn and counts on the second, last commit age and author on the third",
    },
    {
      value: "two-line",
      label: "Two lines",
      title: "Name on its own line, churn, counts, age and author together on the second",
    },
    {
      value: "one-line",
      label: "One line",
      title: "Everything on one line: about 45% more refs on screen, names truncate sooner",
    },
  ];

  /**
   * A fixed instant and a fixed "now" three days after it, so the preview
   * beside the control shows what each style looks like rather than drifting
   * with the clock (and rendering "13mo ago" a year from now).
   */
  const TIMESTAMP_SAMPLE = 1_756_000_000;
  const TIMESTAMP_SAMPLE_NOW = TIMESTAMP_SAMPLE + 3 * 86_400;

  const any = (ids: readonly string[]) => ids.some((id) => shown(id));

  const prefs = $derived($interfaceStore);
  const theme = $derived($themeStore);
  const nativeMaterial = $derived($hostPlatform.native_material);

  // The two native switches. What the reader asked for is `prefs`; what is in
  // effect is the backend's last answer, and an answer to an older request does
  // not count (it would draw "in effect" for a switch just flipped).
  const glassAnswer = $derived(
    $nativeRenderingStatus && $nativeRenderingStatus.liquid_glass.requested === prefs.nativeGlass
      ? $nativeRenderingStatus.liquid_glass
      : null,
  );
  // WebKit fixes the page's frame rate when the page starts, so a 120 Hz change
  // made after launch is saved but waits for the next launch.
  const refreshRestart = $derived(
    $nativeRenderingBoot !== null && $nativeRenderingBoot.highRefresh !== prefs.highRefresh
      ? prefs.highRefresh
        ? "Saved. Takes effect the next time GitPulse starts."
        : "Saved. The interface returns to 60 Hz the next time GitPulse starts."
      : null,
  );
  const refreshAnswer = $derived(
    $nativeRenderingStatus && $nativeRenderingStatus.high_refresh.requested === prefs.highRefresh
      ? $nativeRenderingStatus.high_refresh
      : null,
  );

  // --- accent -----------------------------------------------------------------
  const accent = $derived(resolveAccent(prefs, theme));
  // An imported theme brings its own accent; with the default preset still
  // selected, that accent is what draws, so no swatch may read as picked.
  const themeDrawsAccent = $derived(
    prefs.customTheme !== null && prefs.customTheme.appearance === theme && prefs.customAccent === "" && prefs.accent === "blue",
  );
  // Drafts follow the stored value, but only when that value itself changes:
  // reading the whole preference object here would reset a half-typed field on
  // every unrelated setting.
  const storedAccent = $derived($interfaceStore.customAccent);
  const storedFontUi = $derived($interfaceStore.fontUi);
  const storedFontMono = $derived($interfaceStore.fontMono);
  let hexDraft = $state("");
  let hexInvalid = $state(false);
  $effect(() => {
    hexDraft = storedAccent;
    hexInvalid = false;
  });
  const pickerValue = $derived(parseHex(prefs.customAccent) ? prefs.customAccent : accent.resolved);

  function commitHex(text: string) {
    const trimmed = text.trim();
    if (trimmed === "") {
      hexInvalid = false;
      interfaceStore.setCustomAccent("");
      return;
    }
    const withHash = trimmed.startsWith("#") ? trimmed : `#${trimmed}`;
    if (!parseHex(withHash)) {
      hexInvalid = true;
      return;
    }
    hexInvalid = false;
    interfaceStore.setCustomAccent(withHash.toLowerCase());
  }

  // --- type -------------------------------------------------------------------
  type FontKind = "ui" | "mono";
  let fontDraft = $state<Record<FontKind, string>>({ ui: "", mono: "" });
  let fontInvalid = $state<Record<FontKind, boolean>>({ ui: false, mono: false });
  $effect(() => {
    fontDraft = { ui: storedFontUi, mono: storedFontMono };
    fontInvalid = { ui: false, mono: false };
  });

  function commitFont(kind: FontKind, text: string) {
    const family = normalizeFamily(text);
    if (family === null) {
      fontInvalid[kind] = true;
      return;
    }
    fontInvalid[kind] = false;
    if (kind === "ui") interfaceStore.setFontUi(family);
    else interfaceStore.setFontMono(family);
  }

  /** `true`, `false`, or `null` when the machine could not be asked. */
  function installed(family: string): boolean | null {
    return family === "" ? null : isFamilyInstalled(family);
  }
  const uiInstalled = $derived(installed(prefs.fontUi));
  const monoInstalled = $derived(installed(prefs.fontMono));

  // --- shape ------------------------------------------------------------------
  const CORNER_OPTIONS: readonly { value: string; label: string; title: string }[] = CORNER_STYLES.map((style) => ({
    value: style.id,
    label: style.label,
    title: `Corner radius ${style.scale}× the default`,
  }));
  const cornerId = $derived(
    CORNER_STYLES.find((style) => Math.abs(style.scale - prefs.radiusScale) < 1e-6)?.id ?? "",
  );

  // --- material ---------------------------------------------------------------
  const floor = $derived(MATERIAL_FLOOR[theme]);
  const intensity = $derived(Math.max(prefs.materialIntensity[theme], floor));
  const transparencyChoice = $derived(prefs.reduceTransparency ? "reduced" : "auto");
</script>

<!--
  What a native switch is doing. Three states and no fourth: asked and not yet
  answered, answered and in effect, answered and refused with a reason. A
  refusal is never drawn as in effect, and is also recorded in Diagnostics.
-->
{#snippet outcome(asked: boolean, answer: FlagStatus | null, setting: string, restartNote: string | null)}
  {#if asked && answer !== null && !answer.active}
    <div class="gp-notice" data-tone="warn" role="status" data-native-outcome="refused" data-native-for={setting}>
      <div class="min-w-0 flex-1">Not in effect. {answer.reason ?? "The window gave no reason."} Diagnostics has the same note.</div>
    </div>
  {:else if restartNote !== null}
    <p class="text-micro text-textMuted" data-native-outcome="restart" data-native-for={setting}>{restartNote}</p>
  {:else if asked}
    {#if answer === null}
      <p class="text-micro text-textMuted" data-native-outcome="pending" data-native-for={setting}>Applying…</p>
    {:else}
      <p class="text-micro text-ok" data-native-outcome="active" data-native-for={setting}>In effect.</p>
    {/if}
  {/if}
{/snippet}

{#snippet fontRow(kind: FontKind, label: string, placeholder: string, ok: boolean | null, chosen: string)}
        <label class="flex items-center gap-2 text-meta">
          <span class="w-24 shrink-0 font-medium text-textPrimary">{label}</span>
          <input
            type="text"
            class="gp-field min-w-0 flex-1"
            bind:value={fontDraft[kind]}
            placeholder={placeholder}
            maxlength="64"
            spellcheck="false"
            autocomplete="off"
            aria-label={label}
            aria-invalid={fontInvalid[kind]}
            data-invalid={fontInvalid[kind] ? "" : undefined}
            onchange={(event) => commitFont(kind, event.currentTarget.value)}
          />
        </label>
        {#if fontInvalid[kind]}
          <p class="text-meta text-danger" role="alert">
            Use one font family name: letters, digits, spaces and hyphens.
          </p>
        {:else if chosen !== ""}
          {#if ok === true}
            <p class="text-meta text-ok" role="status">“{chosen}” is installed on this computer.</p>
          {:else if ok === false}
            <p class="text-meta text-warn" role="status">
              “{chosen}” isn't installed on this computer, so the system font draws instead.
            </p>
          {:else}
            <p class="text-meta text-unknown" role="status">
              Couldn't check whether “{chosen}” is installed. The system font draws if it isn't.
            </p>
          {/if}
        {/if}
{/snippet}

<div class="flex flex-col gap-5" data-appearance-settings>
  <!-- Theme -->
  <section class="flex flex-col gap-3" hidden={!any(["theme", "theme-file"])} data-group="theme">
    <h3 class="gp-label">Theme</h3>
    <div data-setting="theme" hidden={!shown("theme")}>
      <SettingSegment
        ariaLabel="Theme appearance"
        options={THEME_OPTIONS}
        value={themePreference}
        onselect={onTheme}
      />
    </div>
    <div data-setting="theme-file" hidden={!shown("theme-file")}>
      <ThemeFileSettings />
    </div>
  </section>

  <!-- Colors -->
  <section class="flex flex-col gap-3" hidden={!any(["accent"])} data-group="colors">
    <h3 class="gp-label">Colors</h3>
    <div data-setting="accent" hidden={!shown("accent")} class="flex flex-col gap-2">
      <!-- `group` + `aria-pressed`, not `radiogroup` + `radio`, matching every
           other single-choice control on this page (SettingSegment). A
           radiogroup would promise arrow-key movement and one tab stop, a
           second keyboard contract to honor for no gain. -->
      <div class="flex flex-wrap items-center gap-2" role="group" aria-label="Accent color">
        {#each ACCENTS as preset (preset.id)}
          {@const picked = prefs.customAccent === "" && prefs.accent === preset.id && !themeDrawsAccent}
          <button
            type="button"
            aria-pressed={picked}
            aria-label="{preset.label} accent"
            title="{preset.label} accent"
            onclick={() => {
              hexInvalid = false;
              interfaceStore.setAccent(preset.id);
            }}
            class="size-6 rounded-full border border-edge transition-transform {picked
              ? 'border-textPrimary scale-110'
              : 'hover:scale-105'}"
            style="background-color: {accentSwatch(preset.id, theme)}"
          >
            {#if picked}
              <Check size={12} class="mx-auto text-onAccent" />
            {/if}
          </button>
        {/each}
      </div>

      <div class="flex flex-wrap items-center gap-2">
        <label class="flex items-center gap-2 text-meta text-textMuted">
          <span class="shrink-0">Custom</span>
          <input
            type="color"
            value={pickerValue}
            aria-label="Custom accent color"
            data-accent-picker
            class="size-6 cursor-pointer rounded-full border border-edge bg-transparent p-0"
            oninput={(event) => interfaceStore.setCustomAccent(event.currentTarget.value)}
          />
        </label>
        <input
          type="text"
          class="gp-field w-24"
          bind:value={hexDraft}
          placeholder="#rrggbb"
          maxlength="7"
          spellcheck="false"
          autocomplete="off"
          aria-label="Custom accent hex"
          aria-invalid={hexInvalid}
          data-invalid={hexInvalid ? "" : undefined}
          data-accent-hex
          onchange={(event) => commitHex(event.currentTarget.value)}
        />
        {#if prefs.customAccent !== ""}
          <button
            type="button"
            class="gp-btn"
            data-variant="ghost"
            data-size="sm"
            onclick={() => interfaceStore.setCustomAccent("")}
          >
            Use preset
          </button>
        {/if}
      </div>
      {#if hexInvalid}
        <p class="text-meta text-danger" role="alert">Enter a color as #rrggbb.</p>
      {/if}

      <!-- The guard's verdict for the color in force, never silent (§4.3). -->
      {#if accent.status === "passes"}
        <p class="text-meta text-ok" role="status" data-accent-guard="passes">
          Passes AA in {theme} mode: {accent.ratio.toFixed(1)}:1 against the dimmest surface.
        </p>
      {:else if accent.status === "adjusted"}
        <p class="text-meta text-warn" role="status" data-accent-guard="adjusted">
          Adjusted from {accent.requested} to {accent.resolved} ({accent.ratio.toFixed(1)}:1) so accent text stays
          readable in {theme} mode.
        </p>
      {:else if accent.status === "snapped"}
        <p class="text-meta text-warn" role="status" data-accent-guard="snapped">
          {accent.requested} can't reach readable contrast in {theme} mode. Using the nearest preset instead.
        </p>
      {/if}
      {#each $appearanceNotes.filter((note) => note.id === "theme-refused" || note.id === "theme-other-mode") as note (note.id)}
        <p class="text-meta {note.tone === 'warn' ? 'text-warn' : 'text-textMuted'}" role="status">{note.text}</p>
      {/each}

      <p class="text-micro text-textMuted">
        Tints selection, focus rings and the commit graph's current-row marker. Every color is checked against WCAG
        AA in light and dark before it is drawn, so switching theme never leaves the accent unreadable.
      </p>
    </div>
  </section>

  <!-- Type -->
  <section class="flex flex-col gap-3" hidden={!any(["ui-scale", "font-ui", "font-mono"])} data-group="type">
    <h3 class="gp-label">Type</h3>
    <div data-setting="ui-scale" hidden={!shown("ui-scale")} class="flex flex-col gap-1.5">
      <Slider
        label="Text size"
        value={prefs.uiFontScale}
        min={0.75}
        max={1.4}
        step={0.05}
        format={(n) => `${Math.round(n * 100)}%`}
        oninput={(next) => interfaceStore.setFontScale(next)}
      />
      <div class="flex items-center gap-2">
        <button
          type="button"
          onclick={() => interfaceStore.zoomOut()}
          class="gp-btn"
          data-size="sm"
          title="Zoom out ({shortcutTextLabel('⌘-', $hostPlatform.os)})"
        >
          Smaller
        </button>
        <button
          type="button"
          onclick={() => interfaceStore.zoomIn()}
          class="gp-btn"
          data-size="sm"
          title="Zoom in ({shortcutTextLabel('⌘+', $hostPlatform.os)})"
        >
          Larger
        </button>
        <button
          type="button"
          onclick={() => interfaceStore.resetZoom()}
          class="gp-btn"
          data-variant="ghost"
          data-size="sm"
          title="Reset zoom ({shortcutTextLabel('⌘0', $hostPlatform.os)})"
        >
          Reset
        </button>
      </div>
    </div>

    <div data-setting="font-ui" hidden={!shown("font-ui")} class="flex flex-col gap-1.5">
      {@render fontRow("ui", "Interface font", "System font", uiInstalled, prefs.fontUi)}
    </div>
    <div data-setting="font-mono" hidden={!shown("font-mono")} class="flex flex-col gap-1.5">
      {@render fontRow("mono", "Code font", "System monospace", monoInstalled, prefs.fontMono)}
    </div>
    {#if any(["font-ui", "font-mono"])}
      <p class="text-micro text-textMuted">
        Installed fonts only. GitPulse never downloads a font; a name that isn't installed falls back to the system
        stack. The code font applies to diffs, code views and Markdown; terminals and the commit graph still draw
        their own.
      </p>
    {/if}
  </section>

  <!-- Shape -->
  <section class="flex flex-col gap-3" hidden={!any(["corner-style"])} data-group="shape">
    <h3 class="gp-label">Shape</h3>
    <div data-setting="corner-style" hidden={!shown("corner-style")} class="flex flex-col gap-1.5">
      <SettingSegment
        ariaLabel="Corner style"
        options={CORNER_OPTIONS}
        value={cornerId}
        onselect={(id) =>
          interfaceStore.setRadiusScale(CORNER_STYLES.find((style) => style.id === id)?.scale ?? 1)}
      />
      {#if cornerId === ""}
        <p class="text-micro text-textMuted">Custom corners at {prefs.radiusScale}× from an imported theme.</p>
      {/if}
    </div>
  </section>

  <!-- Material -->
  <section
    class="flex flex-col gap-3"
    hidden={nativeMaterial
      ? !any(["material-intensity", "float-blur", "background-field", "transparency", "native-glass"])
      : filtering}
    data-group="material"
  >
    <h3 class="gp-label">Material</h3>
    {#if !nativeMaterial}
      <div class="gp-notice" data-tone="info" role="status" data-material-unavailable>
        <div class="min-w-0 flex-1">
          Glass material settings aren't available on this computer. Material intensity, float blur, the background
          field and transparency tune a translucent window material, which this computer's window doesn't use.
        </div>
      </div>
    {/if}

    <div data-setting="material-intensity" hidden={!shown("material-intensity")} class="flex flex-col gap-1.5">
      <Slider
        label="Material intensity"
        value={intensity}
        min={floor}
        max={1}
        step={0.05}
        format={(n) => `${Math.round(n * 100)}%`}
        oninput={(next) => interfaceStore.setMaterialIntensity(theme, next)}
      />
      <p class="text-micro text-textMuted">
        More transparent on the left, more opaque on the right.
        {#if floor > 0}
          In {theme} mode it can't go below {Math.round(floor * 100)}%: thinner glass drops muted text under 4.5:1
          over a dark desktop.
        {/if}
        Reduce Transparency in the system or below always wins.
      </p>
    </div>

    <div data-setting="float-blur" hidden={!shown("float-blur")} class="flex flex-col gap-1.5">
      <Slider
        label="Float blur"
        value={prefs.floatBlur}
        min={0}
        max={FLOAT_BLUR_MAX}
        step={1}
        format={(n) => (n === 0 ? "Off" : `${n} px`)}
        oninput={(next) => interfaceStore.setFloatBlur(next)}
      />
      <p class="text-micro text-textMuted">
        Blur behind menus, popovers and dialogs only. Larger radii cost graphics time; Off draws them without any
        blur.
      </p>
    </div>

    <div data-setting="background-field" hidden={!shown("background-field")} class="flex flex-col gap-2">
      <div class="text-meta font-medium text-textPrimary">Background field</div>
      <SettingSegment
        ariaLabel="Background field"
        options={FIELD_OPTIONS}
        value={prefs.fieldLevel}
        onselect={(level) => interfaceStore.setFieldLevel(level)}
      />
      <Slider
        label="Field hue"
        value={prefs.fieldHue}
        min={0}
        max={360}
        step={5}
        disabled={prefs.fieldLevel === "off"}
        format={(n) => `${n}°`}
        oninput={(next) => interfaceStore.setFieldHue(next)}
      />
      <p class="text-micro text-textMuted">
        The color glowing behind the window. Vivid is the shipped strength and the ceiling; the hue turns all four
        colors together.
      </p>
    </div>

    <div data-setting="transparency" hidden={!shown("transparency")} class="flex flex-col gap-1.5">
      <div class="text-meta font-medium text-textPrimary">Transparency</div>
      <SettingSegment
        ariaLabel="Transparency"
        options={TRANSPARENCY_OPTIONS}
        value={transparencyChoice}
        onselect={(choice) => interfaceStore.setReduceTransparency(choice === "reduced")}
      />
      <p class="text-micro text-textMuted">
        Auto follows the system. Reduced draws solid surfaces whatever the system says. It can only add reduction: if
        the system already asks for less transparency, GitPulse honors that.
      </p>
    </div>

    <div data-setting="native-glass" hidden={!shown("native-glass")} class="flex flex-col gap-1.5">
      <SettingToggle
        label="Liquid Glass window"
        description="Draws the window on Apple's Liquid Glass material instead of the standard one. {liquidGlassRequirement($hostPlatform.os)} It stays off unless you turn it on."
        ariaLabel="Use the Liquid Glass window material"
        checked={prefs.nativeGlass}
        onchange={(next) => interfaceStore.setNativeGlass(next)}
      />
      {@render outcome(prefs.nativeGlass, glassAnswer, "native-glass", null)}
    </div>
  </section>

  <!-- Motion -->
  <section class="flex flex-col gap-3" hidden={!any(["motion", "high-refresh"])} data-group="motion">
    <h3 class="gp-label">Motion</h3>
    <div data-setting="motion" hidden={!shown("motion")}>
      <SettingToggle
        label="Reduce motion"
        description="Drops view entrances, modal scaling, the theme crossfade and spinner rotation. Spinners keep their glyph, so nothing stops telling you it is working."
        ariaLabel="Reduce interface motion"
        checked={prefs.reduceMotion}
        onchange={(next) => interfaceStore.setReduceMotion(next)}
      />
      <p class="text-micro text-textMuted">
        Leaving this off follows the system setting. It can only add reduction: if the operating system already asks
        for less motion, GitPulse honors that whatever this says.
      </p>
    </div>
    <div data-setting="high-refresh" hidden={!shown("high-refresh")} class="flex flex-col gap-1.5">
      <SettingToggle
        label="120 Hz rendering"
        description="Lets the interface draw more than 60 frames a second on a display that supports it. Uses a private WebKit switch that a system update can remove, and it stays off unless you turn it on. A change takes effect the next time GitPulse starts."
        ariaLabel="Render the interface above 60 Hz"
        checked={prefs.highRefresh}
        onchange={(next) => interfaceStore.setHighRefresh(next)}
      />
      {@render outcome(prefs.highRefresh, refreshAnswer, "high-refresh", refreshRestart)}
    </div>
  </section>

  <!-- Density -->
  <section class="flex flex-col gap-3" hidden={!any(["density"])} data-group="density">
    <h3 class="gp-label">Density</h3>
    <div data-setting="density" hidden={!shown("density")} class="flex flex-col gap-1.5">
      <SettingSegment
        ariaLabel="Density"
        options={DENSITY_OPTIONS}
        value={prefs.density}
        onselect={(density) => interfaceStore.setDensity(density)}
      />
      <p class="text-micro text-textMuted">
        Sets row height, header height and padding everywhere, and how tightly the commit graph, branch list, diff
        and file tree pack their rows.
      </p>
    </div>
  </section>

  <!-- Terminal -->
  <section
    class="flex flex-col gap-3"
    hidden={!any([
      "terminal-screen-reader",
      "terminal-gpu",
      "terminal-option-as-meta",
      "terminal-smooth-wheel",
      "terminal-scrollback",
    ])}
    data-group="terminal"
  >
    <h3 class="gp-label">Terminal</h3>
    <div data-setting="terminal-screen-reader" hidden={!shown("terminal-screen-reader")}>
      <SettingToggle
        label="Terminal screen reader support"
        description="Builds an accessible row-by-row tree of the terminal grid and announces new output. Without it the terminal is a picture of text to assistive technology."
        ariaLabel="Terminal screen reader support"
        checked={prefs.terminalScreenReader}
        onchange={(next) => interfaceStore.setTerminalScreenReader(next)}
      />
      <p class="text-micro text-textMuted">
        Off by default because the tree is rebuilt as output arrives, which costs time on a session that prints
        continuously. It does not make the shell's input line an editable field: text-expansion tools that rewrite
        the focused field still cannot act inside a terminal.
      </p>
    </div>

    <div data-setting="terminal-gpu" hidden={!shown("terminal-gpu")}>
      <SettingToggle
        label="GPU terminal rendering"
        description="Draws the terminals you can see with WebGL instead of page elements, which keeps a continuously redrawing agent off the interface's main thread."
        ariaLabel="GPU terminal rendering"
        checked={prefs.terminalGpu}
        onchange={(next) => interfaceStore.setTerminalGpu(next)}
      />
      <p class="text-micro text-textMuted">
        On by default. Turn it off if terminal text draws wrongly on your graphics hardware. A terminal whose GPU
        context is lost goes back to the ordinary renderer for the rest of its life on its own.
      </p>
    </div>

    <div data-setting="terminal-option-as-meta" hidden={!shown("terminal-option-as-meta")}>
      <SettingToggle
        label="Option as Meta in terminals"
        description="Option+B and Option+F move by word and Option+key reaches shell and agent shortcuts, instead of typing a symbol."
        ariaLabel="Option as Meta in terminals"
        checked={prefs.terminalOptionAsMeta}
        onchange={(next) => interfaceStore.setTerminalOptionAsMeta(next)}
      />
      <p class="text-micro text-textMuted">
        On by default. Turn it off if your keyboard layout types a character with Option, such as @ on a German
        layout: with it on, that character cannot be typed inside a terminal. Option-click still selects text in
        programs that capture the mouse. Applies to open terminals at once.
      </p>
    </div>

    <div data-setting="terminal-smooth-wheel" hidden={!shown("terminal-smooth-wheel")}>
      <SettingToggle
        label="Smooth mouse-wheel scrolling in terminals"
        description="A mouse-wheel notch glides over a moment instead of jumping several rows."
        ariaLabel="Smooth mouse-wheel scrolling in terminals"
        checked={prefs.terminalSmoothWheel}
        onchange={(next) => interfaceStore.setTerminalSmoothWheel(next)}
      />
      <p class="text-micro text-textMuted">
        On by default. A trackpad always scrolls directly, and a system setting for reduced motion turns the glide
        off whatever this says. Applies to open terminals at once.
      </p>
    </div>

    <div data-setting="terminal-scrollback" hidden={!shown("terminal-scrollback")} class="flex flex-col gap-1.5">
      <div class="text-meta text-textMuted">Terminal scrollback: shells (rows)</div>
      <SettingSegment
        ariaLabel="Shell terminal scrollback rows"
        options={TERMINAL_SCROLLBACK_OPTIONS}
        value={String(prefs.terminalScrollback)}
        onselect={(rows) => interfaceStore.setTerminalScrollback(Number(rows))}
      />
      <div class="text-meta text-textMuted">Terminal scrollback: agents and task runs (rows)</div>
      <SettingSegment
        ariaLabel="Agent terminal scrollback rows"
        options={TERMINAL_SCROLLBACK_OPTIONS}
        value={String(prefs.terminalAgentScrollback)}
        onselect={(rows) => interfaceStore.setTerminalAgentScrollback(Number(rows))}
      />
      <p class="text-micro text-textMuted">
        Rows of history each terminal keeps. Every terminal holds about twelve bytes per column per row whether or
        not the row has text (roughly 12 MB for 5,000 rows at 200 columns), and the terminals of every repository
        tab you have visited stay open, so agents default to 1,000. Lowering a value trims open terminals at once.
      </p>
    </div>
  </section>

  <!-- Interface -->
  <section
    class="flex flex-col gap-3"
    hidden={!any(["timestamps", "branch-rows", "coach-marks", "walkthrough-replay"])}
    data-group="interface"
  >
    <h3 class="gp-label">Interface</h3>
    <div data-setting="timestamps" hidden={!shown("timestamps")} class="flex flex-col gap-1.5">
      <div class="flex items-center justify-between">
        <span class="text-meta text-textMuted">Timestamps</span>
        <span class="font-mono text-meta text-textMuted">
          {formatTimestamp(TIMESTAMP_SAMPLE, prefs.timestampStyle, TIMESTAMP_SAMPLE_NOW)}
        </span>
      </div>
      <SettingSegment
        ariaLabel="Timestamp style"
        options={TIMESTAMP_OPTIONS}
        value={prefs.timestampStyle}
        onselect={(style) => interfaceStore.setTimestampStyle(style)}
      />
      <p class="text-micro text-textMuted">
        Applies to the commit list, branch tooltips, the diff's change picker, Work and the stack. Hovering a commit
        time in the list shows the other form, and the graph's node card keeps showing both whichever way this is
        set.
      </p>
    </div>

    <div data-setting="branch-rows" hidden={!shown("branch-rows")} class="flex flex-col gap-1.5">
      <div class="text-meta text-textMuted">Sidebar branch rows</div>
      <SettingSegment
        ariaLabel="Sidebar branch row layout"
        options={BRANCH_ROW_OPTIONS}
        value={prefs.branchRowLayout}
        onselect={(layout) => interfaceStore.setBranchRowLayout(layout)}
      />
      <p class="text-micro text-textMuted">
        On one line the branch name is the only thing that can shrink, so a row of counters and buttons squeezes it
        down to a few characters. Two lines give the name the full width and move every number — lines changed,
        ahead and behind, files, age and author — underneath it. Three lines also give the age and author a line of
        their own, so a branch with many numbers never loses the author to them. Density still sets how tall each
        line is.
      </p>
    </div>

    <div
      data-setting="coach-marks"
      hidden={!shown("coach-marks")}
      class="flex items-center justify-between gap-3"
    >
      <div class="min-w-0">
        <div class="text-meta font-medium text-textPrimary">First-run coach marks</div>
        <div class="text-micro text-textMuted">Bring back the one-time tips shown on a fresh install.</div>
      </div>
      <button type="button" onclick={onResetTips} class="gp-btn shrink-0" data-variant="secondary" data-size="sm">
        Reset tips
      </button>
      {#if tipsStatus}<span class="text-micro text-textMuted" role="status">{tipsStatus}</span>{/if}
    </div>

    <div
      data-setting="walkthrough-replay"
      hidden={!shown("walkthrough-replay")}
      class="flex items-center justify-between gap-3"
    >
      <div class="min-w-0">
        <div class="text-meta font-medium text-textPrimary">Guided walkthrough</div>
        <div class="text-micro text-textMuted">
          Replay the six-step tour. It starts from where you left off, or from the beginning once finished.
        </div>
      </div>
      <button type="button" onclick={onReplayTour} class="gp-btn shrink-0" data-variant="secondary" data-size="sm">
        Replay tour
      </button>
    </div>
  </section>

  <div class="flex justify-end">
    <button
      type="button"
      class="gp-btn"
      data-variant="ghost"
      data-size="sm"
      data-reset-appearance
      onclick={() => interfaceStore.resetAppearance()}
    >
      <RotateCcw size={12} />
      Reset appearance
    </button>
  </div>
</div>
