<script lang="ts">
  /**
   * Resumable setup wizard for optional `devmap` / `manvi` CLIs.
   * Steps: explain → source → preflight → install → verify → done.
   */
  import { X, Download, Check, ChevronRight, Clipboard, Terminal } from "@lucide/svelte";
  import { invoke } from "@tauri-apps/api/core";
  import {
    cancelInstall,
    closeSetupWizard,
    markOnboardingComplete,
    onboardingStore,
    runInstall,
    runVerify,
    setWizardPreset,
    setWizardStep,
    setWizardTool,
    type WizardStep,
  } from "../../tools/onboardingStore";
  import {
    cloneToolSource,
    toolStatusSummary,
    type ExternalTool,
    type InstallRung,
  } from "../../tools/externalTools";
  import {
    DEVCOUNCIL_PRESETS,
    buildDevcouncilInstallCommand,
  } from "../../tools/devcouncilInstall";
  import { enqueueConsoleLaunch } from "../../terminal/consoleLaunches";
  import { interfaceStore } from "../../stores/interfaceStore";
  import { repoStore } from "../../stores/repoStore";
  import { copyText } from "../../desktop/clipboard";

  const wizard = onboardingStore.wizard;
  const status = onboardingStore.status;
  const ladder = onboardingStore.ladder;
  const preflight = onboardingStore.preflight;
  const installing = onboardingStore.installing;
  const progressLine = onboardingStore.progressLine;
  const lastOutcome = onboardingStore.lastOutcome;
  const verify = onboardingStore.verify;

  const steps: { id: WizardStep; label: string }[] = [
    { id: "explain", label: "Why" },
    { id: "source", label: "Source" },
    { id: "preflight", label: "Check" },
    { id: "install", label: "Install" },
    { id: "verify", label: "Verify" },
    { id: "done", label: "Done" },
  ];

  let cloneParent = $state("");
  let cloneNote = $state<string | null>(null);
  let copyNote = $state<string | null>(null);

  const toolStatus = $derived(
    $status ? ($wizard.tool === "devmap" ? $status.devmap : $status.manvi) : null,
  );
  const repoPath = $derived($repoStore.currentPath);
  const installSpec = $derived(
    $wizard.tool === "devmap"
      ? buildDevcouncilInstallCommand({
          preset: $wizard.preset,
          sourceCheckout: toolStatus?.source_checkout ?? null,
          windows: typeof navigator !== "undefined" && /Win/i.test(navigator.platform),
        })
      : null,
  );

  async function copyInstallCommand() {
    if (!installSpec?.runnable || !installSpec.command) return;
    copyNote = (await copyText(installSpec.command))
      ? "Copied — paste into a terminal."
      : "Could not copy. Select the command and copy it yourself.";
  }

  function runInstallInTerminal() {
    if (!installSpec?.runnable || !installSpec.command) return;
    if (!repoPath) {
      copyNote = "Open a repository first so Console has a working directory, or copy the command.";
      return;
    }
    try {
      enqueueConsoleLaunch({
        command: installSpec.command,
        label: installSpec.label,
        timeoutSecs: installSpec.timeoutSecs,
      });
      interfaceStore.setTerminalDockOpen(true);
      copyNote = "Running in Terminal → Console.";
    } catch (e) {
      copyNote = String(e);
    }
  }

  async function pickCloneParent() {
    const picked = await invoke<string | null>("cmd_pick_folder");
    if (picked) cloneParent = picked;
  }

  async function doClone() {
    if (!cloneParent) return;
    cloneNote = "Cloning…";
    const result = await cloneToolSource($wizard.tool, cloneParent);
    cloneNote = result.ok
      ? `Cloned to ${result.path}`
      : (result.reason ?? "Clone failed");
  }

  async function installSelected() {
    const rung = ($ladder?.selected ?? null) as InstallRung | null;
    const preferred =
      rung && rung !== "already_on_path" ? rung : ("local_checkout" as InstallRung);
    await runInstall($wizard.tool, preferred);
  }

  async function finish() {
    await markOnboardingComplete();
    closeSetupWizard();
  }
</script>

