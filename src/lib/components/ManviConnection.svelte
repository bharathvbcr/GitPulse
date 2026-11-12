<script lang="ts">
  /**
   * Manvi's policy connection and the capability boundary around it: whether
   * the gate is reachable, what the scoped runner is promised to be, and the
   * shortcuts to the surfaces GitPulse adds around the planes Manvi provides.
   *
   * Lives in Settings → Agents because it is configuration. The grant ledger
   * and the activity journal are about what happened, so they sit with the
   * repository on Work → Overview.
   */
  import { harnessStore } from "../stores/harnessStore";
  import { repoStore } from "../stores/repoStore";
  import { redactDiagnosticText } from "../diagnostics/diagnostics";
  import { manviSectionId } from "../ui/manviFocus";
  import { harnessPermissionMode, harnessPermissionSummary, harnessPostureLabel } from "../harness/availability";
  import { NOTICE_TONE, scopedRunnerPresentation } from "../harness/policyPresentation";
  import {
    RefreshCw,
    ShieldCheck,
    ShieldAlert,
    SquareTerminal,
    Wrench,
    Percent,
    Gauge,
  } from "@lucide/svelte";
  import { openSetupWizard } from "../tools/onboardingStore";

  let {
    onNavigate,
  }: {
    /**
     * Called after a shortcut navigates the app. Settings is a dialog over the
     * view, so a shortcut that left it open would change the page behind it.
     */
    onNavigate?: () => void;
  } = $props();

  let harness = $derived($harnessStore.harness);
  let harnessError = $derived($harnessStore.error);
  let permissionMode = $derived(harnessPermissionMode(harness));
  let postureLabel = $derived(harnessPostureLabel(harness?.posture));
  let runnerPresentation = $derived(scopedRunnerPresentation(permissionMode, harnessError));

  /**
   * The surfaces this card links to, as (view, section) pairs.
   *
   * Health and Coverage stopped being views of their own — they are scans of
   * the repository, and they live in Insights beside Pulse and Storage. The
   * pair is what a link needs now: the view alone would land on Insights'
   * default section, which is not the capability the button names.
   */
  const CAPABILITY_TARGETS = {
    health: { tab: "insights", section: "health" },
    coverage: { tab: "insights", section: "coverage" },
    github: { tab: "work", section: "remote" },
  } as const;

  type CapabilityTab = keyof typeof CAPABILITY_TARGETS;

  function openCapability(tab: CapabilityTab) {
    if (!$repoStore.currentPath) return;
    const target = CAPABILITY_TARGETS[tab];
    repoStore.setActiveTab(target.tab, target.section);
    onNavigate?.();
  }

  /**
   * The terminal is a dock, not a view, so it opens *over* whatever is on
   * screen instead of navigating away from it.
   */
  function openTerminal() {
    if (!$repoStore.currentPath) return;
    repoStore.setTerminalOpen(true);
    onNavigate?.();
  }
</script>

