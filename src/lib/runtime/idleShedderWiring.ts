/**
 * Wires the three-stage idle memory manager (`idleShedder.ts`) into the live
 * frontend stores, GPU slot pool, commit-graph cache, file listings, and
 * Tauri Stage 2 hibernation commands.
 */
import { get, writable, type Readable } from "svelte/store";
import { invoke } from "../ipc/invoke";
import { isTauri } from "../platform";
import { flushPendingWrites } from "../async/deferredWrite";
import { gitActionCount } from "../desktop/exitGuard";
import { hasUnsavedEditorDrafts } from "../files/editorDraftRegistry";
import { invalidateRepoListing } from "../files/repoListing";
import { editorFileSaveQueue } from "../files/serialSave";
import { applyResourceBudgets, COMPACT_BUDGETS, type ResourceBudgets } from "../resourceBudgets";
import { layoutStore } from "../sidebar/layoutStore";
import { filterStore } from "../stores/filterStore";
import { graphStore } from "../stores/graphStore";
import { harnessStore } from "../stores/harnessStore";
import { hostPressure, resourceProfile, type PressureInput } from "../stores/hostLoadStore";
import { interfaceStore } from "../stores/interfaceStore";
import { isPromptOpen } from "../stores/modalStore";
import { repoStore } from "../stores/repoStore";
import { gpuPolicy, gpuSlots } from "../terminal/gpuRenderer";
import type { ForegroundDocument } from "./foreground";
import {
  createIdleShedder,
  publishIdleStage,
  writeWakeSkeletonHint,
  type IdleMemoryMode,
  type IdleShedder,
  type IdleShedderTimers,
  type IdleStage,
} from "./idleShedder";

export interface HibernationSnapshot {
  surface: string;
  active_repo: string | null;
  sidebar_width: number;
  has_terminal_dock: boolean;
  parked_browser_urls: string[];
  saved_at_ms: number;
}

export type MainWindowLifecycle =
  | "visible"
  | "hidden"
  | "hibernating"
  | "hibernated"
  | "waking";

export type PendingMainEvent =
  | { kind: "open_repo"; payload: string }
  | { kind: "browse"; payload: { repo: string; url: string } }
  | { kind: "agent_kickoff"; payload: Record<string, unknown> }
  | { kind: "notification"; payload: { id: string } }
  | { kind: "emit"; payload: { event: string; payload: unknown } };

export interface MainReadyPayload {
  wake_from_hibernation: boolean;
  snapshot: HibernationSnapshot | null;
  pending_events: PendingMainEvent[];
}

export interface IdleMemoryStatus {
  lifecycle?: MainWindowLifecycle;
  mode?: IdleMemoryMode;
  hibernate_count?: number;
  last_hibernated_at_ms?: number | null;
  last_wake_ms?: number | null;
  last_trim_report?: {
    ref_cache_evicted?: number;
    docs_vault_evicted?: number;
    sqlite_trimmed?: number;
    os_allocator_trimmed?: boolean;
  } | null;
}

export type InvokeLike = <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>;

export interface IdleShedderWiringDeps {
  initialMode?: IdleMemoryMode;
  skipInitialLoad?: boolean;
  isTauri?: () => boolean;
  invoke?: InvokeLike;
  doc?: (EventTarget & ForegroundDocument) | null;
  win?: EventTarget | null;
  timers?: IdleShedderTimers;
  pressureSource?: Readable<PressureInput>;
  profileSource?: Readable<{ budgets: ResourceBudgets } | null>;
  activeRepo?: () => string | null;
  openRepoPaths?: () => readonly string[];
  repoTrustRequired?: () => boolean;
  graphFilter?: () => { searchQuery: string; selectedBranch: string | null; refScope: "named" | "all" };
  hasOpenModal?: () => boolean;
  gitMutationCount?: () => number;
  pendingFileSaves?: () => number;
  hasUnsavedDrafts?: () => boolean;
  onFlushBeforeHibernate?: () => void;
  snapshotMeta?: () => {
    surface: string;
    sidebarWidth: number;
    hasTerminalDock: boolean;
    parkedBrowserUrls?: string[];
  };
  gpu?: {
    policy: { hideGraceMs: number };
    slots: {
      setCap(cap: number): void;
      releaseIdle?: () => number;
      setSuspended?: (suspended: boolean) => void;
    };
  };
  graph?: {
    setCacheLimit(limit: number): void;
    shedAll(): number;
    showRepo(path: string | null): void;
    loadGraph(
      repoPath: string,
      query?: string,
      revision?: string | null,
      opts?: { refScope?: "named" | "all" },
    ): Promise<void>;
  };
  invalidateListing?: (repoPath?: string) => void;
  evictHarness?: () => number;
  publishStage?: (stage: IdleStage) => void;
  writeWakeHint?: (hint: {
    surface: string;
    sidebarWidth: number;
    hasTerminalDock: boolean;
    savedAt: number;
  }) => void;
  now?: () => number;
}

