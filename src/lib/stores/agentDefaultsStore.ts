import { setTerminalSessionLimit } from "../terminal/sessionLimit";
import { writable, type Readable } from "svelte/store";
import { invoke } from "../ipc/invoke";
import { isTauri } from "../platform";
import {
  EFFORT_LEVELS,
  EMPTY_AGENT_DEFAULTS,
  MODEL_FIELDS,
  MODEL_FIELDS_BY_LAUNCHER,
  PERMISSION_LAUNCHERS,
  PERMISSION_MODES,
  isEffortLevel,
  sanitizeAgentDefaults,
  type AgentDefaults,
  type EffortLevel,
  type ModelField,
  type PermissionMode,
} from "../terminal/agentDefaults";
import { LAUNCHERS } from "../terminal/tabs";
import type { LauncherKind } from "../terminal/tabs";

/**
 * The defaults a terminal tab launches an agent CLI with.
 *
 * Separate from `sessionAlertsStore` on purpose. That one answers "may this
 * session interrupt me"; this one answers "how much authority does it start
 * with". They are both host-scoped and both read before a spawn, which is
 * what makes them look alike — but a notification setting turning off must
 * never be able to change what a command line says, and merging them would
 * put both behind one save.
 *
 * `modes` and `launchers` travel with the settings rather than being written
 * down here: a chooser offering a mode this build cannot expand would be
 * offering a launch that fails at spawn.
 */

/**
 * The wire shape, mirroring `AgentDefaultsView` in Rust field for field.
 *
 * `modes` and `launchers` are plain strings here and narrowed by {@link adopt}
 * rather than asserted. That is not pedantry: the app and the binary can be
 * different versions, and in that window the backend can legitimately name a
 * mode this build has no label for. Typing the wire as already-narrowed would
 * be a claim about the other side of an IPC boundary that nothing checks.
 */
export interface AgentDefaultsView {
  defaults: AgentDefaults;
  modes: string[];
  launchers: string[];
  model_fields?: Record<string, string[]>;
  effort_levels?: string[];
  model_listing?: Record<string, string>;
}

/** The same answer after narrowing, which is what the app renders. */
export interface AgentDefaultsState {
  defaults: AgentDefaults;
  modes: readonly PermissionMode[];
  launchers: readonly LauncherKind[];
  /** The model fields each launcher takes; a launcher absent takes none. */
  modelFields: Readonly<Partial<Record<LauncherKind, readonly ModelField[]>>>;
  effortLevels: readonly EffortLevel[];
  /**
   * Launchers whose models can be listed (`cmd_agent_models`), and how:
   * `listed` by the CLI itself, `known` from its aliases and the user's
   * settings. A launcher absent has no list, and no list is offered for it.
   */
  modelListing: Readonly<Partial<Record<LauncherKind, ModelListing>>>;
}

export type ModelListing = "listed" | "known";

/**
 * What the panel shows before the backend answers, and what a browser preview
 * shows for good. Mirrors `AgentDefaults::default` in Rust.
 */
export const DEFAULT_AGENT_DEFAULTS_VIEW: AgentDefaultsState = {
  defaults: EMPTY_AGENT_DEFAULTS,
  modes: PERMISSION_MODES,
  launchers: PERMISSION_LAUNCHERS,
  modelFields: MODEL_FIELDS_BY_LAUNCHER,
  effortLevels: EFFORT_LEVELS,
  modelListing: {},
};

const store = writable<AgentDefaultsState>(DEFAULT_AGENT_DEFAULTS_VIEW);

// The terminal session limit is one of these defaults and is what the session
// registry and the terminal panel stop at. Mirrored from every state this
// store takes — loaded, saved or reset — so there is no second path to forget.
store.subscribe((state) => setTerminalSessionLimit(state.defaults.max_terminal_sessions));

let loaded = false;
let inFlight: Promise<AgentDefaultsState> | null = null;

/**
 * Narrows whatever the backend sent to what this build can act on.
 *
 * The backend validates too, and this is not redundant with it: the two
 * disagree exactly when the app and the binary are different versions, and in
 * that window a mode the UI cannot describe must not reach a chooser.
 */
