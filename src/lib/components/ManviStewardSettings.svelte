<script lang="ts">
  /**
   * Periodic Manvi system steward configuration (`src-tauri/src/harness/steward.rs`),
   * combining host-wide defaults with per-repository overrides in Settings -> Agents.
   */
  import { RefreshCw, ShieldAlert, ShieldCheck, Gauge } from "@lucide/svelte";
  import { explainError } from "../workbench/client";
  import { redactDiagnosticText } from "../diagnostics/diagnostics";
  import { manviFocusHint, manviSectionId } from "../ui/manviFocus";
  import { openManviFocus } from "../ui/openManviFocus";
  import { loadOnce } from "../settings/loadOnce";
  import BrandMark from "./BrandMark.svelte";
  import SettingToggle from "./SettingToggle.svelte";
  import SettingSegment from "./SettingSegment.svelte";
  import {
    manviSteward,
    refreshManviSteward,
    runManviStewardNow,
    saveManviStewardRepoOverride,
    saveManviStewardSettings,
  } from "../stores/stewardStore";
  import type { ManviStewardSettings as StewardSettingsModel, RepoStewardOverride } from "../steward/types";

  let {
    active = true,
    repoPath = null,
  }: {
    active?: boolean;
    repoPath?: string | null;
  } = $props();

  const INTERVAL_CHOICES: readonly { value: string; label: string; title: string }[] = [
    { value: "120", label: "2m", title: "Check host pressure every 2 minutes" },
    { value: "180", label: "3m", title: "Check host pressure every 3 minutes" },
    { value: "300", label: "5m", title: "Check host pressure every 5 minutes" },
    { value: "600", label: "10m", title: "Check host pressure every 10 minutes" },
    { value: "900", label: "15m", title: "Check host pressure every 15 minutes" },
  ];

  const RETENTION_CHOICES: readonly { value: string; label: string; title: string }[] = [
    { value: "3", label: "3d", title: "Keep recent build outputs newer than 3 days" },
    { value: "7", label: "7d", title: "Keep recent build outputs newer than 7 days" },
    { value: "14", label: "14d", title: "Keep recent build outputs newer than 14 days" },
    { value: "30", label: "30d", title: "Keep recent build outputs newer than 30 days" },
  ];

  const SCOPE_CHOICES: readonly { value: "global" | "custom"; label: string; title: string }[] = [
    { value: "global", label: "Use global", title: "Follow the global Manvi steward settings for this repository" },
    { value: "custom", label: "Override repository", title: "Set custom steward switches for this repository and all its worktrees" },
  ];

  let busy = $state(false);
  let runningCheck = $state(false);
  let readOk = $state(false);
  let error = $state("");
  let loadedRepoPath = $state<string | null | undefined>(undefined);

  const view = $derived($manviSteward);
  const settings = $derived(view.settings);
  const repoCommonDir = $derived(view.repo_common_dir);
  const currentRepoOverride = $derived<RepoStewardOverride | undefined>(
    repoCommonDir ? settings.repositories[repoCommonDir] : undefined,
  );
  const hasRepoOverride = $derived(view.effective.overridden && currentRepoOverride !== undefined);
  const latestTick = $derived(view.history.length > 0 ? view.history[0] : null);
  type Choice = { value: string; label: string; title: string };
  /** The presets, plus the stored value when it is none of them (a hand-edited
   *  tools.json), so the control shows what is in force rather than a preset. */
  function withStored(choices: readonly Choice[], stored: Choice): readonly Choice[] {
    return choices.some((c) => c.value === stored.value) ? choices : [...choices, stored];
  }
  const intervalChoices = $derived.by(() => {
    const secs = settings.interval_secs;
    const label = secs % 60 === 0 ? `${secs / 60}m` : `${secs}s`;
    return withStored(INTERVAL_CHOICES, {
      value: String(secs),
      label,
      title: `Check host pressure every ${label} (set outside these choices)`,
    });
  });
  const intervalValue = $derived(String(settings.interval_secs));
  const retentionChoices = $derived.by(() => {
    const days = settings.clean_retention_days;
    return withStored(RETENTION_CHOICES, {
      value: String(days),
      label: `${days}d`,
      title: `Keep recent build outputs newer than ${days} days (set outside these choices)`,
    });
  });
  const retentionValue = $derived(String(settings.clean_retention_days));

  function formatBytes(bytes: number): string {
    if (bytes >= 1024 * 1024 * 1024) {
      return `${(bytes / (1024 * 1024 * 1024)).toFixed(1)} GiB`;
    }
    if (bytes >= 1024 * 1024) {
      return `${(bytes / (1024 * 1024)).toFixed(1)} MiB`;
    }
    if (bytes >= 1024) {
      return `${(bytes / 1024).toFixed(0)} KiB`;
    }
    return `${bytes} B`;
  }

  function humanizeToken(value: string): string {
    return value.replaceAll("_", " ").trim();
  }

  function formatTimestamp(ms: number | null): string {
    if (ms === null || ms <= 0) return "Not checked yet";
    return new Date(ms).toLocaleTimeString([], {
      hour: "2-digit",
      minute: "2-digit",
      second: "2-digit",
    });
  }

  async function load() {
    busy = true;
    error = "";
    try {
      await refreshManviSteward(repoPath, { preserveRepoContext: false });
      loadedRepoPath = repoPath;
      readOk = true;
    } catch (cause) {
      error = explainError(cause);
    } finally {
      busy = false;
    }
  }

  async function saveGlobal(patch: Partial<StewardSettingsModel>) {
    if (busy || !readOk) return;
    busy = true;
    error = "";
    try {
      await saveManviStewardSettings({ ...settings, ...patch }, repoPath, {
        preserveRepoContext: false,
      });
    } catch (cause) {
      error = explainError(cause);
    } finally {
      busy = false;
    }
  }

  async function setRepoScope(mode: "global" | "custom") {
    if (busy || !readOk || !repoPath) return;
    busy = true;
    error = "";
    try {
      if (mode === "global") {
        await saveManviStewardRepoOverride(repoPath, null);
      } else {
        await saveManviStewardRepoOverride(repoPath, {
          enabled: currentRepoOverride?.enabled ?? settings.enabled,
          message_agents: currentRepoOverride?.message_agents ?? settings.message_agents,
          slow_critical: currentRepoOverride?.slow_critical ?? settings.slow_critical,
          clean_critical: currentRepoOverride?.clean_critical ?? settings.clean_critical,
        });
      }
    } catch (cause) {
      error = explainError(cause);
    } finally {
      busy = false;
    }
  }

  async function updateRepoOverride(patch: Partial<RepoStewardOverride>) {
    if (busy || !readOk || !repoPath) return;
    busy = true;
    error = "";
    try {
      const next: RepoStewardOverride = {
        enabled: currentRepoOverride?.enabled ?? settings.enabled,
        message_agents: currentRepoOverride?.message_agents ?? settings.message_agents,
        slow_critical: currentRepoOverride?.slow_critical ?? settings.slow_critical,
        clean_critical: currentRepoOverride?.clean_critical ?? settings.clean_critical,
        ...patch,
      };
      await saveManviStewardRepoOverride(repoPath, next);
    } catch (cause) {
      error = explainError(cause);
    } finally {
      busy = false;
    }
  }

  async function runCheckNow() {
    if (busy || runningCheck) return;
    runningCheck = true;
    error = "";
    try {
      await runManviStewardNow(repoPath, { preserveRepoContext: false });
      readOk = true;
    } catch (cause) {
      error = explainError(cause);
    } finally {
      runningCheck = false;
    }
  }

  const loader = loadOnce(load);
  $effect(() => {
    if (active) loader.request();
  });

  $effect(() => {
    if (active && readOk && repoPath !== loadedRepoPath && !busy) {
      void load();
    }
  });
