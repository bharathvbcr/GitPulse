<script lang="ts">
  import AppearanceSettings from "../src/lib/components/AppearanceSettings.svelte";
  import { themeStore, type ThemePreference } from "../src/lib/stores/themeStore";

  /**
   * Settings -> Appearance on its own, as the modal renders it, with the
   * search filter and tips status driven from the page. `window.__gpHide` is
   * the set of ids a search has filtered out; `window.__gpFiltering` is
   * whether a search is active.
   */
  let themePreference = $state<ThemePreference>(themeStore.preference());
  let hidden = $state<string[]>([]);
  let filtering = $state(false);
  let tips = $state("");
  let tours = $state(0);

  const bridge = globalThis as Record<string, unknown>;
  bridge.__gpHide = (ids: string[]) => (hidden = ids);
  bridge.__gpFiltering = (on: boolean) => (filtering = on);
  bridge.__gpTours = () => tours;
</script>

<div class="gp-shell gp-card" style="width: 560px; margin: 16px; padding: 16px">
  <AppearanceSettings
    shown={(id) => !hidden.includes(id)}
    {filtering}
    {themePreference}
    onTheme={(next) => {
      themePreference = next;
      themeStore.setPreference(next);
    }}
    tipsStatus={tips}
    onResetTips={() => (tips = "Tips restored")}
    onReplayTour={() => (tours += 1)}
  />
</div>

<!-- Probes the checks read computed style from: a menu and a float over live
     content, and a panel that wears the material ladder's surface fill. -->
<div id="probe-menu" class="gp-menu" style="position: fixed; left: 600px; top: 20px; width: 120px; height: 40px"></div>
<div id="probe-float" class="shadow-float" style="position: fixed; left: 600px; top: 80px; width: 120px; height: 40px"></div>
<div id="probe-surface" class="bg-surface" style="position: fixed; left: 600px; top: 140px; width: 120px; height: 40px"></div>
<div id="probe-radius" style="position: fixed; left: 600px; top: 200px; width: 40px; height: 40px; border-radius: var(--gp-r-md)"></div>
