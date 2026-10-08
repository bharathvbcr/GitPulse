/**
 * Saved views, swimlanes and work-in-progress limits for the Tasks board.
 *
 * All three are reader preferences kept per board — the global board, each
 * workspace's and each repository's — so `interfaceStore` persists them under
 * a scope key and the board reads them back. They are not in the task store:
 * its workspace record accepts a fixed field list, and none of these change
 * what a task is, only how a board shows it.
 *
 * Everything here reads stored input as hostile, as `taskView.ts` does: a
 * value from `localStorage` may come from another build, be hand-edited or be
 * cut short. Nothing throws, and every reader returns a shape the board can
 * draw. Sizes are bounded, so a runaway writer cannot grow the profile.
 */

import { STATUSES, asTaskStatus, type TaskStatus } from "../workbench/vocabulary";
import { emptyFacet, normalizePriority, type DueFilter, type TaskFacet } from "../workbench/taskOrganize";
import {
  isBoardLayout,
  isTaskDensity,
  sanitizeCardFields,
  sanitizeHiddenStatuses,
  type BoardLayout,
  type TaskCardField,
  type TaskDensity,
} from "./taskView";

/** How cards are grouped into horizontal lanes. */
export const SWIMLANES = ["none", "status", "owner", "label"] as const;
export type Swimlane = (typeof SWIMLANES)[number];
export const SWIMLANE_LABELS: Record<Swimlane, string> = {
  none: "No lanes",
  status: "Status",
  owner: "Owner",
  label: "Label",
};

export function isSwimlane(value: unknown): value is Swimlane {
  return SWIMLANES.some((lane) => lane === value);
}

/**
 * The lanes the board actually draws.
 *
 * On the board, columns already are statuses, so status lanes would put each
 * card in the one cell where its lane and its column agree — a diagonal. They
 * apply to the list, where they group rows; the board draws no lanes for them
 * and the View menu says so rather than offering a choice that does nothing.
 */
export function effectiveSwimlane(layout: BoardLayout, lane: Swimlane): Swimlane {
  return layout === "board" && lane === "status" ? "none" : lane;
}

/** The highest limit a column can carry; anything above is a typo, not a limit. */
export const MAX_WIP_LIMIT = 999;

export type WipLimits = Partial<Record<TaskStatus, number>>;

/** A limit from storage or an input box, or null for "no limit". */
export function sanitizeWipLimit(value: unknown): number | null {
  const n = typeof value === "number" ? value : typeof value === "string" && value.trim() ? Number(value) : NaN;
  return Number.isSafeInteger(n) && n >= 1 && n <= MAX_WIP_LIMIT ? n : null;
}

export function sanitizeWipLimits(value: unknown): WipLimits {
  if (!value || typeof value !== "object" || Array.isArray(value)) return {};
  const out: WipLimits = {};
  for (const [key, raw] of Object.entries(value)) {
    const status = asTaskStatus(key);
    const limit = sanitizeWipLimit(raw);
    if (status && limit !== null) out[status] = limit;
  }
  return out;
}

export type WipState = "under" | "at" | "over";

/**
 * Where a column stands against its limit, or null when it has none.
 *
 * `total` must be the store's count for the column, not the cards on its
 * loaded page: a column showing thirty of forty tasks is over a limit of
 * thirty-five even though only thirty are on screen.
 */
export function wipState(total: number | null | undefined, limit: number | null | undefined): WipState | null {
  if (limit == null || !Number.isFinite(limit) || limit < 1) return null;
  const count = typeof total === "number" && Number.isFinite(total) ? Math.max(0, Math.trunc(total)) : 0;
  return count > limit ? "over" : count === limit ? "at" : "under";
}

/** Everything a saved view puts back. */
export interface TaskViewSnapshot {
  layout: BoardLayout;
  density: TaskDensity;
  swimlane: Swimlane;
  hiddenColumns: TaskStatus[];
  cardFields: TaskCardField[];
  facet: TaskFacet;
  search: string;
}

