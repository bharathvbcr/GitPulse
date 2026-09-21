import { writable, type Readable } from "svelte/store";
import { invoke } from "@tauri-apps/api/core";
import { isTauri } from "../platform";
import {
  EMPTY_AGENT_DEFAULTS,
  PERMISSION_LAUNCHERS,
  PERMISSION_MODES,
  sanitizeAgentDefaults,
  type AgentDefaults,
  type PermissionMode,
} from "../terminal/agentDefaults";
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
}

/** The same answer after narrowing, which is what the app renders. */
export interface AgentDefaultsState {
  defaults: AgentDefaults;
  modes: readonly PermissionMode[];
  launchers: readonly LauncherKind[];
}

/**
 * What the panel shows before the backend answers, and what a browser preview
 * shows for good. Mirrors `AgentDefaults::default` in Rust.
 */
export const DEFAULT_AGENT_DEFAULTS_VIEW: AgentDefaultsState = {
  defaults: EMPTY_AGENT_DEFAULTS,
  modes: PERMISSION_MODES,
  launchers: PERMISSION_LAUNCHERS,
};

const store = writable<AgentDefaultsState>(DEFAULT_AGENT_DEFAULTS_VIEW);

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
  return {
    defaults: sanitizeAgentDefaults(raw?.defaults, launchers),
    modes: modes.length ? modes : PERMISSION_MODES,
    launchers: launchers.length ? launchers : PERMISSION_LAUNCHERS,
  };
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
