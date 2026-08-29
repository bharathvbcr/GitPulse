<script lang="ts">
  import { fade, scale } from "svelte/transition";
  import { themeStore, type ThemePreference } from "../stores/themeStore";
  import { densityStore } from "../stores/densityStore";
  import { interfaceStore } from "../stores/interfaceStore";
  import {
    backdropFade,
    backdropFadeOut,
    cardScale,
    cardScaleOut,
  } from "../ui/transitions";
  import { trapFocus } from "../ui/focusTrap";
  import { LAYERS } from "../ui/layers";
  import { Settings, Monitor, Sun, Moon, Rows3, Languages, ShieldCheck, CircleUserRound, RefreshCw } from "lucide-svelte";
  import {
    checkForAppUpdate,
    describeUpdateCheck,
    type UpdateStatus,
  } from "../updates/updateCheck";
  import { openExternal } from "../desktop/openExternal";
  import { formatError } from "../ui/formatError";

  let {
    isOpen = false,
    onClose,
  }: {
    isOpen?: boolean;
    onClose?: () => void;
  } = $props();

  let themePreference = $state<ThemePreference>(themeStore.preference());

  function setTheme(preference: ThemePreference) {
    themePreference = preference;
    themeStore.setPreference(preference);
  }

  /** Result of the most recent manual check; null until one is pressed. */
  let updateStatus = $state<UpdateStatus | null>(null);
  let updateUrl = $state("");
  let checkingUpdate = $state(false);

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

  const THEME_OPTIONS: Array<{
    value: ThemePreference;
    icon: typeof Monitor;
    label: string;
  }> = [
    { value: "system", icon: Monitor, label: "System" },
    { value: "light", icon: Sun, label: "Light" },
    { value: "dark", icon: Moon, label: "Dark" },
  ];
</script>

