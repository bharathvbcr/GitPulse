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
    FlaskConical,
    RefreshCw,
    Plug,
    Copy,
    Check,
    AlertTriangle,
    RotateCcw,
  } from "lucide-svelte";
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
  import { VIEW_NAV } from "../views/viewNav";
  import type { GraphWidthMode } from "../graph/graphLayout";
  import type { RefScope } from "../graph/refScope";
  import type { StatusBarMode } from "../ui/statusBarMode";
  import type { DiagnosticsButtonMode } from "../ui/diagnosticsButton";
  import SettingToggle from "./SettingToggle.svelte";
  import SettingSegment from "./SettingSegment.svelte";

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
        "Appearance, layout, view visibility, graph and analysis preferences all go back to their defaults. Repositories, tabs and history are untouched.",
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
    class="fixed inset-0 bg-black/40 backdrop-blur-sm flex items-center justify-center p-4 select-none gp-gpu"
    style="z-index: {LAYERS.MODAL}"
  >
    <div
      use:trapFocus
      in:scale={cardScale()}
      out:scale={cardScaleOut()}
      class="w-full max-w-3xl h-[34rem] max-h-[calc(100vh-2rem)] min-h-0 gp-card shadow-float rounded-2xl overflow-hidden flex flex-col font-sans text-xs gp-gpu"
    >
      <div class="p-4 border-b border-border/60 flex items-center justify-between shrink-0">
        <div
          id="settings-modal-title"
          class="flex items-center gap-2 text-sm font-semibold text-textPrimary"
        >
          <Settings size={16} class="text-accent" />
          <span>Settings</span>
        </div>
      </div>

      <!-- Category rail + panel. Both scroll independently so a long panel
           can never push the rail (or the footer) out of a 900x600 window. -->
      <div class="flex flex-1 min-h-0">
        <div
          role="tablist"
          aria-label="Settings sections"
          aria-orientation="vertical"
          class="w-36 shrink-0 overflow-y-auto border-r border-border/60 bg-surfaceHover/30 p-2 space-y-0.5"
        >
          {#each SETTINGS_SECTIONS as entry (entry.id)}
            {@const Icon = SECTION_ICONS[entry.id]}
            {@const active = activeSection === entry.id}
            <button
              type="button"
              role="tab"
              id="settings-tab-{entry.id}"
              aria-selected={active}
              aria-controls="settings-panel-{entry.id}"
              onclick={() => (activeSection = entry.id)}
              class="w-full flex items-center gap-2 rounded-lg px-2 py-1.5 text-left transition-colors duration-100 {active
                ? 'bg-surface text-accent font-semibold shadow-sm'
                : 'text-textMuted hover:text-textPrimary hover:bg-surface/60'}"
            >
              <Icon size={13} class="shrink-0" />
              <span class="truncate">{entry.label}</span>
            </button>
          {/each}
        </div>

        <div class="min-h-0 flex-1 overflow-y-auto min-w-0 p-4">
          {#each SETTINGS_SECTIONS as entry (entry.id)}
            <div
              id="settings-panel-{entry.id}"
              role="tabpanel"
              aria-labelledby="settings-tab-{entry.id}"
              hidden={activeSection !== entry.id}
            >
              <h2 class="text-textPrimary text-sm font-semibold">{entry.label}</h2>
              <p class="text-textMuted text-[10px] leading-snug mt-0.5 mb-3">{entry.summary}</p>

              {#if entry.id === "appearance"}
                <div class="space-y-4">
                  <div>
                    <div class="text-textMuted text-[10px] mb-1.5">Theme</div>
                    <SettingSegment
                      ariaLabel="Theme appearance"
                      options={THEME_OPTIONS}
                      value={themePreference}
                      onselect={setTheme}
                    />
                  </div>

                  <div>
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
                        class="gp-btn !py-0.5 !px-2 text-xs"
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
                        class="gp-btn !py-0.5 !px-2 text-xs"
                        title="Zoom In (⌘+)">+</button
                      >
                      <button
                        type="button"
                        onclick={() => interfaceStore.resetZoom()}
                        class="gp-btn !py-0.5 !px-2 text-[10px]"
                        title="Reset Zoom (⌘0)">Reset</button
                      >
                    </div>
                  </div>

                  <div class="flex items-center justify-between gap-3">
                    <div class="min-w-0">
                      <div class="text-textPrimary text-[11px] font-medium">First-run coach marks</div>
                      <div class="text-textMuted text-[10px] leading-snug">
                        Bring back the one-time tips shown on a fresh install.
                      </div>
                    </div>
                    <button
                      type="button"
                      onclick={() => interfaceStore.resetCoachMarks()}
                      class="gp-btn !py-0.5 !px-2.5 text-[11px] shrink-0"
                    >
                      Reset Tips
                    </button>
                  </div>
                </div>
              {:else if entry.id === "layout"}
                <div class="space-y-4">
                  <div>
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

                  <div>
                    <div class="text-textMuted text-[10px] mb-1.5">Diagnostics button</div>
                    <SettingSegment
                      ariaLabel="Diagnostics button visibility"
                      options={DIAGNOSTICS_OPTIONS}
                      value={$interfaceStore.diagnosticsButton}
                      onselect={(mode) => interfaceStore.setDiagnosticsButton(mode)}
                    />
                  </div>

                  <div class="space-y-0.5 pt-1 border-t border-border/50">
                    <SettingToggle
                      label="Header button labels"
                      description="Words beside the Open and Clone icons in the title bar."
                      ariaLabel="Show labels on header action buttons"
                      checked={$interfaceStore.showHeaderActionLabels}
                      onchange={(next) => interfaceStore.setShowHeaderActionLabels(next)}
                    />
                    <SettingToggle
                      label="Hide repository tabs when alone"
                      description="Drops the tab strip while only one repository is open."
                      ariaLabel="Hide the repository tab strip while a single repository is open"
                      checked={$interfaceStore.autoHideRepoTabs}
                      onchange={(next) => interfaceStore.setAutoHideRepoTabs(next)}
                    />
                    <SettingToggle
                      label="Language mix"
                      description="Dominant language in the status bar; click it for the full breakdown."
                      ariaLabel="Show the language mix in the status bar"
                      checked={$interfaceStore.showLanguageBar}
                      onchange={(next) => interfaceStore.setShowLanguageBar(next)}
                    />
                    <SettingToggle
                      label="MANVI status badges"
                      description="Harness and local-model chips in the toolbar."
                      ariaLabel="Show MANVI status badges"
                      checked={$interfaceStore.showHarnessBadges}
                      onchange={(next) => interfaceStore.setShowHarnessBadges(next)}
                    />
                  </div>
                </div>
              {:else if entry.id === "views"}
                <div class="space-y-3">
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

                  <div class="flex items-center justify-between gap-3 pt-1 border-t border-border/50">
                    <span class="text-textMuted text-[10px]">
                      {hiddenViews.length === 0
                        ? "Every view is listed in the header."
                        : `${hiddenViews.length} view${hiddenViews.length === 1 ? "" : "s"} hidden from the header.`}
                    </span>
                    <button
                      type="button"
                      onclick={() => interfaceStore.showAllViews()}
                      disabled={hiddenViews.length === 0}
                      class="gp-btn !py-0.5 !px-2.5 text-[11px] shrink-0"
                    >
                      Show all
                    </button>
                  </div>
                </div>
              {:else if entry.id === "graph"}
                <div class="space-y-4">
                  <div>
                    <div class="text-textMuted text-[10px] mb-1.5">Branch spacing</div>
                    <SettingSegment
                      ariaLabel="Branch spacing"
                      options={DENSITY_OPTIONS}
                      value={$densityStore}
                      onselect={(mode) => densityStore.setDensity(mode)}
                    />
                  </div>
                  <div>
                    <div class="text-textMuted text-[10px] mb-1.5">Graph width</div>
                    <SettingSegment
                      ariaLabel="Graph width"
                      options={GRAPH_WIDTH_OPTIONS}
                      value={$interfaceStore.graphWidthMode}
                      onselect={(mode) => interfaceStore.setGraphWidthMode(mode)}
                    />
                  </div>
                  <div>
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
                      and whatever is left out is reported in Diagnostics rather than
                      silently dropped.
                    </p>
                  </div>
                  <div class="pt-1 border-t border-border/50">
                    <SettingToggle
                      label="Author avatars"
                      description="Initial badges beside the branch lanes."
                      ariaLabel="Show author avatars in the commit graph"
                      checked={$interfaceStore.showGraphAvatars}
                      onchange={(next) => interfaceStore.setShowGraphAvatars(next)}
                    />
                  </div>
                </div>
              {:else if entry.id === "analysis"}
                <SettingToggle
                  label="Generate coverage automatically"
                  description="Off by default. When on, opening a repository with missing coverage runs its test suites once per session — minutes of CPU on a large project — and writes coverage artifacts into the working tree. A run that only completes because files were excluded is always labelled, never reported as a clean result."
                  ariaLabel="Automatically generate coverage for repositories that have none"
                  checked={$interfaceStore.autoRunCoverage}
                  onchange={(next) => interfaceStore.setAutoRunCoverage(next)}
                />
              {:else if entry.id === "agents"}
                <div>
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
                          class="gp-btn !py-0.5 !px-2 text-[10px] inline-flex items-center gap-1"
                          disabled={!mcpInfo.plugin_manifest_json}
                          onclick={() => void copyMcp("plugin", mcpInfo?.plugin_manifest_json ?? "")}
                        >
                          {#if mcpCopied === "plugin"}<Check size={10} />{:else}<Copy size={10} />{/if}
                          .codex-plugin/plugin.json
                        </button>
                        <button
                          type="button"
                          class="gp-btn !py-0.5 !px-2 text-[10px] inline-flex items-center gap-1"
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
                            {#each mcpInfo.tools as tool (tool.name)}
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
              {:else if entry.id === "updates"}
                <div class="space-y-3">
                  <SettingToggle
                    label="Check for new releases"
                    description="Off by default. When on, GitPulse contacts its own public repository at most once a day to compare release tags. It never downloads or installs anything."
                    ariaLabel="Automatically check for new GitPulse releases"
                    checked={$interfaceStore.checkForUpdates}
                    onchange={(next) => interfaceStore.setCheckForUpdates(next)}
                  />

                  <div class="pt-1 flex items-center justify-between gap-3">
                    <span class="text-textMuted text-[11px]">Check now</span>
                    <button
                      type="button"
                      onclick={runManualUpdateCheck}
                      disabled={checkingUpdate}
                      class="gp-btn !py-0.5 !px-2.5 text-[11px] flex items-center gap-1.5"
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
        class="p-4 border-t border-border/60 bg-surfaceHover/30 flex items-center justify-between gap-2 shrink-0"
      >
        <button
          type="button"
          onclick={restoreDefaults}
          class="gp-btn !py-0.5 !px-2.5 text-[11px] flex items-center gap-1.5"
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