export function normalizeIdleMemoryMode(raw: unknown): IdleMemoryMode {
  if (raw === "off" || raw === "balanced" || raw === "aggressive") return raw;
  return "balanced";
}

const modeStore = writable<IdleMemoryMode>("balanced");
export const idleMemoryMode: Readable<IdleMemoryMode> = { subscribe: modeStore.subscribe };

let activeShedder: IdleShedder | null = null;
let stopSubscriptions: (() => void) | null = null;
let bootListenersPromise: Promise<unknown> = Promise.resolve();

/**
 * Registers async Tauri event-listener promises installed by `main.ts` so
 * `App.svelte` waits for them before calling `cmd_main_ready`.
 */
export function registerBootListenersPromise(promise: Promise<unknown>): void {
  bootListenersPromise = promise;
}

/** Resolves once `main.ts` boot event listeners have finished attaching. */
export async function waitForBootListeners(): Promise<void> {
  await bootListenersPromise.catch(() => undefined);
}

export async function loadIdleMemoryMode(
  deps: { isTauri?: () => boolean; invoke?: InvokeLike } = {},
): Promise<IdleMemoryMode> {
  const tauri = deps.isTauri ?? isTauri;
  const invokeFn = deps.invoke ?? (invoke as InvokeLike);
  if (!tauri()) return get(modeStore);
  const payload = await invokeFn<{ mode?: unknown }>("cmd_idle_memory_settings");
  const mode = normalizeIdleMemoryMode(payload?.mode);
  modeStore.set(mode);
  activeShedder?.setMode(mode);
  return mode;
}

export async function saveIdleMemoryMode(
  mode: IdleMemoryMode,
  deps: { isTauri?: () => boolean; invoke?: InvokeLike } = {},
): Promise<IdleMemoryMode> {
  const normalized = normalizeIdleMemoryMode(mode);
  const tauri = deps.isTauri ?? isTauri;
  const invokeFn = deps.invoke ?? (invoke as InvokeLike);
  if (!tauri()) {
    modeStore.set(normalized);
    activeShedder?.setMode(normalized);
    return normalized;
  }
  const saved = await invokeFn<{ mode?: unknown }>("cmd_idle_memory_settings_save", {
    settings: { mode: normalized },
  });
  const resolved = normalizeIdleMemoryMode(saved?.mode ?? normalized);
  modeStore.set(resolved);
  activeShedder?.setMode(resolved);
  return resolved;
}

export async function loadIdleMemoryStatus(
  deps: { isTauri?: () => boolean; invoke?: InvokeLike } = {},
): Promise<IdleMemoryStatus | null> {
  const tauri = deps.isTauri ?? isTauri;
  const invokeFn = deps.invoke ?? (invoke as InvokeLike);
  if (!tauri()) return null;
  return await invokeFn<IdleMemoryStatus>("cmd_idle_memory_status");
}

export function getActiveIdleShedder(): IdleShedder | null {
  return activeShedder;
}

export function stopIdleShedder(): void {
  stopSubscriptions?.();
  stopSubscriptions = null;
  activeShedder?.stop();
  activeShedder = null;
  publishIdleStage("active");
}

export function resetIdleMemoryStateForTests(): void {
  stopIdleShedder();
  modeStore.set("balanced");
}