function adopt(raw: Partial<AgentDefaultsView> | null | undefined): AgentDefaultsState {
  const launchers = (Array.isArray(raw?.launchers) ? raw.launchers : PERMISSION_LAUNCHERS).filter(
    (launcher): launcher is LauncherKind => PERMISSION_LAUNCHERS.includes(launcher as LauncherKind),
  );
  const modes = (Array.isArray(raw?.modes) ? raw.modes : PERMISSION_MODES).filter(
    (mode): mode is PermissionMode => PERMISSION_MODES.includes(mode as PermissionMode),
  );
  const modelFields = adoptModelFields(raw?.model_fields);
  const effortLevels = (Array.isArray(raw?.effort_levels) ? raw.effort_levels : EFFORT_LEVELS).filter(isEffortLevel);
  return {
    defaults: sanitizeAgentDefaults(raw?.defaults, launchers, modelFields),
    modes: modes.length ? modes : PERMISSION_MODES,
    launchers: launchers.length ? launchers : PERMISSION_LAUNCHERS,
    modelFields,
    effortLevels: effortLevels.length ? effortLevels : EFFORT_LEVELS,
    modelListing: adoptModelListing(raw?.model_listing, modelFields),
  };
}

/**
 * Which launchers can list their models. Only ones this build renders and
 * that take a model; an absent or malformed map is none — an older backend
 * has no `cmd_agent_models` to call.
 */
function adoptModelListing(
  raw: unknown,
  modelFields: Partial<Record<LauncherKind, readonly ModelField[]>>,
): Partial<Record<LauncherKind, ModelListing>> {
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) return {};
  const out: Partial<Record<LauncherKind, ModelListing>> = {};
  for (const [launcher, how] of Object.entries(raw)) {
    if (!modelFields[launcher as LauncherKind]?.includes("model")) continue;
    if (how === "listed" || how === "known") out[launcher as LauncherKind] = how;
  }
  return out;
}

/**
 * The backend's per-launcher model fields, narrowed to launchers and fields
 * this build can render. An older backend sends none, and then the panel
 * shows no model controls at all rather than offering ones that backend would
 * refuse to save — the fallback table is only for a page with no backend.
 */
function adoptModelFields(raw: unknown): Partial<Record<LauncherKind, readonly ModelField[]>> {
  if (raw === undefined) return isTauri() ? {} : MODEL_FIELDS_BY_LAUNCHER;
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) return {};
  const known = new Set<string>(LAUNCHERS.map((entry) => entry.kind));
  const out: Partial<Record<LauncherKind, readonly ModelField[]>> = {};
  for (const [launcher, fields] of Object.entries(raw)) {
    if (!known.has(launcher) || !Array.isArray(fields)) continue;
    const kept = MODEL_FIELDS.filter((field) => fields.includes(field));
    if (kept.length) out[launcher as LauncherKind] = kept;
  }
  return out;
}

/**
 * Loads once and caches. Every agent tab reads this before it spawns, so it
 * is on the hot path of opening a tab; a per-session round trip would delay
 * every launch behind an answer that cannot change between them.
 */
export async function loadAgentDefaults(): Promise<AgentDefaultsState> {
  if (loaded) return current();
  if (inFlight) return inFlight;
  if (!isTauri()) {
    loaded = true;
    return current();
  }
  inFlight = invoke<AgentDefaultsView>("cmd_agent_defaults")
    .then((view) => {
      const adopted = adopt(view);
      store.set(adopted);
      loaded = true;
      return adopted;
    })
    // A failed read keeps the shipped defaults, which means "every CLI on its
    // own". Deliberately not an error the launch path surfaces: a settings
    // file that cannot be read must not be the reason a terminal will not open.
    .catch(() => current())
    .finally(() => {
      inFlight = null;
    });
  return inFlight;
}

export async function saveAgentDefaults(defaults: AgentDefaults): Promise<AgentDefaultsState> {
  if (!isTauri()) {
    const next = { ...current(), defaults };
    store.set(next);
    return next;
  }
  const view = adopt(await invoke<AgentDefaultsView>("cmd_agent_defaults_save", { defaults }));
  store.set(view);
  loaded = true;
  return view;
}

let snapshot: AgentDefaultsState = DEFAULT_AGENT_DEFAULTS_VIEW;
store.subscribe((view) => {
  snapshot = view;
});

function current(): AgentDefaultsState {
  return snapshot;
}

/** Synchronous read for a code path that cannot await, such as a spawn. */
export function agentDefaults(): AgentDefaultsState {
  return snapshot;
}

export const agentDefaultsStore: Readable<AgentDefaultsState> = { subscribe: store.subscribe };

/** Test seam: forget the cached answer so a suite can drive a fresh load. */
export function resetAgentDefaultsForTests(): void {
  loaded = false;
  inFlight = null;
  store.set(DEFAULT_AGENT_DEFAULTS_VIEW);
}