{#if isOpen}
  <div
    role="dialog"
    aria-modal="true"
    aria-label="Settings"
    tabindex="-1"
    onclick={onClose}
    onkeydown={(e) => e.key === "Escape" && onClose?.()}
    in:fade={backdropFade()}
    out:fade={backdropFadeOut()}
    class="fixed inset-0 bg-black/40 backdrop-blur-sm flex items-center justify-center p-4 select-none gp-gpu"
    style="z-index: {LAYERS.MODAL}"
  >
    <!-- svelte-ignore a11y_click_events_have_key_events -->
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div
      use:trapFocus
      onclick={(e) => e.stopPropagation()}
      in:scale={cardScale()}
      out:scale={cardScaleOut()}
      class="w-full max-w-md gp-card shadow-float rounded-2xl overflow-hidden flex flex-col font-sans text-xs gp-gpu"
    >
      <div class="p-4 border-b border-border/60 flex items-center justify-between">
        <div class="flex items-center gap-2 text-sm font-semibold text-textPrimary">
          <Settings size={16} class="text-accent" />
          <span>Settings</span>
        </div>
      </div>

      <div class="p-4 space-y-4">
        <section>
          <h2 class="text-textMuted text-[11px] font-semibold uppercase tracking-wider mb-2">Appearance</h2>
          <div class="gp-segmented" role="group" aria-label="Theme appearance">
            {#each THEME_OPTIONS as option (option.value)}
              {@const Icon = option.icon}
              <button
                type="button"
                onclick={() => setTheme(option.value)}
                aria-pressed={themePreference === option.value}
                data-active={themePreference === option.value ? "true" : "false"}
                class="gp-seg-btn flex items-center gap-1.5"
                title="{option.label} appearance"
              >
                <Icon size={12} />
                <span>{option.label}</span>
              </button>
            {/each}
          </div>
        </section>

        <section>
          <h2 class="text-textMuted text-[11px] font-semibold uppercase tracking-wider mb-2">Commit Graph</h2>
          <div class="gp-segmented" role="group" aria-label="Branch spacing">
            <button
              type="button"
              onclick={() => densityStore.setDensity("spacious")}
              aria-pressed={$densityStore === "spacious"}
              data-active={$densityStore === "spacious" ? "true" : "false"}
              class="gp-seg-btn flex items-center gap-1.5"
              title="Spacious branch spacing (keeps adjacent lanes visually separated)"
            >
              <Rows3 size={12} />
              <span>Spacious</span>
            </button>
            <button
              type="button"
              onclick={() => densityStore.setDensity("compact")}
              aria-pressed={$densityStore === "compact"}
              data-active={$densityStore === "compact" ? "true" : "false"}
              class="gp-seg-btn flex items-center gap-1.5"
              title="Compact branch spacing (fits more history on screen)"
            >
              <Rows3 size={12} />
              <span>Compact</span>
            </button>
          </div>
          <div class="text-textMuted text-[10px] mt-2 mb-1">Graph width</div>
          <div class="gp-segmented" role="group" aria-label="Graph width">
            <button
              type="button"
              onclick={() => interfaceStore.setGraphWidthMode("balanced")}
              aria-pressed={$interfaceStore.graphWidthMode === "balanced"}
              data-active={$interfaceStore.graphWidthMode === "balanced" ? "true" : "false"}
              class="gp-seg-btn"
              title="Balanced width keeps commit messages prominent"
            >
              Balanced
            </button>
            <button
              type="button"
              onclick={() => interfaceStore.setGraphWidthMode("wide")}
              aria-pressed={$interfaceStore.graphWidthMode === "wide"}
              data-active={$interfaceStore.graphWidthMode === "wide" ? "true" : "false"}
              class="gp-seg-btn"
              title="Wide graph viewport with more visible branch lanes"
            >
              Wide
            </button>
            <button
              type="button"
              onclick={() => interfaceStore.setGraphWidthMode("full")}
              aria-pressed={$interfaceStore.graphWidthMode === "full"}
              data-active={$interfaceStore.graphWidthMode === "full" ? "true" : "false"}
              class="gp-seg-btn"
              title="Use all safe graph space while preserving commit details"
            >
              Full
            </button>
          </div>
          <div class="flex items-center justify-between gap-3 py-1 mt-2">
            <div class="flex items-center gap-2 min-w-0">
              <CircleUserRound size={13} class="text-textMuted shrink-0" />
              <div class="min-w-0">
                <div class="text-textPrimary">Author avatars</div>
                <div class="text-textMuted text-[10px]">Initial badges beside the branch lanes</div>
              </div>
            </div>
            <button
              type="button"
              role="switch"
              aria-checked={$interfaceStore.showGraphAvatars}
              aria-label="Show author avatars in the commit graph"
              onclick={() => interfaceStore.setShowGraphAvatars(!$interfaceStore.showGraphAvatars)}
              class="relative w-8 h-[18px] rounded-full transition-colors shrink-0 {$interfaceStore
                .showGraphAvatars
                ? 'bg-accent'
                : 'bg-border'}"
            >
              <span
                class="absolute top-[2px] left-[2px] w-[14px] h-[14px] rounded-full bg-white shadow-sm transition-transform {$interfaceStore
                  .showGraphAvatars
                  ? 'translate-x-[14px]'
                  : ''}"
              ></span>
            </button>
          </div>
        </section>

        <section>
          <h2 class="text-textMuted text-[11px] font-semibold uppercase tracking-wider mb-2">Interface</h2>
          <div class="space-y-1">
            <div class="flex items-center justify-between gap-3 py-1">
              <div class="flex items-center gap-2 min-w-0">
                <Languages size={13} class="text-textMuted shrink-0" />
                <div class="min-w-0">
                  <div class="text-textPrimary">Language statistics bar</div>
                  <div class="text-textMuted text-[10px]">Per-language breakdown above the commit list</div>
                </div>
              </div>
              <button
                type="button"
                role="switch"
                aria-checked={$interfaceStore.showLanguageBar}
                aria-label="Show language statistics bar"
                onclick={() => interfaceStore.setShowLanguageBar(!$interfaceStore.showLanguageBar)}
                class="relative w-8 h-[18px] rounded-full transition-colors shrink-0 {$interfaceStore
                  .showLanguageBar
                  ? 'bg-accent'
                  : 'bg-border'}"
              >
                <span
                  class="absolute top-[2px] left-[2px] w-[14px] h-[14px] rounded-full bg-white shadow-sm transition-transform {$interfaceStore
                    .showLanguageBar
                    ? 'translate-x-[14px]'
                    : ''}"
                ></span>
              </button>
            </div>

            <div class="flex items-center justify-between gap-3 py-1">
              <div class="flex items-center gap-2 min-w-0">
                <ShieldCheck size={13} class="text-textMuted shrink-0" />
                <div class="min-w-0">
                  <div class="text-textPrimary">MANVI status badges</div>
                  <div class="text-textMuted text-[10px]">Harness and local-model chips in the toolbar</div>
                </div>
              </div>
              <button
                type="button"
                role="switch"
                aria-checked={$interfaceStore.showHarnessBadges}
                aria-label="Show MANVI status badges"
                onclick={() => interfaceStore.setShowHarnessBadges(!$interfaceStore.showHarnessBadges)}
                class="relative w-8 h-[18px] rounded-full transition-colors shrink-0 {$interfaceStore
                  .showHarnessBadges
                  ? 'bg-accent'
                  : 'bg-border'}"
              >
                <span
                  class="absolute top-[2px] left-[2px] w-[14px] h-[14px] rounded-full bg-white shadow-sm transition-transform {$interfaceStore
                    .showHarnessBadges
                    ? 'translate-x-[14px]'
                    : ''}"
                ></span>
              </button>
            </div>
          </div>
        </section>

        <section>
          <h2 class="text-textMuted text-[11px] font-semibold uppercase tracking-wider mb-2">Zoom & Guidance</h2>
          <div class="space-y-3">
            <div>
              <div class="flex items-center justify-between text-[11px] mb-1">
                <span class="text-textPrimary font-medium">UI Font Scale</span>
                <span class="font-mono text-accent font-semibold">{Math.round($interfaceStore.uiFontScale * 100)}%</span>
              </div>
              <div class="flex items-center gap-2">
                <button
                  type="button"
                  onclick={() => interfaceStore.zoomOut()}
                  class="gp-btn !py-0.5 !px-2 text-xs"
                  title="Zoom Out (⌘-)"
                >-</button>
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
                  title="Zoom In (⌘+)"
                >+</button>
                <button
                  type="button"
                  onclick={() => interfaceStore.resetZoom()}
                  class="gp-btn !py-0.5 !px-2 text-[10px]"
                  title="Reset Zoom (⌘0)"
                >Reset</button>
              </div>
            </div>

            <div class="pt-1 flex items-center justify-between">
              <span class="text-textMuted text-[11px]">First-run coach marks</span>
              <button
                type="button"
                onclick={() => interfaceStore.resetCoachMarks()}
                class="gp-btn !py-0.5 !px-2.5 text-[11px]"
              >
                Reset Tips
              </button>
            </div>
          </div>
        </section>

        <section>
          <h2 class="text-textMuted text-[11px] font-semibold uppercase tracking-wider mb-2">Updates</h2>
          <div class="space-y-3">
            <div class="flex items-center justify-between gap-3">
              <div class="min-w-0">
                <div class="text-textPrimary text-[11px] font-medium">Check for new releases</div>
                <div class="text-textMuted text-[10px] leading-snug">
                  Off by default. When on, GitPulse contacts its own public repository
                  at most once a day to compare release tags. It never downloads or
                  installs anything.
                </div>
              </div>
              <button
                type="button"
                role="switch"
                aria-checked={$interfaceStore.checkForUpdates}
                aria-label="Automatically check for new GitPulse releases"
                onclick={() => interfaceStore.setCheckForUpdates(!$interfaceStore.checkForUpdates)}
                class="relative w-8 h-[18px] rounded-full transition-colors shrink-0 {$interfaceStore
                  .checkForUpdates
                  ? 'bg-accent'
                  : 'bg-border'}"
              >
                <span
                  class="absolute top-[2px] left-[2px] w-[14px] h-[14px] rounded-full bg-white shadow-sm transition-transform {$interfaceStore
                    .checkForUpdates
                    ? 'translate-x-[14px]'
                    : 'translate-x-0'}"
                ></span>
              </button>
            </div>

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
        </section>
      </div>

      <div class="p-4 border-t border-border/60 bg-surfaceHover/30 flex justify-end gap-2">
        <button onclick={onClose} class="gp-btn">Done</button>
      </div>
    </div>
  </div>
{/if}