export function startIdleShedder(deps: IdleShedderWiringDeps = {}): IdleShedder {
  stopIdleShedder();

  const tauri = deps.isTauri ?? isTauri;
  const invokeFn = deps.invoke ?? (invoke as InvokeLike);
  const pressureSource = deps.pressureSource ?? hostPressure;
  const profileSource = deps.profileSource ?? resourceProfile;
  const gpu = deps.gpu ?? { policy: gpuPolicy, slots: gpuSlots };
  const graph = deps.graph ?? graphStore;
  const invalidateListing = deps.invalidateListing ?? invalidateRepoListing;
  const evictHarness = deps.evictHarness ?? (() => harnessStore.evictInactiveRepositories());
  const publishStage = deps.publishStage ?? publishIdleStage;
  const writeWakeHint = deps.writeWakeHint ?? writeWakeSkeletonHint;
  const now = deps.now ?? (deps.timers ? () => deps.timers!.now() : () => Date.now());

  const activeRepo =
    deps.activeRepo ?? (() => get(repoStore).currentPath ?? null);
  const openRepoPaths =
    deps.openRepoPaths ?? (() => get(repoStore).openTabs.map((tab) => tab.path));
  const repoTrustRequired =
    deps.repoTrustRequired ?? (() => get(repoStore).trustRequired);
  const graphFilter =
    deps.graphFilter ??
    (() => {
      const filter = get(filterStore);
      const prefs = get(interfaceStore);
      return {
        searchQuery: filter.searchQuery,
        selectedBranch: filter.selectedBranch,
        refScope: prefs.graphRefScope,
      };
    });

  if (deps.initialMode !== undefined) {
    modeStore.set(normalizeIdleMemoryMode(deps.initialMode));
  }

  const retainActiveListing = (active: string | null) => {
    if (!active) {
      invalidateListing();
      return;
    }
    for (const path of openRepoPaths()) {
      if (path !== active) invalidateListing(path);
    }
  };

  const sendIdleHint = (lowMemory: boolean) => {
    if (!tauri()) return;
    void invokeFn<void>("cmd_webview_idle_hint", { lowMemory }).catch(() => undefined);
  };

  const shedder = createIdleShedder(
    {
      onSoftShed() {
        gpu.policy.hideGraceMs = 0;
        gpu.slots.releaseIdle?.();
        graph.setCacheLimit(1);
        retainActiveListing(activeRepo());
        evictHarness();
        publishStage("soft_shed");
      },
      onDeepShed() {
        gpu.slots.setSuspended?.(true);
        graph.shedAll();
        invalidateListing();
        sendIdleHint(true);
        publishStage("deep_shed");
      },
      onResume(previous) {
        const budgets = get(profileSource)?.budgets ?? COMPACT_BUDGETS;
        applyResourceBudgets(budgets, {
          slots: gpu.slots,
          policy: gpu.policy,
          graph,
        });
        gpu.slots.setSuspended?.(false);
        sendIdleHint(false);
        publishStage("active");

        if (previous === "deep_shed" || previous === "hibernating") {
          const active = activeRepo();
          if (active && !repoTrustRequired()) {
            const filter = graphFilter();
            graph.showRepo(active);
            void graph
              .loadGraph(active, filter.searchQuery, filter.selectedBranch, {
                refScope: filter.refScope,
              })
              .catch(() => undefined);
          }
        }
      },
      hibernateBlocker() {
        const gitCount = deps.gitMutationCount
          ? deps.gitMutationCount()
          : gitActionCount(get(repoStore.mutationActivity));
        if (gitCount > 0) return "Git action in progress";

        const pendingSaves = deps.pendingFileSaves
          ? deps.pendingFileSaves()
          : editorFileSaveQueue.pending;
        if (pendingSaves > 0) return "Editor file save in progress";

        const unsaved = deps.hasUnsavedDrafts
          ? deps.hasUnsavedDrafts()
          : hasUnsavedEditorDrafts();
        if (unsaved) return "Unsaved editor drafts";

        if (isPromptOpen() || deps.hasOpenModal?.()) {
          return "Interactive modal is open";
        }
        return null;
      },
      async onHibernate() {
        if (deps.onFlushBeforeHibernate) {
          deps.onFlushBeforeHibernate();
        } else {
          flushPendingWrites();
        }

        const meta = deps.snapshotMeta
          ? deps.snapshotMeta()
          : {
              surface: get(interfaceStore).globalSurface,
              sidebarWidth: get(layoutStore).collapsed ? 0 : get(layoutStore).width,
              hasTerminalDock: Boolean(get(repoStore).terminalOpen),
              parkedBrowserUrls: [] as string[],
            };
        const savedAt = now();
        writeWakeHint({
          surface: meta.surface,
          sidebarWidth: meta.sidebarWidth,
          hasTerminalDock: meta.hasTerminalDock,
          savedAt,
        });

        if (!tauri()) return false;
        const snapshot: HibernationSnapshot = {
          surface: meta.surface,
          active_repo: activeRepo(),
          sidebar_width: meta.sidebarWidth,
          has_terminal_dock: meta.hasTerminalDock,
          parked_browser_urls: meta.parkedBrowserUrls ?? [],
          saved_at_ms: savedAt,
        };
        try {
          return await invokeFn<boolean>("cmd_main_hibernate", { snapshot });
        } catch {
          return false;
        }
      },
    },
    {
      initialMode: get(modeStore),
      doc: deps.doc,
      win: deps.win,
      timers: deps.timers,
    },
  );

  activeShedder = shedder;

  const stopPressure = pressureSource.subscribe((pressure) => {
    shedder.setPressure(pressure);
  });
  const stopMode = modeStore.subscribe((mode) => {
    shedder.setMode(mode);
  });
  stopSubscriptions = () => {
    stopPressure();
    stopMode();
  };

  if (!deps.skipInitialLoad && deps.initialMode === undefined) {
    void loadIdleMemoryMode({ isTauri: tauri, invoke: invokeFn }).catch(() => undefined);
  }

  return shedder;
}
