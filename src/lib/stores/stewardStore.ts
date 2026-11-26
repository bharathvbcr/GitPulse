import { writable, type Readable } from "svelte/store";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { invoke } from "../ipc/invoke";
import { isTauri } from "../platform";
import type {
  EffectiveStewardPolicy,
  ManviStewardSettings,
  ManviStewardStatus,
  RepoStewardOverride,
} from "../steward/types";

export const DEFAULT_MANVI_STEWARD_SETTINGS: ManviStewardSettings = {
  enabled: true,
  interval_secs: 180,
  message_agents: true,
  slow_critical: false,
  clean_critical: false,
  clean_retention_days: 3,
  repositories: {},
};

export function resolveEffectiveStewardPolicy(
  settings: ManviStewardSettings,
  repoCommonDir: string | null,
  envForced: boolean | null,
): EffectiveStewardPolicy {
  const ov = repoCommonDir ? settings.repositories[repoCommonDir] : undefined;
  const overridden = Boolean(
    ov &&
      (ov.enabled !== undefined ||
        ov.message_agents !== undefined ||
        ov.slow_critical !== undefined ||
        ov.clean_critical !== undefined),
  );
  return {
    enabled: envForced ?? ov?.enabled ?? settings.enabled,
    interval_secs: settings.interval_secs,
    message_agents: ov?.message_agents ?? settings.message_agents,
    slow_critical: ov?.slow_critical ?? settings.slow_critical,
    clean_critical: ov?.clean_critical ?? settings.clean_critical,
    clean_retention_days: settings.clean_retention_days,
    overridden,
  };
}

/**
 * Mirrors Rust `steward::is_master_enabled`: whether the background steward is
 * active machine-wide (respecting `GITPULSE_MANVI_STEWARD` before global/per-repo switches).
 */
export function isMasterStewardEnabled(
  settings: ManviStewardSettings,
  envForced: boolean | null,
): boolean {
  if (typeof envForced === "boolean") return envForced;
  return (
    settings.enabled ||
    Object.values(settings.repositories).some((ov) => ov?.enabled === true)
  );
}

export const EMPTY_MANVI_STEWARD_STATUS: ManviStewardStatus = {
  settings: DEFAULT_MANVI_STEWARD_SETTINGS,
  effective: resolveEffectiveStewardPolicy(DEFAULT_MANVI_STEWARD_SETTINGS, null, null),
  repo_common_dir: null,
  env_override: null,
  env_forced: null,
  manvi_state: "not_chosen",
  manvi_mode: "",
  manvi_model: "",
  manvi_detail: "",
  running: false,
  critical_throttle_active: false,
  throttled_count: 0,
  last_check_ms: null,
  next_check_ms: null,
  history: [],
};

const state = writable<ManviStewardStatus>(EMPTY_MANVI_STEWARD_STATUS);
let unlistenPromise: Promise<UnlistenFn> | null = null;

function mergeIncomingStewardStatus(
  prev: ManviStewardStatus,
  incoming: ManviStewardStatus,
  preserveRepoContext: boolean,
): ManviStewardStatus {
  const repoCommonDir = preserveRepoContext
    ? incoming.repo_common_dir ?? prev.repo_common_dir
    : incoming.repo_common_dir;
  const effective = resolveEffectiveStewardPolicy(
    incoming.settings,
    repoCommonDir,
    incoming.env_forced,
  );
  return {
    ...incoming,
    repo_common_dir: repoCommonDir,
    effective,
    next_check_ms:
      repoCommonDir !== null && !effective.enabled ? null : incoming.next_check_ms,
  };
}

function applyIncomingStatus(
  incoming: ManviStewardStatus,
  preserveRepoContext: boolean,
): ManviStewardStatus {
  let merged = incoming;
  state.update((prev) => {
    merged = mergeIncomingStewardStatus(prev, incoming, preserveRepoContext);
    return merged;
  });
  return merged;
}