<div class="space-y-4">
  <!-- Policy connection -->
  <section id={manviSectionId("harness")} tabindex="-1" class="gp-card p-4 space-y-2">
    <div class="flex items-center justify-between gap-2">
      <h3 class="gp-label">Manvi policy</h3>
      <button
        onclick={() => harnessStore.reconnect()}
        class="gp-btn"
        data-size="sm"
        title="Restart Manvi and scan for model servers again"
      >
        <RefreshCw size={12} class={$harnessStore.isProbing ? "animate-spin" : ""} />
        <span>Reconnect</span>
      </button>
    </div>
    {#if harnessError}
      <div class="gp-notice" data-tone="unknown" role="alert">
        <ShieldAlert size={14} class="mt-0.5 shrink-0" style="color: var(--gp-notice-ink)" />
        <div class="min-w-0 flex-1 space-y-1.5">
          <div class="font-medium">Couldn't check Manvi status</div>
          <p class="text-textMuted leading-relaxed wrap-break-word">
            {redactDiagnosticText(harnessError)} Any previously displayed connection state may be stale;
            press Reconnect to check again.
          </p>
        </div>
      </div>
    {:else if permissionMode === "connected" && harness}
      <div class="gp-notice" data-tone="ok">
        <ShieldCheck size={14} class="mt-0.5 shrink-0" style="color: var(--gp-notice-ink)" />
        <div class="min-w-0 flex-1 space-y-1.5">
          <div class="font-medium">Connected · protocol {harness.protocol}{#if postureLabel}, {postureLabel}{/if}</div>
          <div class="font-mono text-meta text-textMuted break-all">{harness.binary}</div>
          <p class="text-textMuted leading-relaxed">
            Commits, pushes, merges, rebases, branch deletions, discards and conflict-editor
            saves are put to Manvi's gates before they run. Hard rules — force pushes,
            verification bypass flags, writes to credential paths — are refused here.
          </p>
          <div class="flex flex-wrap gap-1 pt-1">
            {#each harness.ops as op}
              <span class="gp-chip font-mono" data-tone="neutral" data-kind="label">{op}</span>
            {/each}
          </div>
        </div>
      </div>
    {:else if permissionMode === "unguarded"}
      <div class="gp-notice" data-tone="warn">
        <ShieldAlert size={14} class="mt-0.5 shrink-0" style="color: var(--gp-notice-ink)" />
        <div class="min-w-0 flex-1 space-y-1.5">
          <div class="font-medium">Manvi is not installed</div>
          <p class="text-textMuted leading-relaxed">
            {harnessPermissionSummary(harness)} Install Manvi from External CLIs below, or point
            <span class="font-mono">GITPULSE_MANVI_BIN</span> at the binary, then press Reconnect.
          </p>
          <button
            type="button"
            class="gp-btn"
            data-size="sm"
            onclick={() => openSetupWizard("manvi", "explain")}
          >
            Set up Manvi
          </button>
        </div>
      </div>
    {:else if permissionMode === "blocked"}
      <div class="gp-notice" data-tone="danger" role="alert">
        <ShieldAlert size={14} class="mt-0.5 shrink-0" style="color: var(--gp-notice-ink)" />
        <div class="min-w-0 flex-1 space-y-1.5">
          <div class="font-medium">Policy gate failed — mutations blocked</div>
          <p class="text-textMuted leading-relaxed">{harnessPermissionSummary(harness)}</p>
          <p class="text-textMuted leading-relaxed">
            Press Reconnect after resolving the failure. GitPulse will not treat a timed-out,
            busy, unavailable, or incompatible policy check as approval.
          </p>
        </div>
      </div>
    {:else}
      <div class="gp-notice" data-tone="unknown" role="status">
        {harnessPermissionSummary(harness)}
      </div>
    {/if}
  </section>

  <!-- The live `hello` response is intentionally narrow: GitPulse embeds
       Manvi's policy and local-model planes only. This card keeps that wire
       truth separate from app-owned, user-confirmed command execution. -->
  <section class="gp-card p-4 space-y-3">
    <div class="space-y-1">
      <h3 class="gp-label">Capability boundary</h3>
      <p class="text-textMuted leading-relaxed">
        No autonomous PTY or app-control API is exposed by <span class="font-mono">manvi serve</span>.
        GitPulse adds narrow, explicit controls around the planes Manvi actually provides.
      </p>
    </div>

    <div class="grid gap-2 sm:grid-cols-2">
      <div class="rounded-xl bg-background p-3 space-y-1">
        <div class="flex items-center gap-2 text-textPrimary font-medium">
          <SquareTerminal size={13} class="text-textMuted" />
          <span>Interactive shell</span>
          <span class="gp-chip ml-auto" data-tone="warn" data-kind="label">User only</span>
        </div>
        <p class="text-textMuted leading-relaxed">
          Manvi never receives the PTY handle or keystrokes. Shell commands are typed and owned by you.
        </p>
      </div>
      <div class="gp-notice" data-tone={NOTICE_TONE[runnerPresentation.tone]}>
        <div class="min-w-0 flex-1 space-y-1">
          <div class="flex items-center gap-2 text-textPrimary font-medium">
            {#if runnerPresentation.tone === "ready"}
              <ShieldCheck size={13} class={runnerPresentation.badgeClass} />
            {:else}
              <ShieldAlert size={13} class={runnerPresentation.badgeClass} />
            {/if}
            <span>Scoped action runner</span>
            <span class="ml-auto text-micro {runnerPresentation.badgeClass}">{runnerPresentation.label}</span>
          </div>
          <p class="text-textMuted leading-relaxed">
            {runnerPresentation.detail}
          </p>
        </div>
      </div>
    </div>

    <div class="grid grid-cols-2 sm:grid-cols-4 gap-2">
      <button
        class="gp-btn justify-start"
        disabled={!$repoStore.currentPath}
        onclick={() => openCapability("health")}
        title="Scan dependencies, ask Manvi for a remediation plan, and run approved steps"
      >
        <Wrench size={12} /> Health fixes
      </button>
      <button
        class="gp-btn justify-start"
        disabled={!$repoStore.currentPath}
        onclick={() => openCapability("coverage")}
        title="Generate, scan, and analyze coverage reports. Rust needs cargo-llvm-cov; a full run can take several minutes."
      >
        <Percent size={12} /> Coverage
      </button>
      <button
        class="gp-btn justify-start"
        disabled={!$repoStore.currentPath}
        onclick={openTerminal}
        title="Show the user-owned shell and bounded console in the dock below"
      >
        <SquareTerminal size={12} /> Terminal
      </button>
      <button
        class="gp-btn justify-start"
        disabled={!$repoStore.currentPath}
        onclick={() => openCapability("github")}
        title="Run this repository's bounded local CI pipeline"
      >
        <Gauge size={12} /> CI:local
      </button>
    </div>
  </section>
</div>
