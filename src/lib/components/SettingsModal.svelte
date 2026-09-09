<script lang="ts">
  import { fade, scale } from "svelte/transition";
  import { themeStore, type ThemePreference } from "../stores/themeStore";
  import { densityStore, type DensityMode } from "../stores/densityStore";
  import { interfaceStore } from "../stores/interfaceStore";
  import {
    backdropFade,
    backdropFadeOut,
    cardScale,
    cardScaleOut,
  } from "../ui/transitions";
  import { trapFocus } from "../ui/focusTrap";
  import { LAYERS } from "../ui/layers";
  import {
    Settings,
    Palette,
    PanelsTopLeft,
    Eye,
    GitBranch,
    FileCode,
    FlaskConical,
    RefreshCw,
    Plug,
    Copy,
    Check,
    AlertTriangle,
    RotateCcw,
    Search,
    X,
  } from "@lucide/svelte";
  import { getMcpInfo } from "../insights/client";
  import type { McpInfo } from "../insights/types";
  import { copyText } from "../desktop/clipboard";
  import {
    checkForAppUpdate,
    describeUpdateCheck,
    type UpdateStatus,
  } from "../updates/updateCheck";
  import { openExternal } from "../desktop/openExternal";
  import { formatError } from "../ui/formatError";
  import { askConfirm } from "../stores/modalStore";
  import {
    SETTINGS_SECTIONS,
    type SettingsSectionId,
  } from "../ui/settingsSections";
  import { matchSettings } from "../ui/settingsCatalog";
  import { VIEW_NAV } from "../views/viewNav";
  import type { GraphWidthMode } from "../graph/graphLayout";
  import type { RefScope } from "../graph/refScope";
  import type { StatusBarMode } from "../ui/statusBarMode";
  import type { DiagnosticsButtonMode } from "../ui/diagnosticsButton";
  import { ACCENTS, accentSwatch } from "../ui/accents";
  import {
    TAB_WIDTHS,
    type DiffLayout,
    type TabWidth,
  } from "../ui/codeDisplay";
  import { formatTimestamp, type TimestampStyle } from "../ui/timestampStyle";
  import SettingToggle from "./SettingToggle.svelte";
  import SettingSegment from "./SettingSegment.svelte";
  import ExternalToolsPanel from "./ExternalToolsPanel.svelte";

  let {
    isOpen = false,
    onClose,
  }: {
    isOpen?: boolean;
    onClose?: () => void;
  } = $props();

  let activeSection = $state<SettingsSectionId>("appearance");
  let themePreference = $state<ThemePreference>(themeStore.preference());

  function setTheme(preference: ThemePreference) {
    themePreference = preference;
    themeStore.setPreference(preference);
  }

  // --- filter ------------------------------------------------------------
  // Thirty-odd controls across eight categories is past the point where
  // grouping alone finds one: knowing that "wrap" lives under Diff & code
  // means already knowing the taxonomy. The catalog answers that instead.
  let query = $state("");
  let railEl = $state<HTMLElement>();

  const filter = $derived(matchSettings(query));
  const matchedSettings = $derived(new Set(filter.settings));
  const railSections = $derived(
    SETTINGS_SECTIONS.filter((entry) => filter.sections.includes(entry.id)),
  );
  const noMatches = $derived(!filter.all && railSections.length === 0);

  /** Whether one control survives the filter; `data-setting` ids match the catalog. */
  const shown = (id: string): boolean => filter.all || matchedSettings.has(id);

  // A filter that leaves the open panel behind would show an empty pane and
  // read as "no results" while results sit one category away.
  $effect(() => {
    if (railSections.length === 0) return;
    if (!railSections.some((entry) => entry.id === activeSection)) {
      activeSection = railSections[0].id;
    }
  });

  // Closing clears the query, so reopening starts on the whole page rather
  // than on whatever was being hunted for last time.
  $effect(() => {
    if (!isOpen) query = "";
  });

  // The theme preference is a snapshot, and ⌘-shortcuts and the native View
  // menu change it from outside this modal. Re-reading on open keeps the
  // segment from reporting a choice the app is no longer honouring — a
  // control that reads as selected while something else is in force is the
  // same failure as a setting that does nothing.
  $effect(() => {
    if (isOpen) themePreference = themeStore.preference();
  });

  /**
   * Arrow-key movement for the category rail.
   *
   * Bound to each tab rather than the tablist: the tabs are what take focus
   * (the container never does, per the APG roving-tabindex pattern), and an
   * interactive-role container carrying a key handler it cannot receive is
   * exactly what svelte-check's `a11y_interactive_supports_focus` flags.
   *
   * The rail has always been a `tablist` of `tab`s; without this it was one
   * in name only — every category took a Tab stop and none responded to the
   * arrow keys the role promises. Roving `tabindex` makes the group a single
   * stop, and the keys move between its members.
   */
  function onRailKeydown(event: KeyboardEvent) {
    const keys = ["ArrowDown", "ArrowUp", "Home", "End"];
    if (!keys.includes(event.key)) return;
    const index = railSections.findIndex((entry) => entry.id === activeSection);
    if (index < 0) return;
    const last = railSections.length - 1;
    const next =
      event.key === "ArrowDown"
        ? index === last
          ? 0
          : index + 1
        : event.key === "ArrowUp"
          ? index === 0
            ? last
            : index - 1
          : event.key === "Home"
            ? 0
            : last;
    event.preventDefault();
    activeSection = railSections[next].id;
    // Selection follows focus, so the focused tab has to move with it or the
    // next arrow press would jump back to where the DOM focus was left.
    railEl?.querySelector<HTMLElement>(`#settings-tab-${railSections[next].id}`)?.focus();
  }

  /** Result of the most recent manual check; null until one is pressed. */
  let updateStatus = $state<UpdateStatus | null>(null);
  let updateUrl = $state("");
  let checkingUpdate = $state(false);
  let mcpInfo = $state<McpInfo | null>(null);
  let mcpError = $state<string | null>(null);
  let mcpCopied = $state("");
  let mcpCopyTimer: ReturnType<typeof setTimeout> | undefined;

  let hiddenViews = $derived($interfaceStore.hiddenViews);

  $effect(() => {
    if (!isOpen) return;
    void loadMcpInfo();
  });

  $effect(() => () => {
    if (mcpCopyTimer) clearTimeout(mcpCopyTimer);
  });

  async function loadMcpInfo() {
    try {
      mcpInfo = await getMcpInfo();
      mcpError = null;
    } catch (error) {
      mcpInfo = null;
      mcpError = formatError(error);
    }
  }

  async function copyMcp(key: string, text: string) {
    if (!text) return;
    if (await copyText(text)) {
      mcpCopied = key;
      if (mcpCopyTimer) clearTimeout(mcpCopyTimer);
      mcpCopyTimer = setTimeout(() => (mcpCopied = ""), 1500);
    }
  }

  async function runManualUpdateCheck() {
    if (checkingUpdate) return;
    checkingUpdate = true;
    updateStatus = null;
    try {
      const result = await checkForAppUpdate();
      updateStatus = describeUpdateCheck(result);
      // Only offer the link when the check actually ran; a failed check has
      // nothing to point at beyond the generic releases page.
      updateUrl = result.checked ? result.releaseUrl : "";
      if (result.checked && result.updateAvailable) {
        // A version the user has now seen here should not also nag on the
        // next launch.
        interfaceStore.dismissUpdateVersion(result.latestVersion);
      }
    } finally {
      checkingUpdate = false;
    }
  }

  async function openReleasePage() {
    try {
      await openExternal(updateUrl);
    } catch (error) {
      updateStatus = { kind: "failed", message: formatError(error) };
    }
  }

  /**
   * Restores every preference this modal owns, across all three stores, so
   * "defaults" means what it says rather than "the ones on this panel".
   */
  async function restoreDefaults() {
    const confirmed = await askConfirm({
      title: "Restore default settings?",
      message:
        "Appearance (theme, accent, scale, motion, timestamps), layout, view visibility, graph, diff and analysis preferences all go back to their defaults. Repositories, tabs and history are untouched.",
      confirmLabel: "Restore defaults",
    });
    if (!confirmed) return;
    interfaceStore.reset();
    densityStore.setDensity("spacious");
    setTheme("system");
  }

  const SECTION_ICONS: Record<SettingsSectionId, typeof Settings> = {
    appearance: Palette,
    layout: PanelsTopLeft,
    views: Eye,
    graph: GitBranch,
    diff: FileCode,
    analysis: FlaskConical,
    agents: Plug,
    updates: RefreshCw,
  };

  const THEME_OPTIONS: readonly { value: ThemePreference; label: string; title: string }[] = [
    { value: "system", label: "System", title: "Follow the operating system appearance" },
    { value: "light", label: "Light", title: "Always use the light theme" },
    { value: "dark", label: "Dark", title: "Always use the dark theme" },
  ];

  const DENSITY_OPTIONS: readonly { value: DensityMode; label: string; title: string }[] = [
    {
      value: "spacious",
      label: "Spacious",
      title: "Spacious branch spacing (keeps adjacent lanes visually separated)",
    },
    {
      value: "compact",
      label: "Compact",
      title: "Compact branch spacing (fits more history on screen)",
    },
  ];

  const GRAPH_WIDTH_OPTIONS: readonly { value: GraphWidthMode; label: string; title: string }[] = [
    { value: "balanced", label: "Balanced", title: "Balanced width keeps commit messages prominent" },
    { value: "wide", label: "Wide", title: "Wide graph viewport with more visible branch lanes" },
    { value: "full", label: "Full", title: "Use all safe graph space while preserving commit details" },
  ];

  const REF_SCOPE_OPTIONS: readonly { value: RefScope; label: string; title: string }[] = [
    {
      value: "named",
      label: "Named refs",
      title:
        "Branches, remote-tracking branches, tags and HEAD — every lane carries a name you can read",
    },
    {
      value: "all",
      label: "All refs",
      title:
        "Also walk custom namespaces (agent checkpoints, prefetch mirrors, CI pull refs); they are labelled by their full ref path",
    },
  ];

  const STATUS_BAR_OPTIONS: readonly { value: StatusBarMode; label: string; title: string }[] = [
    { value: "full", label: "Full", title: "Branch, changes, commit cadence and shortcut hints" },
    { value: "minimal", label: "Compact", title: "Branch and anything needing attention only" },
    { value: "hidden", label: "Hidden", title: "No status bar unless something needs attention" },
  ];

  const DIAGNOSTICS_OPTIONS: readonly {
    value: DiagnosticsButtonMode;
    label: string;
    title: string;
  }[] = [
    { value: "always", label: "Always", title: "Keep the diagnostics button in the header" },
    {
      value: "issues",
      label: "When recorded",
      title: "Show it only once an error or warning has been recorded",
    },
  ];

  const TIMESTAMP_OPTIONS: readonly {
    value: TimestampStyle;
    label: string;
    title: string;
  }[] = [
    { value: "relative", label: "Relative", title: "How long ago, e.g. 3d ago" },
    {
      value: "absolute",
      label: "Date",
      title: "Calendar date as YYYY-MM-DD — fixed width and the same in every locale",
    },
  ];

  const DIFF_LAYOUT_OPTIONS: readonly { value: DiffLayout; label: string; title: string }[] = [
    {
      value: "unified",
      label: "Unified",
      title: "One column of +/- rows; best for reading long lines",
    },
    {
      value: "split",
      label: "Split",
      title: "Old beside new; best for comparing two versions side by side",
    },
  ];

  const TAB_WIDTH_OPTIONS: readonly { value: string; label: string; title: string }[] =
    TAB_WIDTHS.map((width) => ({
      value: String(width),
      label: String(width),
      title: `Render a tab as ${width} columns`,
    }));

  /**
   * A fixed instant and a fixed "now" three days after it, so the preview
   * beside the control shows what each style looks like rather than drifting
   * with the clock (and rendering "13mo ago" a year from now).
   */
  const TIMESTAMP_SAMPLE = 1_756_000_000;
  const TIMESTAMP_SAMPLE_NOW = TIMESTAMP_SAMPLE + 3 * 86_400;
