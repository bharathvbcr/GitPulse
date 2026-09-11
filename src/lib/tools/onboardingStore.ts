/**
 * Shared store for external-tool status, setup wizard, and install progress.
 */
import { writable, derived, get } from "svelte/store";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  cancelExternalToolInstall,
  getExternalToolsStatus,
  getToolConfig,
  getToolLadder,
  getToolPreflight,
  installExternalTool,
  refreshToolCapability,
  saveToolConfig,
  verifyTool,
  type ExternalTool,
  type InstallOutcome,
  type InstallRung,
  type LadderAssessment,
  type PreflightReport,
  type ToolConfigView,
  type ToolsStatus,
  type ToolInstallProgress,
  type VerifyReport,
} from "./externalTools";

import type { DevcouncilPreset } from "./devcouncilInstall";

export type WizardStep = "explain" | "source" | "preflight" | "install" | "verify" | "done";

export interface SetupWizardState {
  open: boolean;
  tool: ExternalTool;
  step: WizardStep;
  focusToolOnly: boolean;
  /** DevCouncil component subset. Ignored when `tool` is `manvi`. */
  preset: DevcouncilPreset;
}

const wizard = writable<SetupWizardState>({
  open: false,
  tool: "devmap",
  step: "explain",
  focusToolOnly: false,
  preset: "devmap",
});

const status = writable<ToolsStatus | null>(null);
const config = writable<ToolConfigView | null>(null);
const loadError = writable<string | null>(null);
const installing = writable<ExternalTool | null>(null);
const lastOutcome = writable<InstallOutcome | null>(null);
const progressLine = writable<string | null>(null);
const preflight = writable<PreflightReport | null>(null);
const ladder = writable<LadderAssessment | null>(null);
const verify = writable<VerifyReport | null>(null);

let progressUnlisten: UnlistenFn | null = null;

export const onboardingStore = {
  wizard,
  status,
  config,
  loadError,
  installing,
  lastOutcome,
  progressLine,
  preflight,
  ladder,
  verify,
  /** True when either tool is missing and onboarding was not dismissed. */
  showFirstRunCard: derived([status, config], ([$status, $config]) => {
    if (!$status || !$config) return false;
    if ($config.config.onboarding.dismissed) return false;
    if ($config.config.onboarding.completed_at) return false;
    return !$status.devmap.installed || !$status.manvi.installed;
  }),
};

export async function refreshToolsStatus(): Promise<ToolsStatus | null> {
  try {
    await refreshToolCapability();
    const next = await getExternalToolsStatus();
    status.set(next);
    loadError.set(null);
    return next;
  } catch (e) {
    loadError.set(String(e));
    return null;
  }
}

export async function refreshToolConfig(): Promise<ToolConfigView | null> {
  try {
    const next = await getToolConfig();
    config.set(next);
    return next;
  } catch (e) {
    loadError.set(String(e));
    return null;
  }
}

export function openSetupWizard(tool: ExternalTool = "devmap", step: WizardStep = "explain") {
  wizard.set({
    open: true,
    tool,
    step,
    focusToolOnly: tool !== "devmap" || step !== "explain",
    preset: "devmap",
  });
  void ensureProgressListener();
  void refreshToolsStatus();
  void refreshToolConfig();
  void loadLadderAndPreflight(tool);
}

export function closeSetupWizard() {
  wizard.update((w) => ({ ...w, open: false }));
}

export function setWizardStep(step: WizardStep) {
  wizard.update((w) => ({ ...w, step }));
}

export function setWizardTool(tool: ExternalTool) {
  wizard.update((w) => ({ ...w, tool, preset: tool === "devmap" ? w.preset : "devmap" }));
  void loadLadderAndPreflight(tool);
}

export function setWizardPreset(preset: DevcouncilPreset) {
  wizard.update((w) => ({ ...w, preset }));
}

async function loadLadderAndPreflight(tool: ExternalTool) {
  try {
    ladder.set(await getToolLadder(tool));
    preflight.set(await getToolPreflight(tool));
  } catch (e) {
    loadError.set(String(e));
  }
}

async function ensureProgressListener() {
  if (progressUnlisten) return;
  progressUnlisten = await listen<ToolInstallProgress>(
    "tool-install-progress",
    (event) => {
      progressLine.set(event.payload.line);
    },
  );
}

export async function runInstall(tool: ExternalTool, rung?: InstallRung | null) {
  if (get(installing)) return;
  installing.set(tool);
  lastOutcome.set(null);
  progressLine.set(null);
  await ensureProgressListener();
  try {
    const outcome = await installExternalTool(tool, rung);
    lastOutcome.set(outcome);
    await refreshToolsStatus();
    if (outcome.ok) {
      verify.set(await verifyTool(tool));
    }
  } catch (e) {
    lastOutcome.set({
      tool,
      ok: false,
      binary: null,
      lookup: null,
      version: null,
      source_used: null,
      command: "",
      exit_code: null,
      stdout: "",
      stderr: String(e),
      timed_out: false,
      cancelled: false,
      reason: String(e),
    });
  } finally {
    installing.set(null);
  }
}

export async function cancelInstall() {
  await cancelExternalToolInstall();
}

export async function dismissFirstRun() {
  const current = get(config)?.config ?? {
    version: 1,
    devmap: {},
    manvi: {},
    onboarding: { skipped_tools: [], dismissed: false },
  };
  const next = await saveToolConfig({
    ...current,
    onboarding: {
      ...current.onboarding,
      dismissed: true,
      skipped_tools: current.onboarding.skipped_tools ?? [],
    },
  });
  config.set(next);
}

export async function markOnboardingComplete() {
  const current = get(config)?.config;
  if (!current) return;
  const next = await saveToolConfig({
    ...current,
    onboarding: {
      ...current.onboarding,
      completed_at: new Date().toISOString(),
      dismissed: true,
      skipped_tools: current.onboarding.skipped_tools ?? [],
    },
  });
  config.set(next);
}

export async function runVerify(tool: ExternalTool) {
  verify.set(await verifyTool(tool));
}

export type { DevcouncilPreset };
