import { PRIORITY_LABELS } from "./boardDrag";
import type { EnhancementField, Task, TaskCard, TaskStatus } from "./client";

export type BoardLayout = "board" | "list";
export type DueFilter = "all" | "overdue" | "soon" | "none";
export type DueState = "none" | "overdue" | "soon" | "later";

export interface TaskFacet {
  priority: number | "all";
  kind: string | "all";
  owner: string | "all";
  label: string | "all";
  due: DueFilter;
}

export interface FacetOptions {
  kinds: string[];
  owners: string[];
  labels: string[];
}

export interface CardChrome {
  kind: string | null;
  owner: string | null;
  extraRepos: number;
  extraLabels: number;
  due: DueState;
}

const SOON_SECONDS = 7 * 24 * 60 * 60;

export function emptyFacet(): TaskFacet {
  return { priority: "all", kind: "all", owner: "all", label: "all", due: "all" };
}

export function facetActive(facet: TaskFacet): boolean {
  return normalizePriority(facet.priority) !== "all" || facet.kind !== "all" || facet.owner !== "all" || facet.label !== "all" || facet.due !== "all";
}

export function dueState(dueAt: number | null | undefined, nowSec: number): DueState {
  if (dueAt == null || !Number.isFinite(dueAt) || dueAt <= 0) return "none";
  const now = Number.isFinite(nowSec) ? nowSec : 0;
  if (dueAt < now) return "overdue";
  if (dueAt <= now + SOON_SECONDS) return "soon";
  return "later";
}

export function normalizePriority(value: unknown): number | "all" {
  if (value === "all" || value === "" || value == null) return "all";
  const n = typeof value === "number" ? value : Number(value);
  return n === 0 || n === 1 || n === 2 || n === 3 ? n : "all";
}

export function cardMatchesFacet(card: TaskCard, facet: TaskFacet, nowSec: number): boolean {
  const priority = normalizePriority(facet.priority);
  if (priority !== "all" && card.priority !== priority) return false;
  if (facet.kind !== "all" && card.kind !== facet.kind) return false;
  if (facet.owner !== "all") {
    const owner = (card.owner ?? "").trim();
    if (facet.owner === "" ? owner !== "" : owner !== facet.owner) return false;
  }
  if (facet.label !== "all" && !card.labels.includes(facet.label)) return false;
  if (facet.due !== "all") {
    const due = dueState(card.due_at, nowSec);
    if (facet.due === "none" && due !== "none") return false;
    if (facet.due === "overdue" && due !== "overdue") return false;
    if (facet.due === "soon" && due !== "soon") return false;
  }
  return true;
}

export function collectFacetOptions(cards: readonly TaskCard[]): FacetOptions {
  const kinds = new Set<string>();
  const owners = new Set<string>();
  const labels = new Set<string>();
  for (const card of cards) {
    if (card.kind.trim()) kinds.add(card.kind);
    if (card.owner?.trim()) owners.add(card.owner.trim());
    for (const label of card.labels) if (label.trim()) labels.add(label);
  }
  const sort = (values: Iterable<string>) => [...values].sort((a, b) => a.localeCompare(b));
  return { kinds: sort(kinds), owners: sort(owners), labels: sort(labels) };
}

export function allLoadedCards(
  columns: Partial<Record<TaskStatus, { items: readonly TaskCard[] }>>,
): TaskCard[] {
  const seen = new Set<string>();
  const out: TaskCard[] = [];
  for (const page of Object.values(columns)) {
    for (const card of page?.items ?? []) {
      if (seen.has(card.id)) continue;
      seen.add(card.id);
      out.push(card);
    }
  }
  return out;
}

export function cardChrome(
  card: Pick<TaskCard, "kind" | "owner" | "repository_ids" | "labels" | "due_at">,
  nowSec: number,
  labelCap = 2,
): CardChrome {
  const cap = Number.isSafeInteger(labelCap) && labelCap >= 0 ? labelCap : 2;
  return {
    kind: card.kind.trim() ? card.kind : null,
    owner: card.owner?.trim() ? card.owner.trim() : null,
    extraRepos: Math.max(0, card.repository_ids.length - 1),
    extraLabels: Math.max(0, card.labels.length - cap),
    due: dueState(card.due_at, nowSec),
  };
}

export interface HiddenDetail {
  key: string;
  label: string;
  value: string;
  empty: boolean;
}