function ensureSubscribed(): void {
  if (!isTauri() || unlistenPromise !== null) return;
  unlistenPromise = listen<ManviStewardStatus>("manvi-steward-updated", (event) => {
    const incoming = event.payload;
    if (!incoming || typeof incoming !== "object" || !incoming.settings) return;
    applyIncomingStatus(incoming, true);
  }).catch(() => {
    unlistenPromise = null;
    return () => {};
  });
}

export interface StewardStoreOptions {
  preserveRepoContext?: boolean;
}

export async function refreshManviSteward(
  repoPath: string | null = null,
  options?: StewardStoreOptions,
): Promise<ManviStewardStatus> {
  if (!isTauri()) return EMPTY_MANVI_STEWARD_STATUS;
  ensureSubscribed();
  const status = await invoke<ManviStewardStatus>("cmd_manvi_steward_status", {
    repoPath,
  });
  if (!status || typeof status !== "object" || !status.settings) {
    return EMPTY_MANVI_STEWARD_STATUS;
  }
  const preserve = options?.preserveRepoContext ?? repoPath === null;
  return applyIncomingStatus(status, preserve);
}

export async function saveManviStewardSettings(
  settings: ManviStewardSettings,
  repoPath: string | null = null,
  options?: StewardStoreOptions,
): Promise<ManviStewardStatus> {
  const preserve = options?.preserveRepoContext ?? repoPath === null;
  const fallbackUpdate = (): ManviStewardStatus => {
    let nextStatus = EMPTY_MANVI_STEWARD_STATUS;
    state.update((prev) => {
      const repoCommonDir = preserve ? prev.repo_common_dir : null;
      nextStatus = {
        ...prev,
        repo_common_dir: repoCommonDir,
        settings,
        effective: resolveEffectiveStewardPolicy(settings, repoCommonDir, prev.env_forced),
      };
      return nextStatus;
    });
    return nextStatus;
  };
  if (!isTauri()) {
    return fallbackUpdate();
  }
  ensureSubscribed();
  const status = await invoke<ManviStewardStatus>("cmd_manvi_steward_save", {
    settings,
    repoPath,
  });
  if (!status || typeof status !== "object" || !status.settings) {
    return fallbackUpdate();
  }
  return applyIncomingStatus(status, preserve);
}

export async function saveManviStewardRepoOverride(
  repoPath: string,
  overridePolicy: RepoStewardOverride | null,
): Promise<ManviStewardStatus> {
  if (!isTauri()) return EMPTY_MANVI_STEWARD_STATUS;
  ensureSubscribed();
  const status = await invoke<ManviStewardStatus>("cmd_manvi_steward_set_repo", {
    repoPath,
    overridePolicy,
  });
  if (!status || typeof status !== "object" || !status.settings) {
    let current = EMPTY_MANVI_STEWARD_STATUS;
    state.update((prev) => {
      current = prev;
      return prev;
    });
    return current;
  }
  return applyIncomingStatus(status, false);
}

export async function runManviStewardNow(
  repoPath: string | null = null,
  options?: StewardStoreOptions,
): Promise<ManviStewardStatus> {
  if (!isTauri()) return EMPTY_MANVI_STEWARD_STATUS;
  ensureSubscribed();
  const status = await invoke<ManviStewardStatus>("cmd_manvi_steward_run_now", {
    repoPath,
  });
  if (!status || typeof status !== "object" || !status.settings) {
    return EMPTY_MANVI_STEWARD_STATUS;
  }
  const preserve = options?.preserveRepoContext ?? repoPath === null;
  return applyIncomingStatus(status, preserve);
}

/** Test-only helper to seed store state in unit/SSR tests. */
export function setManviStewardForTest(next: ManviStewardStatus): void {
  state.set(next);
}

export function resetManviStewardForTests(): void {
  state.set(EMPTY_MANVI_STEWARD_STATUS);
}

export const manviSteward: Readable<ManviStewardStatus> = {
  subscribe: state.subscribe,
};