export interface SavedTaskView extends TaskViewSnapshot {
  id: string;
  name: string;
}

export const MAX_VIEW_NAME = 60;
export const MAX_VIEWS_PER_BOARD = 20;
export const MAX_BOARDS = 100;
const MAX_SEARCH = 512;
const MAX_FACET_TEXT = 300;
const DUE_FILTERS: readonly DueFilter[] = ["all", "overdue", "soon", "none"];

/** A view name as the reader typed it, trimmed and bounded, or "" when unusable. */
export function viewName(value: unknown): string {
  if (typeof value !== "string") return "";
  const text = value.replace(/\s+/g, " ").trim();
  return text.length > 0 && [...text].length <= MAX_VIEW_NAME ? text : "";
}

function facetText(value: unknown): string | "all" {
  if (value === "all") return "all";
  return typeof value === "string" && value.length <= MAX_FACET_TEXT ? value : "all";
}

export function sanitizeFacet(value: unknown): TaskFacet {
  if (!value || typeof value !== "object" || Array.isArray(value)) return emptyFacet();
  const raw = value as Record<string, unknown>;
  return {
    priority: normalizePriority(raw.priority),
    kind: facetText(raw.kind),
    owner: facetText(raw.owner),
    label: facetText(raw.label),
    due: DUE_FILTERS.find((due) => due === raw.due) ?? "all",
  };
}

export function sanitizeSnapshot(value: unknown): TaskViewSnapshot {
  const raw = value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : {};
  return {
    layout: isBoardLayout(raw.layout) ? raw.layout : "board",
    density: isTaskDensity(raw.density) ? raw.density : "comfortable",
    swimlane: isSwimlane(raw.swimlane) ? raw.swimlane : "none",
    hiddenColumns: sanitizeHiddenStatuses(raw.hiddenColumns),
    cardFields: sanitizeCardFields(raw.cardFields),
    facet: sanitizeFacet(raw.facet),
    search: typeof raw.search === "string" ? raw.search.slice(0, MAX_SEARCH) : "",
  };
}

/** A stored view, or null when it has no usable id or name. */
export function sanitizeSavedView(value: unknown): SavedTaskView | null {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  const raw = value as Record<string, unknown>;
  const id = typeof raw.id === "string" && /^[A-Za-z0-9_-]{1,64}$/.test(raw.id) ? raw.id : "";
  const name = viewName(raw.name);
  if (!id || !name) return null;
  return { id, name, ...sanitizeSnapshot(raw) };
}

export interface BoardPrefs {
  views: SavedTaskView[];
  wip: WipLimits;
}

export function emptyBoardPrefs(): BoardPrefs {
  return { views: [], wip: {} };
}

/** One board's views (unique ids and names, at most twenty) and limits. */
export function sanitizeBoardPrefs(value: unknown): BoardPrefs {
  if (!value || typeof value !== "object" || Array.isArray(value)) return emptyBoardPrefs();
  const raw = value as Record<string, unknown>;
  const views: SavedTaskView[] = [];
  const ids = new Set<string>(), names = new Set<string>();
  for (const entry of Array.isArray(raw.views) ? raw.views : []) {
    const view = sanitizeSavedView(entry);
    if (!view || ids.has(view.id) || names.has(view.name.toLowerCase())) continue;
    ids.add(view.id); names.add(view.name.toLowerCase());
    views.push(view);
    if (views.length >= MAX_VIEWS_PER_BOARD) break;
  }
  return { views, wip: sanitizeWipLimits(raw.wip) };
}

/** Which board a preference belongs to. */
export type BoardScope = { kind: "global" } | { kind: "workspace" | "repository"; id: string };

export function boardKey(scope: BoardScope): string {
  return scope.kind === "global" ? "global" : `${scope.kind}:${scope.id}`;
}

function isBoardKey(key: string): boolean {
  return key === "global" || /^(workspace|repository):[A-Za-z0-9_-]{1,128}$/.test(key);
}