</script>

<section id={manviSectionId("steward")} tabindex="-1" class="gp-card p-4 space-y-3 text-meta" data-testid="manvi-steward-settings">
  <div class="flex items-center justify-between gap-2">
    <div class="flex items-center gap-2 min-w-0">
      <BrandMark mark="manvi" size={16} class="shrink-0" />
      <h3 class="gp-label">Manvi system steward</h3>
    </div>
    <button
      type="button"
      class="gp-btn inline-flex items-center gap-1.5"
      data-size="sm"
      data-testid="manvi-steward-run-now"
      disabled={busy || runningCheck || view.running}
      onclick={() => void runCheckNow()}
      title="Run one Manvi steward check immediately"
    >
      <RefreshCw size={12} class={runningCheck || view.running ? "animate-spin" : ""} />
      <span>Run check</span>
    </button>
  </div>

  <p class="text-textMuted text-micro leading-snug">
    Uses your configured Manvi model (cloud or local) every few minutes to watch memory pressure,
    CPU load, thermal state and free disk space. When agent sessions ignore normal guards, the
    steward messages active sessions, enforces critical slowdowns, and reclaims leftover build
    caches in trusted repositories.
  </p>

  {#if view.manvi_state === "ready"}
    <div class="gp-notice" data-tone="ok" data-testid="manvi-steward-model-ready">
      <ShieldCheck size={14} class="mt-0.5 shrink-0" style="color: var(--gp-notice-ink)" />
      <div class="min-w-0 flex-1 space-y-0.5">
        <div class="font-medium">
          Manvi active ({view.manvi_mode === "cloud" ? "cloud" : "local"} · {view.manvi_model})
        </div>
        {#if view.manvi_detail}
          <p class="text-textMuted text-micro break-all">{redactDiagnosticText(view.manvi_detail)}</p>
        {/if}
      </div>
    </div>
  {:else if view.manvi_state === "failed" || view.manvi_state === "deferred" || view.manvi_state === "unchecked"}
    <div class="gp-notice" data-tone="unknown" data-testid="manvi-steward-model-unknown">
      <ShieldAlert size={14} class="mt-0.5 shrink-0" style="color: var(--gp-notice-ink)" />
      <div class="min-w-0 flex-1 space-y-1">
        <div class="font-medium">
          {view.manvi_state === "unchecked"
            ? "Manvi model not checked while steward is off"
            : "Couldn't check Manvi model right now"}
        </div>
        <p class="text-textMuted text-micro leading-relaxed">
          {view.manvi_detail
            ? redactDiagnosticText(view.manvi_detail)
            : "Couldn't query Manvi right now; press Run check once host load settles."}
        </p>
      </div>
    </div>
  {:else if view.manvi_state === "not_installed" || view.manvi_state === "needs_update"}
    <div class="gp-notice" data-tone="warn" data-testid="manvi-steward-model-unavailable">
      <ShieldAlert size={14} class="mt-0.5 shrink-0" style="color: var(--gp-notice-ink)" />
      <div class="min-w-0 flex-1 space-y-1">
        <div class="font-medium">Manvi unavailable</div>
        <p class="text-textMuted text-micro leading-relaxed">
          {view.manvi_detail
            ? redactDiagnosticText(view.manvi_detail)
            : "Install or update Manvi so the system steward can monitor this computer."}
        </p>
      </div>
    </div>
  {:else}
    <div class="gp-notice" data-tone="warn" data-testid="manvi-steward-model-missing">
      <ShieldAlert size={14} class="mt-0.5 shrink-0" style="color: var(--gp-notice-ink)" />
      <div class="min-w-0 flex-1 space-y-1.5">
        <div class="font-medium">Manvi model not configured yet</div>
        <p class="text-textMuted text-micro leading-relaxed">
          {view.manvi_detail
            ? redactDiagnosticText(view.manvi_detail)
            : "Choose a cloud Claude model or a local model server for Manvi so the steward can monitor this computer."}
        </p>
        <button
          type="button"
          class="gp-btn"
          data-size="sm"
          data-testid="manvi-steward-choose-model"
          title={manviFocusHint("model")}
          onclick={() => openManviFocus("model")}
        >
          Choose Manvi model
        </button>
      </div>
    </div>
  {/if}

  {#if view.env_override}
    <p class="text-warn text-micro" data-testid="manvi-steward-env-override">
      {redactDiagnosticText(view.env_override)}
    </p>
  {/if}

  {#if view.critical_throttle_active}
    <div class="gp-notice" data-tone="warn" data-testid="manvi-steward-critical-banner">
      <Gauge size={14} class="mt-0.5 shrink-0" style="color: var(--gp-notice-ink)" />
      <div class="min-w-0 flex-1">
        <div class="font-medium">
          Critical slowdown active · {view.throttled_count} background agent worker processes throttled
        </div>
        <p class="text-textMuted text-micro">
          Heavy build commands are refused and rapid shell calls are paced until severe host pressure clears.
        </p>
      </div>
    </div>
  {/if}

  <div class="space-y-1">
    <SettingToggle
      label="Enable Manvi system steward (global default)"
      description="On by default once Manvi has a configured cloud model (explicit cloud selection, ~/.anthropic-accounts, or a saved Keychain Claude key) or local model. Runs on the interval below and wakes early when host load enters severe pressure."
      checked={settings.enabled}
      disabled={busy || !readOk}
      onchange={(next) => void saveGlobal({ enabled: next })}
    />

    <div class="space-y-1 pt-1" data-testid="manvi-steward-interval">
      <p class="text-textPrimary">Check interval</p>
      <SettingSegment
        ariaLabel="How often Manvi checks system pressure"
        options={intervalChoices}
        value={intervalValue}
        size="sm"
        disabled={busy || !readOk}
        onselect={(next) => {
          if (readOk && !busy) void saveGlobal({ interval_secs: Number(next) });
        }}
      />
    </div>

    <SettingToggle
      label="Message active agent sessions under load"
      description="Sends Manvi's operational directive to active sessions in trusted repositories when host pressure is elevated or severe, delivered on the next tool call without waking idle sessions."
      checked={settings.message_agents}
      disabled={busy || !readOk || !settings.enabled}
      onchange={(next) => void saveGlobal({ message_agents: next })}
    />

    <SettingToggle
      label="Slow agents when host load is critical"
      description="Enforces command-gate refusals on heavy builds even when Advise is selected, paces rapid shell loops, and lowers OS scheduler and disk priority on agent worker processes until critical load clears."
      checked={settings.slow_critical}
      disabled={busy || !readOk || !settings.enabled}
      onchange={(next) => void saveGlobal({ slow_critical: next })}
    />

    <SettingToggle
      label="Clean leftover Cargo and build caches when storage is critical"
      description="Reclaims producer-verified ignored build directories, idle worktree Cargo targets, leftover agent Cargo scratch builds (in trusted repositories and finished system-temp session scratchpads), and prunable shared Go, npm, uv, and pnpm caches (preserving Cargo registry downloads). When Manvi cloud is configured, it evaluates the candidate inventory and selects idle items to reclaim (including recent idle scratch builds when disk space is critical). Under severe memory pressure, local-model calls are skipped so loading weights never worsens RAM usage."
      checked={settings.clean_critical}
      disabled={busy || !readOk || !settings.enabled}
      onchange={(next) => void saveGlobal({ clean_critical: next })}
    />

    <div class="space-y-1 pt-1" data-testid="manvi-steward-retention">
      <p class="text-textPrimary">Minimum cache age before critical cleanup</p>
      <SettingSegment
        ariaLabel="Minimum age in days before critical storage cleanup removes build caches"
        options={retentionChoices}
        value={retentionValue}
        size="sm"
        disabled={busy || !readOk}
        onselect={(next) => {
          if (readOk && !busy) void saveGlobal({ clean_retention_days: Number(next) });
        }}
      />
    </div>
  </div>

  {#if repoPath && repoCommonDir}
    <div class="rounded-xl border border-edge bg-surfaceHover/40 p-3 space-y-2" data-testid="manvi-steward-repo-scope">
      <div class="flex flex-wrap items-center justify-between gap-2">
        <span class="gp-label">Current repository scope</span>
        <SettingSegment
          ariaLabel="Whether this repository uses global steward defaults or an override"
          options={SCOPE_CHOICES}
          value={hasRepoOverride ? "custom" : "global"}
          size="sm"
          disabled={busy || !readOk}
          onselect={(mode) => void setRepoScope(mode)}
        />
      </div>
      <p class="text-textMuted text-micro break-all">
        Applies across every worktree sharing <span class="font-mono">{repoCommonDir}</span>.
      </p>
      {#if hasRepoOverride && currentRepoOverride}
        <div class="space-y-1 pt-1">
          <SettingToggle
            label="Enable steward in this repository"
            checked={currentRepoOverride.enabled ?? settings.enabled}
            disabled={busy || !readOk}
            onchange={(next) => void updateRepoOverride({ enabled: next })}
          />
          <SettingToggle
            label="Message active sessions in this repository"
            checked={currentRepoOverride.message_agents ?? settings.message_agents}
            disabled={busy || !readOk || !(currentRepoOverride.enabled ?? settings.enabled)}
            onchange={(next) => void updateRepoOverride({ message_agents: next })}
          />
          <SettingToggle
            label="Slow critical load in this repository"
            checked={currentRepoOverride.slow_critical ?? settings.slow_critical}
            disabled={busy || !readOk || !(currentRepoOverride.enabled ?? settings.enabled)}
            onchange={(next) => void updateRepoOverride({ slow_critical: next })}
          />
          <SettingToggle
            label="Clean critical storage in this repository"
            checked={currentRepoOverride.clean_critical ?? settings.clean_critical}
            disabled={busy || !readOk || !(currentRepoOverride.enabled ?? settings.enabled)}
            onchange={(next) => void updateRepoOverride({ clean_critical: next })}
          />
        </div>
      {/if}
    </div>
  {/if}

  {#if latestTick}
    <div class="rounded-xl border border-edge bg-surfaceHover/40 p-2.5 space-y-1 text-micro" data-testid="manvi-steward-last-tick">
      <div class="flex flex-wrap items-center justify-between gap-2 text-textPrimary font-medium">
        <span>
          Last check: {formatTimestamp(latestTick.ran_at_ms)} · {humanizeToken(latestTick.outcome)}
          {#if latestTick.tier && latestTick.tier !== "clear"}
            ({humanizeToken(latestTick.tier)}{latestTick.binding ? ` · ${humanizeToken(latestTick.binding)}` : ""})
          {/if}
        </span>
        {#if latestTick.reclaimed_bytes > 0}
          <span class="gp-chip">Reclaimed {formatBytes(latestTick.reclaimed_bytes)}</span>
        {/if}
      </div>
      <p class="text-textMuted">{redactDiagnosticText(latestTick.summary)}</p>
      {#if latestTick.directive}
        <p class="text-textPrimary leading-relaxed" data-testid="manvi-steward-directive">
          {redactDiagnosticText(latestTick.directive)}
        </p>
      {/if}
      {#if latestTick.messaged_sessions > 0 || latestTick.throttled_processes > 0}
        <p class="text-textMuted">
          Messaged {latestTick.messaged_sessions} sessions across {latestTick.messaged_repositories} repositories · Throttled {latestTick.throttled_processes} worker processes
        </p>
      {/if}
      {#if latestTick.cleaned_items.length > 0}
        <ul class="space-y-0.5 text-textMuted" data-testid="manvi-steward-cleaned-items">
          {#each latestTick.cleaned_items.slice(0, 4) as item, index (`${latestTick.id}-c-${index}`)}
            <li class="break-all">
              <span class="font-medium text-textPrimary">{humanizeToken(item.status)}</span>
              · <span class="font-mono">{redactDiagnosticText(item.target)}</span>
              {#if item.detail}
                — {redactDiagnosticText(item.detail)}
              {/if}
            </li>
          {/each}
          {#if latestTick.cleaned_items.length > 4}
            <li>
              Showing 4 of {latestTick.cleaned_items.length} cleanup items from this check.
            </li>
          {/if}
        </ul>
      {/if}
      {#if latestTick.warnings.length > 0}
        <ul class="space-y-0.5 text-warn">
          {#each latestTick.warnings as warning, index (`${latestTick.id}-w-${index}`)}
            <li>{redactDiagnosticText(warning)}</li>
          {/each}
        </ul>
      {/if}
    </div>
  {/if}

  {#if error}
    <p class="text-warn" role="alert">{redactDiagnosticText(error)}</p>
    <button
      type="button"
      class="gp-btn text-meta px-2 py-0.5"
      disabled={busy}
      onclick={() => loader.retry()}
    >
      Try again
    </button>
  {/if}
</section>
