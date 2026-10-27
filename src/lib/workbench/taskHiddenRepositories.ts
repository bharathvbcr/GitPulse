/**
 * Repositories a reader has hidden from the Tasks board.
 *
 * Hiding is a reader preference, not a store write: the repository stays
 * registered, its tasks stay where they are, and its own board still shows
 * them. Three rules keep it honest, and all three live here:
 *
 * 1. A task belongs to its primary repository. That is the repository an
 *    agent runs in and the one the card's checkout flag reads, so a task
 *    that merely also links a hidden repository stays on the board.
 * 2. Naming a repository shows it. Its own board, or a board narrowed to it
 *    (the Filters bar's repository, a single `repo:`), is the reader asking
 *    for exactly those tasks.
 * 3. Hiding is never silent. The board says how many tasks it is keeping
 *    off screen, and whether that count covers every task or only the ones
 *    loaded so far.
 */
import type { Scope, TaskCard } from "./client";
import { plural } from "../format";

/** How many hidden repositories the profile keeps; far past any real sidebar. */
export const MAX_HIDDEN_REPOSITORIES = 256;
/** The longest id kept; a workbench id is far shorter. */
const MAX_ID_LENGTH = 256;

function cleanId(value: unknown): string | null {
  if (typeof value !== "string") return null;
  const id = value.trim();
  return id && id.length <= MAX_ID_LENGTH ? id : null;
}

/** The stored list as the board may use it: distinct ids, in stored order, bounded. */
export function sanitizeHiddenRepositories(value: unknown): string[] {
  if (!Array.isArray(value)) return [];
  const kept = new Set<string>();
  for (const entry of value) {
    const id = cleanId(entry);
    if (id) kept.add(id);
    if (kept.size >= MAX_HIDDEN_REPOSITORIES) break;
  }
  return [...kept];
}

/** Hides `id`, or shows it again when it is already hidden. A full list takes no more. */
export function toggleHiddenRepository(hidden: readonly string[], id: string): string[] {
  const current = sanitizeHiddenRepositories(hidden);
  const target = cleanId(id);
  if (!target) return current;
  if (current.includes(target)) return current.filter((entry) => entry !== target);
  return current.length >= MAX_HIDDEN_REPOSITORIES ? current : [...current, target];
}

/** Whether hiding applies on this board: never on a repository's own. */
export function hidesRepositories(scope: Scope, hidden: readonly string[]): boolean {
  return scope.kind !== "repository" && hidden.length > 0;
}

/**
 * Whether a hidden repository keeps this card off the board.
 *
 * `shownAnyway` is a repository the reader narrowed the board to; its tasks
 * are shown even when it is hidden.
 */
export function hiddenByRepository(
  card: Pick<TaskCard, "primary_repository_id" | "repository_ids">,
  hidden: ReadonlySet<string>,
  shownAnyway: string | null,
): boolean {
  const home = card.primary_repository_id || card.repository_ids[0] || "";
  return home !== "" && home !== shownAnyway && hidden.has(home);
}

export interface HiddenRepositoryCount {
  /** Loaded tasks a hidden repository keeps off the board. */
  tasks: number;
  /** Repositories hidden. */
  repositories: number;
  /** Whether every task of the board was loaded, so `tasks` is the whole count. */
  complete: boolean;
}

const MAX_NAME_LENGTH = 200;

function cleanNames(names: readonly string[]): string[] {
  const clean: string[] = [];
  const seen = new Set<string>();
  for (const name of names) {
    if (typeof name !== "string") continue;
    const trimmed = name.trim();
    if (!trimmed || trimmed.length > MAX_NAME_LENGTH || seen.has(trimmed)) continue;
    seen.add(trimmed);
    clean.push(trimmed);
    if (clean.length >= MAX_HIDDEN_REPOSITORIES) break;
  }
  return clean;
}

function safeCount(value: number): number {
  if (!Number.isFinite(value)) return 0;
  return Math.max(0, Math.min(MAX_HIDDEN_REPOSITORIES, Math.trunc(value)));
}

/**
 * Who is hidden, by name, for the navigator heading and the board note.
 *
 * One or two names are written out. Anything past that, including a hidden
 * repository whose name is not loaded yet, is "and N others", so the line
 * still names someone and still fits the navigator.
 * No names at all returns "" — the note then falls back to a count.
 */
export function hiddenRepositoryPhrase(names: readonly string[], total: number): string {
  const clean = cleanNames(names);
  const count = Math.max(clean.length, safeCount(total));
  if (clean.length === 0) return "";
  const first = clean[0] ?? "";
  const second = clean[1];
  if (second === undefined) {
    const rest = count - 1;
    return rest > 0 ? `${first} and ${plural(rest, "other")}` : first;
  }
  const rest = count - 2;
  if (rest <= 0) return `${first} and ${second}`;
  return `${first}, ${second} and ${plural(rest, "other")}`;
}

/** The line the board shows while repositories are hidden. */
export function hiddenRepositoryNote(count: HiddenRepositoryCount, names: readonly string[] = []): string {
  const named = cleanNames(names);
  const loaded = count.complete ? "" : "loaded ";
  if (count.tasks === 0 && named.length === 0) {
    return `${plural(count.repositories, "repository is", "repositories are")} hidden; none of this board's ${loaded}tasks belong to ${count.repositories === 1 ? "it" : "them"}.`;
  }
  const who = named.length === 0
    ? plural(count.repositories, "hidden repository", "hidden repositories")
    : hiddenRepositoryPhrase(named, count.repositories);
  if (count.tasks === 0) {
    const many = named.length !== 1 || safeCount(count.repositories) !== 1;
    return `${who} ${many ? "are" : "is"} hidden; none of this board's ${loaded}tasks belong to ${many ? "them" : "it"}.`;
  }
  const verb = count.tasks === 1 ? "is" : "are";
  const scope = count.complete ? "" : ", counting only the tasks loaded so far";
  return `${plural(count.tasks, "task")} from ${who} ${verb} not on this board${scope}.`;
}