/** Every board's preferences, dropping empty entries and unknown keys. */
export function sanitizeBoards(value: unknown): Record<string, BoardPrefs> {
  if (!value || typeof value !== "object" || Array.isArray(value)) return {};
  const out: Record<string, BoardPrefs> = {};
  let count = 0;
  for (const [key, raw] of Object.entries(value)) {
    if (!isBoardKey(key)) continue;
    const prefs = sanitizeBoardPrefs(raw);
    if (!prefs.views.length && !Object.keys(prefs.wip).length) continue;
    out[key] = prefs;
    if (++count >= MAX_BOARDS) break;
  }
  return out;
}

export function boardPrefs(boards: Record<string, BoardPrefs>, key: string): BoardPrefs {
  return boards[key] ?? emptyBoardPrefs();
}

function withBoard(boards: Record<string, BoardPrefs>, key: string, prefs: BoardPrefs): Record<string, BoardPrefs> {
  const next = { ...boards };
  if (!prefs.views.length && !Object.keys(prefs.wip).length) delete next[key];
  else next[key] = prefs;
  return sanitizeBoards(next);
}

export type SaveViewResult =
  | { ok: true; boards: Record<string, BoardPrefs>; view: SavedTaskView; replaced: boolean }
  | { ok: false; reason: string };

/**
 * Save the board's current view under `name`.
 *
 * A name the board already has replaces that view (keeping its id), which is
 * how a reader updates one; names compare without case, as a reader reads
 * them. A twenty-first view is refused out loud rather than silently dropping
 * an old one.
 */
export function saveView(
  boards: Record<string, BoardPrefs>,
  key: string,
  name: unknown,
  snapshot: TaskViewSnapshot,
  newId: () => string,
): SaveViewResult {
  if (!isBoardKey(key)) return { ok: false, reason: "This board cannot hold saved views." };
  const clean = viewName(name);
  if (!clean) return { ok: false, reason: `Name the view (up to ${MAX_VIEW_NAME} characters).` };
  const prefs = boardPrefs(boards, key);
  const existing = prefs.views.find((view) => view.name.toLowerCase() === clean.toLowerCase());
  if (!existing && prefs.views.length >= MAX_VIEWS_PER_BOARD) {
    return { ok: false, reason: `A board keeps at most ${MAX_VIEWS_PER_BOARD} saved views. Delete one first.` };
  }
  const view: SavedTaskView = { id: existing?.id ?? newId().replace(/[^A-Za-z0-9_-]/g, "").slice(0, 64), name: clean, ...sanitizeSnapshot(snapshot) };
  if (!view.id) return { ok: false, reason: "Could not name the view." };
  const views = existing ? prefs.views.map((entry) => entry.id === existing.id ? view : entry) : [...prefs.views, view];
  return { ok: true, boards: withBoard(boards, key, { ...prefs, views }), view, replaced: Boolean(existing) };
}

export function deleteView(boards: Record<string, BoardPrefs>, key: string, id: string): Record<string, BoardPrefs> {
  const prefs = boardPrefs(boards, key);
  return withBoard(boards, key, { ...prefs, views: prefs.views.filter((view) => view.id !== id) });
}

export function setWipLimit(boards: Record<string, BoardPrefs>, key: string, status: TaskStatus, limit: unknown): Record<string, BoardPrefs> {
  if (!isBoardKey(key) || !STATUSES.includes(status)) return boards;
  const prefs = boardPrefs(boards, key);
  const wip = { ...prefs.wip };
  const clean = sanitizeWipLimit(limit);
  if (clean === null) delete wip[status];
  else wip[status] = clean;
  return withBoard(boards, key, { ...prefs, wip });
}

/** Whether the board on screen is showing exactly this saved view. */
export function sameSnapshot(a: TaskViewSnapshot, b: TaskViewSnapshot): boolean {
  return JSON.stringify(sanitizeSnapshot(a)) === JSON.stringify(sanitizeSnapshot(b));
}
