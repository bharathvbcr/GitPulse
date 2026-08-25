<script lang="ts">
  import { fade, scale } from "svelte/transition";
  import { themeStore, type ThemePreference } from "../stores/themeStore";
  import { densityStore } from "../stores/densityStore";
  import { interfaceStore } from "../stores/interfaceStore";
  import { fadeParams, scaleParams } from "../motion/easing";
  import { trapFocus } from "../ui/focusTrap";
  import { LAYERS } from "../ui/layers";
  import { Settings, Monitor, Sun, Moon, Rows3, Languages, ShieldCheck } from "lucide-svelte";

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
    transition:fade={fadeParams()}
    class="fixed inset-0 bg-black/40 backdrop-blur-sm flex items-center justify-center p-4 select-none gp-gpu"
    style="z-index: {LAYERS.MODAL}"
  >
    <!-- svelte-ignore a11y_click_events_have_key_events -->
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div
      use:trapFocus
      onclick={(e) => e.stopPropagation()}
      in:scale={scaleParams()}
      out:scale={scaleParams()}
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
      </div>

      <div class="p-4 border-t border-border/60 bg-surfaceHover/30 flex justify-end gap-2">
        <button onclick={onClose} class="gp-btn">Done</button>
      </div>
    </div>
  </div>
{/if}