function line(value: string | null | undefined): { value: string; empty: boolean } {
  const text = (value ?? "").trim();
  return text ? { value: text, empty: false } : { value: "None", empty: true };
}

export function hiddenTaskDetails(
  task: Task,
  repoName: (id: string) => string | undefined,
): HiddenDetail[] {
  const extraRepos = task.repository_ids.slice(1).map((id) => repoName(id) ?? id);
  const locks = (task.locked_fields ?? []).map((field) => field === "title" ? "Title" : "Description");
  const due = task.due_at && task.due_at > 0 ? new Date(task.due_at * 1000).toLocaleString() : "";
  const description = line(task.description);
  const criteria = task.acceptance_criteria.map((item) => item.trim()).filter(Boolean);
  return [
    { key: "description", label: "Description", ...description },
    {
      key: "criteria",
      label: "Acceptance criteria",
      value: criteria.length ? criteria.map((item) => `• ${item}`).join("\n") : "None",
      empty: criteria.length === 0,
    },
    { key: "kind", label: "Type", ...line(task.kind) },
    { key: "owner", label: "Owner", ...line(task.owner) },
    { key: "due", label: "Due", ...line(due) },
    { key: "severity", label: "Severity", ...line(task.severity) },
    { key: "labels", label: "Labels", ...line(task.labels.join(", ")) },
    {
      key: "repos",
      label: "Other repositories",
      value: extraRepos.length ? extraRepos.join(", ") : "None",
      empty: extraRepos.length === 0,
    },
    {
      key: "locks",
      label: "Enhancement locks",
      value: locks.length ? locks.join(", ") : "None",
      empty: locks.length === 0,
    },
  ];
}

export function visibleHiddenDetails(
  details: readonly HiddenDetail[],
  showEmpty: boolean,
): HiddenDetail[] {
  if (!Array.isArray(details)) return [];
  return showEmpty ? [...details] : details.filter((row) => !row.empty);
}

export function dueInputValue(dueAt: number | null | undefined): string {
  if (dueAt == null || !Number.isFinite(dueAt) || dueAt <= 0) return "";
  const date = new Date(dueAt * 1000);
  if (!Number.isFinite(date.getTime())) return "";
  const pad = (value: number) => String(value).padStart(2, "0");
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}T${pad(date.getHours())}:${pad(date.getMinutes())}`;
}

export function parseDueInput(value: unknown): number | null {
  if (typeof value !== "string") return null;
  const text = value.trim();
  if (!text || text.length > 32) return null;
  const ms = Date.parse(text);
  if (!Number.isFinite(ms)) return null;
  const seconds = Math.floor(ms / 1000);
  return Number.isSafeInteger(seconds) && seconds > 0 ? seconds : null;
}

export function canStartEnhanceFromDraft(draft: {
  title?: unknown;
  repository_ids?: unknown;
}): string | null {
  const title = typeof draft.title === "string" ? draft.title.trim() : "";
  if (!title) return "Add a title before Quick Enhance.";
  const repos = Array.isArray(draft.repository_ids) ? draft.repository_ids.filter((id) => typeof id === "string" && id.length > 0) : [];
  if (!repos.length) return "Link a repository before Quick Enhance.";
  return null;
}

export function enhanceableFields(task: Pick<Task, "locked_fields">): EnhancementField[] {
  const locked = new Set(task.locked_fields ?? []);
  return (["title", "description"] as const).filter((field) => !locked.has(field));
}

export function canQuickEnhance(
  task: Pick<Task, "locked_fields">,
  configuration: { provider: string; model: string } | null,
  error: string | null,
): { ok: true; fields: EnhancementField[] } | { ok: false; reason: string } {
  if (error) return { ok: false, reason: error };
  if (!configuration) return { ok: false, reason: "Manvi configuration has not been loaded." };
  const provider = configuration.provider.trim();
  const model = configuration.model.trim();
  if (!provider || !model) return { ok: false, reason: "Manvi has no provider and model selected." };
  const fields = enhanceableFields(task);
  if (fields.length === 0) return { ok: false, reason: "Title and description are locked against enhancement." };
  return { ok: true, fields };
}

export function priorityName(priority: number): string {
  return PRIORITY_LABELS[priority as 0 | 1 | 2 | 3] ?? "Unknown";
}

export function layoutLabel(layout: BoardLayout): string {
  return layout === "list" ? "List" : "Board";
}