{#if $wizard.open}
  <div
    class="gp-scrim fixed inset-0 z-50 flex items-center justify-center bg-black/40 p-4"
    role="dialog"
    aria-modal="true"
    aria-label="Optional tools setup"
  >
    <div class="gp-card w-full max-w-lg rounded-2xl border border-border bg-surface shadow-float">
      <div class="flex items-center justify-between border-b border-border/60 px-4 py-3">
        <div>
          <h2 class="text-sm font-semibold text-textPrimary">Set up optional tools</h2>
          <p class="text-[11px] text-textMuted">
            <span class="font-mono">{$wizard.tool}</span> — Map and policy are optional
          </p>
        </div>
        <button type="button" class="p-1 text-textMuted hover:text-textPrimary" onclick={closeSetupWizard} aria-label="Close">
          <X size={14} />
        </button>
      </div>

      <div class="flex gap-1 border-b border-border/40 px-3 py-2 overflow-x-auto">
        {#each steps as s (s.id)}
          <button
            type="button"
            class="rounded px-2 py-0.5 text-[10px] {$wizard.step === s.id
              ? 'bg-accent/15 text-textPrimary'
              : 'text-textMuted'}"
            onclick={() => setWizardStep(s.id)}
          >
            {s.label}
          </button>
        {/each}
      </div>

      <div class="flex gap-2 px-4 pt-3">
        {#each (["devmap", "manvi"] as ExternalTool[]) as t (t)}
          <button
            type="button"
            class="rounded-lg border px-2 py-1 text-[11px] font-mono {$wizard.tool === t
              ? 'border-accent/50 bg-accent/10 text-textPrimary'
              : 'border-border/60 text-textMuted'}"
            onclick={() => setWizardTool(t)}
          >
            {t}
          </button>
        {/each}
      </div>

      {#if $wizard.tool === "devmap"}
        <div class="flex flex-col gap-1 px-4 pt-3">
          <p class="text-[10px] text-textMuted">DevCouncil components</p>
          <div class="flex flex-wrap gap-2">
            {#each DEVCOUNCIL_PRESETS as p (p.id)}
              <button
                type="button"
                class="rounded-lg border px-2 py-1 text-left text-[11px] {$wizard.preset === p.id
                  ? 'border-accent/50 bg-accent/10 text-textPrimary'
                  : 'border-border/60 text-textMuted'}"
                onclick={() => setWizardPreset(p.id)}
              >
                <div class="font-medium">{p.label}</div>
                <div class="text-[10px] text-textMuted leading-snug">{p.detail}</div>
              </button>
            {/each}
          </div>
        </div>
      {/if}

      <div class="px-4 py-3 space-y-3 text-[12px] text-textSecondary min-h-[200px]">
        {#if $wizard.step === "explain"}
          <p>
            <span class="font-mono">devmap</span> powers Code → Map and symbol search.
            <span class="font-mono">manvi</span> adds a local policy harness for git mutations.
            Both are optional — GitPulse works without them.
          </p>
          <p class="text-textMuted text-[11px]">
            Install order: PATH → prebuilt release → cargo/go install → local checkout.
            There is no uv / Python path — these are native binaries.
          </p>
          <button type="button" class="gp-btn text-[11px] inline-flex items-center gap-1" onclick={() => setWizardStep("source")}>
            Continue <ChevronRight size={12} />
          </button>
        {:else if $wizard.step === "source"}
          {#if toolStatus?.source_checkout}
            <p>
              Source checkout: <span class="font-mono break-all">{toolStatus.source_checkout}</span>
            </p>
          {:else}
            <p class="text-textMuted">No sibling checkout detected. Clone the public repo, or pick a folder later.</p>
          {/if}
          <div class="flex flex-wrap gap-2 items-center">
            <button type="button" class="gp-btn text-[11px]" onclick={() => void pickCloneParent()}>
              Pick parent folder
            </button>
            {#if cloneParent}
              <span class="font-mono text-[10px] break-all">{cloneParent}</span>
              <button type="button" class="gp-btn text-[11px]" onclick={() => void doClone()}>Clone</button>
            {/if}
          </div>
          {#if cloneNote}
            <p class="text-[11px] text-textMuted">{cloneNote}</p>
          {/if}
          <button type="button" class="gp-btn text-[11px]" onclick={() => setWizardStep("preflight")}>
            Continue
          </button>
        {:else if $wizard.step === "preflight"}
          {#if $preflight}
            <p class="text-textMuted text-[11px]">{$preflight.estimate}</p>
            <ul class="space-y-1">
              {#each $preflight.requirements as req, i (`${req.name}#${i}`)}
                <li class="flex items-start gap-2 font-mono text-[11px]">
                  <span class={req.satisfies ? "text-emerald-500" : "text-amber-500"}>
                    {req.satisfies ? "✓" : "✗"}
                  </span>
                  <span>
                    {req.name}
                    {#if req.version}<span class="text-textMuted"> · {req.version}</span>{/if}
                    {#if req.note}<div class="text-textMuted font-sans">{req.note}</div>{/if}
                  </span>
                </li>
              {/each}
            </ul>
          {:else}
            <p class="text-textMuted">Checking prerequisites…</p>
          {/if}
          <button type="button" class="gp-btn text-[11px]" onclick={() => setWizardStep("install")}>
            Continue
          </button>
        {:else if $wizard.step === "install"}
          {#if $wizard.tool === "devmap" && installSpec}
            <p class="text-[11px] text-textMuted">
              {#if $wizard.preset === "devmap"}
                In-app Install uses GitPulse’s ladder (PATH, prebuilt, cargo, checkout). Copy or run the command to install from the documented scripts instead.
              {:else}
                Analysis and full DevCouncil install in Terminal. In-app Install stays DevMap-only.
              {/if}
            </p>
            <p class="font-mono text-[10px] text-textMuted break-all leading-snug">{installSpec.command}</p>
            <div class="flex flex-wrap gap-2">
              <button
                type="button"
                class="gp-btn text-[11px] inline-flex items-center gap-1"
                disabled={!installSpec.runnable}
                title={installSpec.runnable ? "Copy the install command" : "This checkout path cannot be turned into a shell command"}
                onclick={() => void copyInstallCommand()}
              >
                <Clipboard size={12} />
                Copy command
              </button>
              <button
                type="button"
                class="gp-btn text-[11px] inline-flex items-center gap-1"
                disabled={!repoPath || !installSpec.runnable}
                title={
                  !installSpec.runnable
                    ? "This checkout path cannot be turned into a shell command"
                    : repoPath
                      ? installSpec.label
                      : "Open a repository first so Console has a working directory"
                }
                onclick={() => runInstallInTerminal()}
              >
                <Terminal size={12} />
                Run in terminal
              </button>
            </div>
            {#if copyNote}
              <p class="text-[11px] text-textMuted">{copyNote}</p>
            {/if}
          {/if}
          {#if $wizard.tool !== "devmap" || $wizard.preset === "devmap"}
            {#if $ladder}
              <ul class="space-y-1.5">
                {#each $ladder.rungs as rung, i (`${rung.rung}#${i}`)}
                  <li
                    class="rounded-lg border px-2 py-1.5 text-[11px] {rung.available
                      ? 'border-border/60'
                      : 'border-border/30 opacity-70'}"
                  >
                    <div class="font-medium text-textPrimary">{rung.rung.replaceAll("_", " ")}</div>
                    <div class="text-textMuted">{rung.cost}{#if rung.command} · {rung.command}{/if}</div>
                    {#if rung.block}
                      <div class="text-amber-600 dark:text-amber-400">{rung.block}</div>
                    {/if}
                    {#if $ladder.selected === rung.rung}
                      <div class="text-accent text-[10px] mt-0.5">Selected</div>
                    {/if}
                  </li>
                {/each}
              </ul>
            {/if}
            {#if toolStatus}
              <p class="font-mono text-[10px] text-textMuted break-all">{toolStatusSummary(toolStatus)}</p>
            {/if}
            {#if $progressLine}
              <p class="font-mono text-[10px] text-textMuted truncate">{$progressLine}</p>
            {/if}
            {#if $lastOutcome && !$lastOutcome.ok}
              <p class="text-amber-600 dark:text-amber-400 text-[11px]">
                {$lastOutcome.reason ?? $lastOutcome.stderr}
              </p>
            {/if}
            <div class="flex gap-2">
              {#if $installing === $wizard.tool}
                <button type="button" class="gp-btn text-[11px]" onclick={() => void cancelInstall()}>Cancel</button>
              {:else}
                <button
                  type="button"
                  class="gp-btn text-[11px] inline-flex items-center gap-1"
                  disabled={toolStatus?.installed && $ladder?.selected === "already_on_path"}
                  onclick={() => void installSelected()}
                >
                  <Download size={12} />
                  {toolStatus?.installed ? "Reinstall" : "Install"}
                </button>
                <button type="button" class="gp-btn text-[11px]" onclick={() => setWizardStep("verify")}>
                  Skip to verify
                </button>
              {/if}
            </div>
            {#if $lastOutcome?.ok}
              <button type="button" class="gp-btn text-[11px]" onclick={() => setWizardStep("verify")}>
                Continue to verify
              </button>
            {/if}
          {:else}
            <button type="button" class="gp-btn text-[11px]" onclick={() => setWizardStep("verify")}>
              Skip to verify
            </button>
          {/if}
        {:else if $wizard.step === "verify"}
          <button
            type="button"
            class="gp-btn text-[11px]"
            onclick={() => void runVerify($wizard.tool)}
          >
            Run verify
          </button>
          {#if $verify && $verify.tool === $wizard.tool}
            <p class={$verify.ok ? "text-emerald-600 dark:text-emerald-400" : "text-amber-600 dark:text-amber-400"}>
              {$verify.detail}
            </p>
            {#if $verify.binary}
              <p class="font-mono text-[10px] break-all text-textMuted">{$verify.binary}</p>
            {/if}
          {/if}
          <button type="button" class="gp-btn text-[11px]" onclick={() => setWizardStep("done")}>
            Continue
          </button>
        {:else}
          <div class="flex items-center gap-2 text-emerald-600 dark:text-emerald-400">
            <Check size={14} />
            <span>Setup complete for this session.</span>
          </div>
          <button type="button" class="gp-btn text-[11px]" onclick={() => void finish()}>Done</button>
        {/if}
      </div>
    </div>
  </div>
{/if}
