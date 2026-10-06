/**
 * The models an agent CLI can be started with, as the CLI says.
 *
 * Suggestions for the model settings row, never a constraint: the stored
 * value stays free text, because a list is only as complete as its source.
 * Antigravity lists its models (`agy models`, per account, over the network);
 * Claude Code publishes no list, so its answer is its own aliases plus the
 * models the user's Claude settings name. `cmd_agent_models` in Rust owns
 * both; this module narrows the answer and keeps one request per launcher in
 * flight.
 *
 * A listing that failed arrives as `error` beside an empty list, and stays
 * that way here: "could not ask" must never render as "has no models".
 */
import { writable, type Readable } from "svelte/store";
import { invoke } from "../ipc/invoke";
import { isModelId } from "./agentDefaults";
import type { LauncherKind } from "./tabs";

/** Mirrors `agent_models::AgentModelOption`. */
export interface AgentModelOption {
  id: string;
  label: string | null;
  source: string;
}

/** Mirrors `agent_models::AgentModelCatalog`. */
export interface AgentModelCatalog {
  launcher: string;
  models: AgentModelOption[];
  listing: string;
  command: string | null;
  fetched_at: number;
  cached: boolean;
  skipped: number;
  truncated: boolean;
  error: string | null;
}

export type CatalogState =
  | { status: "loading"; previous: AgentModelCatalog | null }
  | { status: "ready"; catalog: AgentModelCatalog }
  | { status: "failed"; message: string; previous: AgentModelCatalog | null };

/** Mirrors `agent_models::MAX_CATALOG_MODELS`. */
export const MAX_CATALOG_MODELS = 256;

const store = writable<Partial<Record<LauncherKind, CatalogState>>>({});
const inFlight = new Map<LauncherKind, Promise<CatalogState>>();
let snapshot: Partial<Record<LauncherKind, CatalogState>> = {};
store.subscribe((value) => {
  snapshot = value;
});

export const agentModelCatalogs: Readable<Partial<Record<LauncherKind, CatalogState>>> = { subscribe: store.subscribe };

/**
 * An answer narrowed to what this build can render: entries that are not a
 * model name are dropped (and counted with the backend's own skips), the list
 * is bounded, and an answer of the wrong shape is `null`.
 */
export function adoptCatalog(raw: unknown, launcher: LauncherKind): AgentModelCatalog | null {
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) return null;
  const value = raw as Record<string, unknown>;
  if (value.launcher !== launcher || !Array.isArray(value.models)) return null;
  let skipped = typeof value.skipped === "number" && Number.isInteger(value.skipped) && value.skipped >= 0 ? value.skipped : 0;
  const models: AgentModelOption[] = [];
  const seen = new Set<string>();
  for (const entry of value.models) {
    const item = entry as Record<string, unknown> | null;
    if (!item || typeof item !== "object" || !isModelId(item.id)) {
      skipped += 1;
      continue;
    }
    if (seen.has(item.id) || models.length >= MAX_CATALOG_MODELS) continue;
    seen.add(item.id);
    models.push({
      id: item.id,
      label: typeof item.label === "string" && item.label.length <= 120 ? item.label : null,
      source: typeof item.source === "string" ? item.source : "cli",
    });
  }
  const error = typeof value.error === "string" && value.error.trim() ? value.error : null;
  return {
    launcher,
    models,
    listing: value.listing === "listed" ? "listed" : "known",
    command: typeof value.command === "string" ? value.command : null,
    fetched_at: typeof value.fetched_at === "number" && Number.isFinite(value.fetched_at) ? value.fetched_at : 0,
    cached: value.cached === true,
    skipped,
    truncated: value.truncated === true || value.models.length > MAX_CATALOG_MODELS,
    // A "successful" answer with nothing usable in it is not a listing.
    error: error ?? (models.length === 0 ? "The model list came back empty." : null),
  };
}

function previousOf(state: CatalogState | undefined): AgentModelCatalog | null {
  if (!state) return null;
  return state.status === "ready" ? state.catalog : state.previous;
}

/**
 * Asks for `launcher`'s models. Concurrent asks share one request; a refresh
 * while one is in flight waits for it rather than starting a second. The last
 * good list stays visible while a new one loads or after one fails.
 */
export function loadAgentModels(launcher: LauncherKind, refresh = false): Promise<CatalogState> {
  const pending = inFlight.get(launcher);
  if (pending) return pending;
  const previous = previousOf(snapshot[launcher]);
  store.update((all) => ({ ...all, [launcher]: { status: "loading", previous } }));
  const request = invoke<unknown>("cmd_agent_models", { launcher, refresh })
    .then((raw): CatalogState => {
      const catalog = adoptCatalog(raw, launcher);
      if (!catalog) return { status: "failed", message: "The model list was not in a shape this build can read.", previous };
      // An answer that carries an error is a failed listing however it
      // arrived: it keeps the last good list rather than replacing it with
      // an empty one, exactly as a thrown failure does.
      if (catalog.error) return { status: "failed", message: catalog.error, previous };
      return { status: "ready", catalog };
    })
    .catch((err: unknown): CatalogState => ({ status: "failed", message: err instanceof Error ? err.message : String(err), previous }))
    .then((state) => {
      store.update((all) => ({ ...all, [launcher]: state }));
      return state;
    })
    .finally(() => {
      inFlight.delete(launcher);
    });
  inFlight.set(launcher, request);
  return request;
}

/**
 * Whether `model` is one the listing knows. `null` when there is no
 * successful listing to judge by — an unknown answer, not a "no".
 */
export function listedModel(state: CatalogState | undefined, model: string | undefined): boolean | null {
  if (!model || !state || state.status !== "ready" || state.catalog.error) return null;
  return state.catalog.models.some((entry) => entry.id === model);
}

/** Test seam: forget every answer and request. */
export function resetAgentModelCatalogsForTests(): void {
  inFlight.clear();
  store.set({});
}