</script>

{#if isOpen}
  <div
    role="dialog"
    aria-modal="true"
    aria-labelledby="settings-modal-title"
    tabindex="-1"
    onclick={(e) => e.target === e.currentTarget && onClose?.()}
    onkeydown={(e) => e.key === "Escape" && onClose?.()}
    in:fade={backdropFade()}
    out:fade={backdropFadeOut()}
    class="gp-scrim bg-black/40 flex items-center justify-center p-4 select-none gp-gpu"
    style="z-index: {LAYERS.MODAL}"
  >
    <div
      use:trapFocus
      in:scale={cardScale()}
      out:scale={cardScaleOut()}
      class="w-full max-w-3xl h-136 max-h-[calc(100vh-2rem)] min-h-0 gp-card shadow-float rounded-2xl overflow-hidden flex flex-col font-sans text-xs gp-gpu"
    >
      <div class="p-4 border-b border-border/60 gp-section-edge flex items-center justify-between gap-3 shrink-0">
        <div
          id="settings-modal-title"
          class="flex items-center gap-2 text-sm font-semibold text-textPrimary shrink-0"
        >
          <Settings size={16} class="text-accent" />
          <span>Settings</span>
        </div>

        <!-- Search narrows the rail AND the rows inside each panel, so a hit
             is visible without hunting down the category it landed in. -->
        <div class="relative min-w-0 flex-1 max-w-60">
          <Search
            size={12}
            class="pointer-events-none absolute left-2 top-1/2 -translate-y-1/2 text-textMuted"
          />
          <input
            type="search"
            bind:value={query}
            placeholder="Search settings"
            aria-label="Search settings"
            class="w-full rounded-lg border border-border/70 bg-surfaceHover/40 py-1 pl-6 pr-6 text-[11px] text-textPrimary placeholder:text-textMuted focus:border-accent/60 focus:outline-hidden"
          />
          {#if query}
            <button
              type="button"
              onclick={() => (query = "")}
              aria-label="Clear settings search"
              class="absolute right-1.5 top-1/2 -translate-y-1/2 rounded p-0.5 text-textMuted hover:text-textPrimary"
            >
              <X size={11} />
            </button>
          {/if}
        </div>
      </div>

      <!-- Category rail + panel. Both scroll independently so a long panel
           can never push the rail (or the footer) out of a 900x600 window. -->
      <div class="flex flex-1 min-h-0">
        <div
          bind:this={railEl}
          role="tablist"
          aria-label="Settings sections"
          aria-orientation="vertical"
          class="w-36 shrink-0 overflow-y-auto border-r border-border/60 bg-surfaceHover/30 p-2 space-y-0.5"
        >
          {#each railSections as entry (entry.id)}
            {@const Icon = SECTION_ICONS[entry.id]}
            {@const active = activeSection === entry.id}
            <button
              type="button"
              role="tab"
              id="settings-tab-{entry.id}"
              aria-selected={active}
              aria-controls="settings-panel-{entry.id}"
              tabindex={active ? 0 : -1}
              onclick={() => (activeSection = entry.id)}
              onkeydown={onRailKeydown}
              class="w-full flex items-center gap-2 rounded-lg px-2 py-1.5 text-left transition-colors duration-100 {active
                ? 'bg-surface text-accent font-semibold shadow-xs'
                : 'text-textMuted hover:text-textPrimary hover:bg-surface/60'}"
            >
              <Icon size={13} class="shrink-0" />
              <span class="truncate">{entry.label}</span>
            </button>
          {/each}
          {#if noMatches}
            <p class="px-2 py-1.5 text-[10px] leading-snug text-textMuted">
              No setting matches that.
            </p>
          {/if}
        </div>

        <div class="min-h-0 flex-1 overflow-y-auto min-w-0 p-4">
          {#if noMatches}
            <p class="text-textMuted text-[11px] leading-snug" role="status">
              Nothing on this page matches <span class="text-textPrimary">“{query}”</span>.
              Try a shorter term, or clear the search to see every category again.
            </p>
          {/if}
          {#each SETTINGS_SECTIONS as entry (entry.id)}
            <div
              id="settings-panel-{entry.id}"
              role="tabpanel"
              aria-labelledby="settings-tab-{entry.id}"
              hidden={noMatches || activeSection !== entry.id}
            >
              <h2 class="text-textPrimary text-sm font-semibold">{entry.label}</h2>
              <p class="text-textMuted text-[10px] leading-snug mt-0.5 mb-3">{entry.summary}</p>

              {#if entry.id === "appearance"}
                <div class="space-y-4">
                  <div data-setting="theme" hidden={!shown("theme")}>
                    <div class="text-textMuted text-[10px] mb-1.5">Theme</div>
                    <SettingSegment
                      ariaLabel="Theme appearance"
                      options={THEME_OPTIONS}
                      value={themePreference}
                      onselect={setTheme}
                    />
                  </div>

                  <div data-setting="accent" hidden={!shown("accent")}>
                    <div class="text-textMuted text-[10px] mb-1.5">Accent colour</div>
                    <!-- `group` + `aria-pressed`, not `radiogroup` + `radio`,
                         matching every other single-choice control on this
                         page (SettingSegment). A radiogroup would promise
                         arrow-key movement and one tab stop, which is a
                         second keyboard contract to honour for no gain over
                         the pattern the rest of the modal already uses. -->
                    <div class="flex flex-wrap gap-1.5" role="group" aria-label="Accent colour">
                      {#each ACCENTS as accent (accent.id)}
                        {@const picked = $interfaceStore.accent === accent.id}
                        <button
                          type="button"
                          aria-pressed={picked}
                          aria-label="{accent.label} accent"
                          title="{accent.label} accent"
                          onclick={() => interfaceStore.setAccent(accent.id)}
                          class="h-6 w-6 rounded-full border transition-transform {picked
                            ? 'border-textPrimary scale-110'
                            : 'border-border/70 hover:scale-105'}"
                          style="background-color: {accentSwatch(accent.id, $themeStore)}"
                        >
                          {#if picked}
                            <Check size={12} class="mx-auto text-white drop-shadow" />
                          {/if}
                        </button>
                      {/each}
                    </div>
                    <p class="text-textMuted text-[10px] leading-snug mt-1.5">
                      Tints selection, focus rings and the commit graph's current-row
                      marker. Every choice carries a separate light and dark shade,
                      each checked against WCAG AA, so switching theme never leaves the
                      accent unreadable.
                    </p>
                  </div>

                  <div data-setting="ui-scale" hidden={!shown("ui-scale")}>
                    <div class="flex items-center justify-between text-[11px] mb-1">
                      <span class="text-textPrimary font-medium">UI Font Scale</span>
                      <span class="font-mono text-accent font-semibold"
                        >{Math.round($interfaceStore.uiFontScale * 100)}%</span
                      >
                    </div>
                    <div class="flex items-center gap-2">
                      <button
                        type="button"
                        onclick={() => interfaceStore.zoomOut()}
                        class="gp-btn py-0.5! px-2! text-xs"
                        title="Zoom Out (⌘-)">-</button
                      >
                      <input
                        type="range"
                        min="0.75"
                        max="1.4"
                        step="0.05"
                        value={$interfaceStore.uiFontScale}
                        oninput={(e) => interfaceStore.setFontScale(parseFloat(e.currentTarget.value))}
                        class="flex-1"
                        aria-label="UI Font Scale Slider"
                      />
                      <button
                        type="button"
                        onclick={() => interfaceStore.zoomIn()}
                        class="gp-btn py-0.5! px-2! text-xs"
                        title="Zoom In (⌘+)">+</button
                      >
                      <button
                        type="button"
                        onclick={() => interfaceStore.resetZoom()}
                        class="gp-btn py-0.5! px-2! text-[10px]"
                        title="Reset Zoom (⌘0)">Reset</button
                      >
                    </div>
                  </div>

                  <div data-setting="motion" hidden={!shown("motion")}>
                    <SettingToggle
                      label="Reduce motion"
                      description="Drops view entrances, modal scaling, the theme crossfade and spinner rotation. Spinners keep their glyph, so nothing stops telling you it is working."
                      ariaLabel="Reduce interface motion"
                      checked={$interfaceStore.reduceMotion}
                      onchange={(next) => interfaceStore.setReduceMotion(next)}
                    />
                    <p class="text-textMuted text-[10px] leading-snug">
                      Leaving this off follows the system setting. It can only add
                      reduction: if the operating system already asks for less motion,
                      GitPulse honours that whatever this says.
                    </p>
                  </div>

                  <div data-setting="timestamps" hidden={!shown("timestamps")}>
                    <div class="flex items-center justify-between mb-1.5">
                      <span class="text-textMuted text-[10px]">Timestamps</span>
                      <span class="text-textMuted text-[10px] font-mono">
                        {formatTimestamp(
                          TIMESTAMP_SAMPLE,
                          $interfaceStore.timestampStyle,
                          TIMESTAMP_SAMPLE_NOW,
                        )}
                      </span>
                    </div>
                    <SettingSegment
                      ariaLabel="Timestamp style"
                      options={TIMESTAMP_OPTIONS}
                      value={$interfaceStore.timestampStyle}
                      onselect={(style) => interfaceStore.setTimestampStyle(style)}
                    />
                    <p class="text-textMuted text-[10px] leading-snug mt-1.5">
                      Applies to the commit list, branch tooltips, the diff's change
                      picker, Work and the stack. Hovering a commit time in the list
                      shows the other form, and the graph's node card keeps showing both
                      whichever way this is set.
                    </p>
                  </div>

                  <div
                    data-setting="coach-marks"
                    hidden={!shown("coach-marks")}
                    class="flex items-center justify-between gap-3"
                  >
                    <div class="min-w-0">
                      <div class="text-textPrimary text-[11px] font-medium">First-run coach marks</div>
                      <div class="text-textMuted text-[10px] leading-snug">
                        Bring back the one-time tips shown on a fresh install.
                      </div>
                    </div>
                    <button
                      type="button"
                      onclick={() => interfaceStore.resetCoachMarks()}
                      class="gp-btn py-0.5! px-2.5! text-[11px] shrink-0"
                    >
                      Reset Tips
                    </button>
                  </div>
                </div>
              {:else if entry.id === "layout"}
                <div class="space-y-4">
                  <div data-setting="status-icon" hidden={!shown("status-icon")}>
                    <SettingToggle
                      label="Menu bar status icon"
                      description="Show repository status while GitPulse is in the background. Closing the window hides it; use Quit to exit."
                      ariaLabel="Show menu bar status icon"
                      checked={$interfaceStore.showStatusIcon}
                      onchange={(next) => interfaceStore.setShowStatusIcon(next)}
                    />
                  </div>
                  <div data-setting="status-bar" hidden={!shown("status-bar")}>
                    <div class="text-textMuted text-[10px] mb-1.5">Status bar</div>
                    <SettingSegment
                      ariaLabel="Status bar detail"
                      options={STATUS_BAR_OPTIONS}
                      value={$interfaceStore.statusBarMode}
                      onselect={(mode) => interfaceStore.setStatusBarMode(mode)}
                    />
                    <p class="text-textMuted text-[10px] leading-snug mt-1.5">
                      A hidden bar still comes back for a parked merge or rebase, unresolved
                      conflicts, or a stalled file watcher — decluttering never costs you a
                      warning.
                    </p>
                  </div>

                  <div data-setting="diagnostics-button" hidden={!shown("diagnostics-button")}>
                    <div class="text-textMuted text-[10px] mb-1.5">Diagnostics button</div>
                    <SettingSegment
                      ariaLabel="Diagnostics button visibility"
                      options={DIAGNOSTICS_OPTIONS}
                      value={$interfaceStore.diagnosticsButton}
                      onselect={(mode) => interfaceStore.setDiagnosticsButton(mode)}
                    />
                  </div>

                  <div class="gp-separator" aria-hidden="true"></div>
                  <div class="space-y-0.5 pt-1">
                    <div data-setting="header-labels" hidden={!shown("header-labels")}>
                      <SettingToggle
                        label="Header button labels"
                        description="Words beside the Open and Clone icons in the title bar."
                        ariaLabel="Show labels on header action buttons"
                        checked={$interfaceStore.showHeaderActionLabels}
                        onchange={(next) => interfaceStore.setShowHeaderActionLabels(next)}
                      />
                    </div>
                    <div data-setting="repo-tabs" hidden={!shown("repo-tabs")}>
                      <SettingToggle
                        label="Hide repository tabs when alone"
                        description="Drops the tab strip while only one repository is open."
                        ariaLabel="Hide the repository tab strip while a single repository is open"
                        checked={$interfaceStore.autoHideRepoTabs}
                        onchange={(next) => interfaceStore.setAutoHideRepoTabs(next)}
                      />
                    </div>
                    <div data-setting="language-bar" hidden={!shown("language-bar")}>
                      <SettingToggle
                        label="Language mix"
                        description="Dominant language in the status bar; click it for the full breakdown."
                        ariaLabel="Show the language mix in the status bar"
                        checked={$interfaceStore.showLanguageBar}
                        onchange={(next) => interfaceStore.setShowLanguageBar(next)}
                      />
                    </div>
                    <div data-setting="harness-badges" hidden={!shown("harness-badges")}>
                      <SettingToggle
                        label="MANVI status badges"
                        description="Harness and local-model chips in the toolbar."
                        ariaLabel="Show MANVI status badges"
                        checked={$interfaceStore.showHarnessBadges}
                        onchange={(next) => interfaceStore.setShowHarnessBadges(next)}
                      />
                    </div>
                  </div>
                </div>
              {:else if entry.id === "views"}
                <div class="space-y-3" data-setting="view-visibility">
                  <p class="text-textMuted text-[10px] leading-snug">
                    Unchecking a view removes it from the header only. It stays reachable from
                    the command palette (⌘K) and the View menu, the view you are currently in
                    always shows, and Work reappears on its own while conflicts are
                    unresolved — that is where Resolve lives.
                  </p>

                  <!-- One flat list: the header is four tabs, so grouping
                       them under "header tabs" / "header menu" headings would
                       be describing a distinction that no longer exists. -->
                  <div class="grid grid-cols-2 gap-x-3 gap-y-0.5">
                    {#each VIEW_NAV as item (item.id)}
                      {@const shown = !hiddenViews.includes(item.id)}
                      <label
                        class="flex items-center gap-2 rounded-lg px-1.5 py-1 text-[11px] text-textPrimary hover:bg-surfaceHover/60 cursor-pointer"
                      >
                        <input
                          type="checkbox"
                          class="accent-accent"
                          checked={shown}
                          aria-label="Show {item.label} in the header"
                          onchange={(e) =>
                            interfaceStore.setViewHidden(
                              item.id,
                              !e.currentTarget.checked,
                            )}
                        />
                        <span class="truncate">{item.label}</span>
                      </label>
                    {/each}
                  </div>

                  <div class="gp-separator" aria-hidden="true"></div>
                  <div class="flex items-center justify-between gap-3 pt-1">
                    <span class="text-textMuted text-[10px]">
                      {hiddenViews.length === 0
                        ? "Every view is listed in the header."
                        : `${hiddenViews.length} view${hiddenViews.length === 1 ? "" : "s"} hidden from the header.`}
                    </span>
                    <button
                      type="button"
                      onclick={() => interfaceStore.showAllViews()}
                      disabled={hiddenViews.length === 0}
                      class="gp-btn py-0.5! px-2.5! text-[11px] shrink-0"
                    >
                      Show all
                    </button>
                  </div>
                </div>
              {:else if entry.id === "graph"}
                <div class="space-y-4">
                  <div data-setting="branch-spacing" hidden={!shown("branch-spacing")}>
                    <div class="text-textMuted text-[10px] mb-1.5">Branch spacing</div>
                    <SettingSegment
                      ariaLabel="Branch spacing"
                      options={DENSITY_OPTIONS}
                      value={$densityStore}
                      onselect={(mode) => densityStore.setDensity(mode)}
                    />
                  </div>
                  <div data-setting="graph-width" hidden={!shown("graph-width")}>
                    <div class="text-textMuted text-[10px] mb-1.5">Graph width</div>
                    <SettingSegment
                      ariaLabel="Graph width"
                      options={GRAPH_WIDTH_OPTIONS}
                      value={$interfaceStore.graphWidthMode}
                      onselect={(mode) => interfaceStore.setGraphWidthMode(mode)}
                    />
                  </div>
                  <div data-setting="ref-scope" hidden={!shown("ref-scope")}>
                    <div class="text-textMuted text-[10px] mb-1.5">Refs drawn</div>
                    <SettingSegment
                      ariaLabel="Refs drawn"
                      options={REF_SCOPE_OPTIONS}
                      value={$interfaceStore.graphRefScope}
                      onselect={(scope) => interfaceStore.setGraphRefScope(scope)}
                    />
                    <p class="text-textMuted text-[10px] leading-snug mt-1.5">
                      Namespaces outside branches, remotes and tags — agent turn
                      checkpoints, prefetch mirrors, CI pull refs — can add dozens of
                      lanes nothing in the UI can name. They are left out by default,
                      and whatever is left out is named above the graph rather than
                      silently dropped.
                    </p>
                  </div>
                  <div
                    data-setting="graph-avatars"
                    hidden={!shown("graph-avatars")}
                    class="pt-1"
                  >
                    <div class="gp-separator" aria-hidden="true"></div>
                    <SettingToggle
                      label="Author avatars"
                      description="Initial badges beside the branch lanes."
                      ariaLabel="Show author avatars in the commit graph"
                      checked={$interfaceStore.showGraphAvatars}
                      onchange={(next) => interfaceStore.setShowGraphAvatars(next)}
                    />
                  </div>
                </div>
              {:else if entry.id === "diff"}
                <div class="space-y-4">
                  <p class="text-textMuted text-[10px] leading-snug">
                    The first three set what a diff <em>opens</em> as. The toolbar above
                    each diff still switches the file in front of you; what changes here
                    is where it starts, which used to reset on every file.
                  </p>

                  <div data-setting="diff-layout" hidden={!shown("diff-layout")}>
                    <div class="text-textMuted text-[10px] mb-1.5">Default layout</div>
                    <SettingSegment
                      ariaLabel="Default diff layout"
                      options={DIFF_LAYOUT_OPTIONS}
                      value={$interfaceStore.diffLayout}
                      onselect={(layout) => interfaceStore.setDiffLayout(layout)}
                    />
                  </div>

                  <div class="space-y-0.5">
                    <div data-setting="diff-wrap" hidden={!shown("diff-wrap")}>
                      <SettingToggle
                        label="Wrap long lines"
                        description="Reflows instead of scrolling sideways. Wrapping is only offered on diffs under 4,000 rows — beyond that the rows stay fixed-height so the list can stay windowed."
                        ariaLabel="Open diffs with word wrap on"
                        checked={$interfaceStore.diffWordWrap}
                        onchange={(next) => interfaceStore.setDiffWordWrap(next)}
                      />
                    </div>
                    <div data-setting="diff-syntax" hidden={!shown("diff-syntax")}>
                      <SettingToggle
                        label="Syntax highlighting"
                        description="Language colouring inside diff rows. Turning it off makes very large diffs cheaper to scroll."
                        ariaLabel="Open diffs with syntax highlighting on"
                        checked={$interfaceStore.diffSyntaxHighlight}
                        onchange={(next) => interfaceStore.setDiffSyntaxHighlight(next)}
                      />
                    </div>
                    <div data-setting="diff-whitespace" hidden={!shown("diff-whitespace")}>
                      <SettingToggle
                        label="Ignore whitespace"
                        description="Hides whitespace-only changes in newly opened repositories. Staging still applies the real bytes, so a hidden change is hidden from reading, never from the commit."
                        ariaLabel="Open repositories with whitespace-only changes ignored"
                        checked={$interfaceStore.diffIgnoreWhitespace}
                        onchange={(next) => interfaceStore.setDiffIgnoreWhitespace(next)}
                      />
                    </div>
                  </div>

                  <div
                    data-setting="tab-width"
                    hidden={!shown("tab-width")}
                    class="pt-1"
                  >
                    <div class="gp-separator" aria-hidden="true"></div>
                    <div class="text-textMuted text-[10px] mb-1.5">Tab width</div>
                    <SettingSegment
                      ariaLabel="Tab width"
                      options={TAB_WIDTH_OPTIONS}
                      value={String($interfaceStore.tabWidth)}
                      onselect={(width) =>
                        interfaceStore.setTabWidth(Number(width) as TabWidth)}
                    />
                    <p class="text-textMuted text-[10px] leading-snug mt-1.5">
                      Columns a literal tab advances to, in the diff, the file viewer,
                      blame and the conflict editor. Display only — GitPulse never
                      rewrites the bytes in the file.
                    </p>
                  </div>
                </div>
              {:else if entry.id === "analysis"}
                <div data-setting="auto-coverage">
                  <SettingToggle
                    label="Generate coverage automatically"
                    description="Off by default. When on, opening a repository with missing coverage runs its test suites once per session — minutes of CPU on a large project — and writes coverage artifacts into the working tree. A run that only completes because files were excluded is always labelled, never reported as a clean result."
                    ariaLabel="Automatically generate coverage for repositories that have none"
                    checked={$interfaceStore.autoRunCoverage}
                    onchange={(next) => interfaceStore.setAutoRunCoverage(next)}
                  />
                </div>
              {:else if entry.id === "agents"}
                <div data-setting="mcp-plugin" hidden={!shown("mcp-plugin")}>
                  <p class="text-textMuted text-[10px] leading-snug mb-2">
                    Agents connect through the native Codex plugin package
                    (`.codex-plugin/plugin.json` + `.mcp.json`) and speak MCP 2026-07-28. The
                    surface is read-only: it never checks out a branch or writes a file.
                  </p>
                  {#if mcpError}
                    <div
                      class="flex items-start gap-1.5 text-amber-600 dark:text-amber-400 text-[10px] mb-2"
                    >
                      <AlertTriangle size={12} class="shrink-0 mt-px" />
                      <span>{mcpError}</span>
                    </div>
                  {:else if mcpInfo}
                    <div class="space-y-1.5 rounded-xl border border-border/70 bg-surfaceHover/40 p-2.5">
                      <div class="flex items-start gap-2">
                        <Plug size={13} class="text-accent shrink-0 mt-0.5" />
                        <div class="min-w-0">
                          <div class="text-textPrimary text-[11px] font-medium">
                            {mcpInfo.server_name} · MCP {mcpInfo.protocol_version}
                          </div>
                          <div class="text-textMuted text-[10px] font-mono break-all">
                            {#if mcpInfo.binary_found}
                              {mcpInfo.binary_path}
                            {:else}
                              {mcpInfo.binary_error}
                            {/if}
                          </div>
                          <div class="text-textMuted text-[10px] mt-1">
                            {#if mcpInfo.plugin_found}
                              Plugin: {mcpInfo.plugin_path}
                            {:else}
                              {mcpInfo.plugin_error}
                            {/if}
                          </div>
                        </div>
                      </div>
                      <div class="flex flex-wrap gap-1.5 pt-1">
                        <button
                          type="button"
                          class="gp-btn py-0.5! px-2! text-[10px] inline-flex items-center gap-1"
                          disabled={!mcpInfo.plugin_manifest_json}
                          onclick={() => void copyMcp("plugin", mcpInfo?.plugin_manifest_json ?? "")}
                        >
                          {#if mcpCopied === "plugin"}<Check size={10} />{:else}<Copy size={10} />{/if}
                          .codex-plugin/plugin.json
                        </button>
                        <button
                          type="button"
                          class="gp-btn py-0.5! px-2! text-[10px] inline-flex items-center gap-1"
                          disabled={!mcpInfo.plugin_mcp_json}
                          onclick={() => void copyMcp("mcp", mcpInfo?.plugin_mcp_json ?? "")}
                        >
                          {#if mcpCopied === "mcp"}<Check size={10} />{:else}<Copy size={10} />{/if}
                          .mcp.json
                        </button>
                      </div>
                      {#if mcpInfo.tools.length > 0}
                        <details class="pt-1">
                          <summary class="cursor-pointer text-[10px] text-textMuted">
                            {mcpInfo.tools.length} tools (start with gitpulse_insights)
                          </summary>
                          <ul class="mt-1 space-y-0.5 max-h-32 overflow-y-auto">
                            {#each mcpInfo.tools as tool, i (`${tool.name}#${i}`)}
                              <li class="font-mono text-[10px] text-textPrimary">
                                {tool.name}
                                <span class="text-textMuted font-sans"> — {tool.title}</span>
                              </li>
                            {/each}
                          </ul>
                        </details>
                      {/if}
                    </div>
                  {/if}
                </div>
                <div data-setting="external-tools" hidden={!shown("external-tools")} class="mt-3 space-y-1.5">
                  <h4 class="text-[10px] font-bold uppercase tracking-wider text-textMuted">
                    External CLIs
                  </h4>
                  <p class="text-textMuted text-[10px] leading-snug">
                    Install or update <span class="font-mono">devmap</span> (Code → Map) and
                    <span class="font-mono">manvi</span> (policy harness) via the install ladder:
                    PATH → prebuilt release → cargo/go install → local checkout. Env vars still win
                    over saved config.
                  </p>
                  <button
                    type="button"
                    class="gp-btn text-[11px] px-2 py-0.5"
                    onclick={() => window.dispatchEvent(new CustomEvent("gitpulse:setup-tools"))}
                  >
                    Run setup
                  </button>
                  <ExternalToolsPanel />
                </div>
              {:else if entry.id === "updates"}
                <div class="space-y-3">
                  <div data-setting="update-check" hidden={!shown("update-check")}>
                    <SettingToggle
                      label="Check for new releases"
                      description="Off by default. When on, GitPulse contacts its own public repository at most once a day to compare release tags. It never downloads or installs anything."
                      ariaLabel="Automatically check for new GitPulse releases"
                      checked={$interfaceStore.checkForUpdates}
                      onchange={(next) => interfaceStore.setCheckForUpdates(next)}
                    />
                  </div>

                  <div
                    data-setting="update-now"
                    hidden={!shown("update-now")}
                    class="pt-1 flex items-center justify-between gap-3"
                  >
                    <span class="text-textMuted text-[11px]">Check now</span>
                    <button
                      type="button"
                      onclick={runManualUpdateCheck}
                      disabled={checkingUpdate}
                      class="gp-btn py-0.5! px-2.5! text-[11px] flex items-center gap-1.5"
                    >
                      <RefreshCw size={11} class={checkingUpdate ? "animate-spin" : ""} />
                      {checkingUpdate ? "Checking…" : "Check"}
                    </button>
                  </div>

                  {#if updateStatus}
                    <div
                      class="text-[10px] leading-snug {updateStatus.kind === 'available'
                        ? 'text-accent'
                        : updateStatus.kind === 'failed'
                          ? 'text-red-400'
                          : 'text-textMuted'}"
                      role="status"
                    >
                      {updateStatus.message}
                      {#if updateStatus.kind === "available" && updateUrl}
                        <button
                          type="button"
                          onclick={openReleasePage}
                          class="underline underline-offset-2 hover:text-textPrimary ml-1"
                        >
                          View release
                        </button>
                      {/if}
                    </div>
                  {/if}
                </div>
              {/if}
            </div>
          {/each}
        </div>
      </div>

      <div
        class="p-4 border-t border-border/60 gp-section-edge bg-surfaceHover/30 flex items-center justify-between gap-2 shrink-0"
      >
        <button
          type="button"
          onclick={restoreDefaults}
          class="gp-btn py-0.5! px-2.5! text-[11px] flex items-center gap-1.5"
          title="Restore every setting on this page to its default"
        >
          <RotateCcw size={11} />
          <span>Restore defaults</span>
        </button>
        <button onclick={onClose} class="gp-btn">Done</button>
      </div>
    </div>
  </div>
{/if}
