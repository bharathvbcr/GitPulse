/**
 * The one TypeScript owner of the task priority scale: 0 = Urgent, 1 = High,
 * 2 = Normal (the default), 3 = Low. The board, the store, `gitpulse_add_task`
 * and Manvi's suggest table all use it.
 *
 * `parsePriority` has a Rust twin, `parse_priority` in
 * `src-tauri/src/tasks/file_tasks.rs`. `priority.cases.json` is the one corpus
 * both are held to (`taskPriority.test.ts`, `priority_parse_matches_the_shared_corpus`).
 */

export type Priority = 0 | 1 | 2 | 3;

/** What a priority is called on a card, a chip and a screen reader. */
export const PRIORITY_LABELS = ["Urgent", "High", "Normal", "Low"] as const;

/** The priority of anything that does not say. */
export const DEFAULT_PRIORITY: Priority = 2;

/** Lower-case names a reader may type, and the priority each means. */
const NAMES: Readonly<Record<string, Priority>> = Object.assign(Object.create(null), {
  urgent: 0,
  high: 1,
  normal: 2,
  medium: 2,
  low: 3,
});

const PRIORITIES: readonly Priority[] = [0, 1, 2, 3];

function isPriority(value: unknown): value is Priority {
  return typeof value === "number" && PRIORITIES.includes(value as Priority);
}

/** ASCII-only trim and lower-casing: Unicode folding would let lookalikes in. */
function fold(raw: string): string {
  return raw.replace(/^[ \t\r\n]+|[ \t\r\n]+$/g, "").replace(/[A-Z]/g, (c) => c.toLowerCase());
}

/**
 * A priority from what a reader wrote, or null. The whole value must be one
 * form: `0`–`3`, `p0`–`p3`, a name (`urgent`, `high`, `normal`, `medium`,
 * `low`), or the brief form `1 (High)` whose number and name agree. Mixed forms
 * that disagree (`0 (Low)`) or are merely ambiguous (`High (1)`), leading
 * zeros, signs, decimals and trailing words are refused, never guessed at.
 */
export function parsePriority(raw: unknown): Priority | null {
  if (typeof raw === "number") return isPriority(raw) ? raw : null;
  if (typeof raw !== "string") return null;
  const text = fold(raw);
  const plain = /^p?([0-3])$/.exec(text);
  if (plain) return Number(plain[1]) as Priority;
  if (text in NAMES) return NAMES[text];
  const brief = /^([0-3]) \(([a-z]+)\)$/.exec(text);
  if (brief && brief[2] in NAMES && NAMES[brief[2]] === Number(brief[1])) return Number(brief[1]) as Priority;
  return null;
}

/** A priority from a stored number or its decimal string; nothing else. */
export function numericPriority(value: unknown): Priority | null {
  if (typeof value === "number") return isPriority(value) ? value : null;
  return typeof value === "string" && /^[0-3]$/.test(value) ? (Number(value) as Priority) : null;
}

/** The name of a priority; the default's name for anything unreadable. */
export function priorityLabel(value: unknown): string {
  return PRIORITY_LABELS[numericPriority(value) ?? DEFAULT_PRIORITY];
}
