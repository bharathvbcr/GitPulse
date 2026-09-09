import type { Search } from "@lucide/svelte";
import { fuzzyMatch } from "../branches/groupBranches";

export type PaletteMode = "commands" | "files" | "repositories" | "commits" | "branches" | "symbols" | "workspace" | "help";
export interface PaletteItem {
  id: string;
  label: string;
  description?: string;
  keywords?: string;
  icon: typeof Search;
  filePath?: string;
  shortcut?: string;
  category: string;
  disabledReason?: string;
  /** A mode change stays open; a dialog handoff closes before taking focus. */
  keepOpen?: boolean;
  closeBefore?: boolean;
  action: () => unknown | Promise<unknown>;
}

export const PAGE_SIZE = 50;
export const MAX_QUERY_LENGTH = 256;
export const PALETTE_MODES: ReadonlyArray<{ mode: PaletteMode; prefix: string; label: string; hint: string }> = [
  { mode: "commands", prefix: ">", label: "Commands", hint: "Search actions, settings and views" },
  { mode: "files", prefix: "/", label: "Files", hint: "Search repository file names and paths" },
  { mode: "repositories", prefix: "%", label: "Repositories", hint: "Switch tabs or open a recent repository" },
  { mode: "commits", prefix: "#", label: "Commits", hint: "Search loaded history by hash, message or author" },
  { mode: "branches", prefix: "@", label: "Branches", hint: "Search local and remote branches" },
  { mode: "symbols", prefix: ":", label: "Symbols", hint: "Search the active repository's code map" },
  { mode: "workspace", prefix: "::", label: "Workspace", hint: "Search registered repositories; append ~ for TF-IDF name ranking" },
  { mode: "help", prefix: "?", label: "Help", hint: "Discover search modes and keyboard shortcuts" },
];

export function parsePaletteQuery(query: string) {
  const trimmed = query.trim().slice(0, MAX_QUERY_LENGTH);
  let mode: PaletteMode = "commands";
  let prefix = "";
  if (trimmed.startsWith("::")) { mode = "workspace"; prefix = "::"; }
  else if (trimmed.startsWith("#")) { mode = "commits"; prefix = "#"; }
  else if (trimmed.startsWith("@")) { mode = "branches"; prefix = "@"; }
  else if (trimmed.startsWith(":")) { mode = "symbols"; prefix = ":"; }
  else if (trimmed.startsWith("?")) { mode = "help"; prefix = "?"; }
  else if (trimmed.startsWith("/")) { mode = "files"; prefix = "/"; }
  else if (trimmed.startsWith("%")) { mode = "repositories"; prefix = "%"; }
  else if (trimmed.startsWith(">")) prefix = ">";
  let text = trimmed.slice(prefix.length).trim();
  const semantic = mode === "workspace" && text.endsWith("~");
  if (semantic) text = text.slice(0, -1).trim();
  return { mode, text, semantic };
}

export interface Usage { count: number; lastUsed: number }
export type Frecency = ReadonlyMap<string, Usage>;
export interface PaletteStorage { getItem(key: string): string | null; setItem(key: string, value: string): void }
export const FRECENCY_KEY = "gitpulse_palette_frecency";
const MAX_HISTORY = 120;

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

/** Accept the old count-only format, but never trust persisted value types. */
export function readFrecency(storage: PaletteStorage | null, now = Date.now()): Frecency {
  try {
    const raw = storage?.getItem(FRECENCY_KEY);
    if (!raw || raw.length > 100_000) return new Map();
    const data: unknown = JSON.parse(raw);
    if (!isRecord(data)) return new Map();
    const entries: Array<[string, Usage]> = [];
    for (const [id, value] of Object.entries(data)) {
      const count = typeof value === "number" ? value : isRecord(value) ? value.count : null;
      const lastUsed = isRecord(value) ? value.lastUsed : 0;
      if (!id || id.length > 2048 || typeof count !== "number" || !Number.isFinite(count) || count <= 0 ||
        typeof lastUsed !== "number" || !Number.isFinite(lastUsed) || lastUsed < 0) continue;
      entries.push([id, { count: Math.min(Math.floor(count), 10_000), lastUsed: Math.min(lastUsed, now) }]);
    }
    return new Map(entries.sort((a, b) => b[1].lastUsed - a[1].lastUsed).slice(0, MAX_HISTORY));
  } catch { return new Map(); }
}

export function recordFrecency(history: Frecency, id: string, storage: PaletteStorage | null, now = Date.now()): Frecency {
  const next = new Map(history);
  next.set(id, { count: Math.min((history.get(id)?.count ?? 0) + 1, 10_000), lastUsed: now });
  const bounded = new Map([...next].sort((a, b) => b[1].lastUsed - a[1].lastUsed).slice(0, MAX_HISTORY));
  // History is optional. Unavailable storage must not prevent an action.
  try { storage?.setItem(FRECENCY_KEY, JSON.stringify(Object.fromEntries(bounded))); } catch { /* keep the session history */ }
  return bounded;
}

function usageScore(usage: Usage | undefined, now: number): number {
  if (!usage) return 0;
  const days = Math.max(0, now - usage.lastUsed) / 86_400_000;
  return Math.log2(usage.count + 1) + 12 / (1 + days);
}

/** Match all tokens, weight labels above metadata, and never let usage beat relevance. */
export function rankItems<T extends Pick<PaletteItem, "id" | "label" | "description" | "keywords" | "category" | "disabledReason">>(
  items: readonly T[], query: string, history: Frecency = new Map(), now = Date.now(),
): T[] {
  const text = query.trim().toLowerCase().slice(0, MAX_QUERY_LENGTH);
  const tokens = text.split(/\s+/).filter(Boolean);
  const seen = new Set<string>();
  const ranked: Array<{ item: T; score: number; order: number }> = [];
  items.forEach((item, order) => {
    if (seen.has(item.id)) return;
    seen.add(item.id);
    const label = item.label.toLowerCase();
    const metadata = `${item.description ?? ""} ${item.keywords ?? ""} ${item.category}`.toLowerCase();
    let score = 0;
    for (const token of tokens) {
      if (label === token) score += 120;
      else if (label.startsWith(token)) score += 100;
      else if (label.includes(token)) score += 80 + 15 / (1 + label.indexOf(token));
      else if (metadata.includes(token)) score += 50;
      else if (fuzzyMatch(token, label)) score += 20;
      else return;
    }
    if (text && label === text) score += 200;
    // Empty-query suggestions prefer executable actions. Search keeps unavailable
    // commands discoverable so their explanation can be read.
    if (!text && item.disabledReason) score -= 1000;
    ranked.push({ item, score, order });
  });
  return ranked.sort((a, b) => b.score - a.score ||
    usageScore(history.get(b.item.id), now) - usageScore(history.get(a.item.id), now) || a.order - b.order).map(row => row.item);
}

export function actionFailure(result: unknown): string | null {
  if (isRecord(result) && result.ok === false) {
    return typeof result.error === "string" && result.error ? result.error : "The action was cancelled or did not complete.";
  }
  return null;
}
