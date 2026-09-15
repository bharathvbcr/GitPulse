/**
 * The task sheet's due date: calendar arithmetic and how a deadline reads.
 *
 * This module owns none of the two things that already have owners. The
 * grammar of a date *word* (`friday`, `tomorrow`, `+2w`, `2026-02-03`, and now
 * a trailing clock) belongs to `parseQuickAddDue` in `taskQuickAdd`, and how
 * urgent a deadline is belongs to `dueState` in `taskOrganize` — the board
 * sorts and filters on it, so the sheet must not answer that question a second
 * way. What is left, and what lives here, is the month grid, the split and
 * join of a timestamp against a `<input type="time">`, and the phrasing.
 *
 * All of it is local-time, because a due date is a day in the reader's week,
 * not an instant: the sheet must not show "Sep 18" for a timestamp the board
 * files under the 19th.
 */

import { dueState } from "./taskOrganize";
import { parseQuickAddDue, QUICK_ADD_DUE_HOUR } from "./taskQuickAdd";

/** A calendar day, in the fields `Date` uses: `month` is 0-11. */
export interface DueDay {
  year: number;
  month: number;
  day: number;
}

/** One cell of the month grid. */
export interface DueCell extends DueDay {
  /** Stable across re-renders, so a keyed `{#each}` never re-creates a cell. */
  key: string;
  /** Day of the month, as drawn. */
  label: number;
  /** False for the leading and trailing days borrowed from the neighbours. */
  inMonth: boolean;
  isToday: boolean;
  isSelected: boolean;
}

/** The month a grid is drawn for. */
export interface DueView {
  year: number;
  month: number;
}

const DAY_MS = 86_400_000;
/** Six weeks: every month fits, so the grid never changes height mid-year. */
const GRID_CELLS = 42;

function localDay(date: Date): DueDay {
  return { year: date.getFullYear(), month: date.getMonth(), day: date.getDate() };
}

function pad(value: number): string {
  return String(value).padStart(2, "0");
}

function sameDay(a: DueDay | null, b: DueDay): boolean {
  return a !== null && a.year === b.year && a.month === b.month && a.day === b.day;
}

function validDue(dueAt: number | null | undefined): dueAt is number {
  return dueAt != null && Number.isFinite(dueAt) && dueAt > 0;
}

/**
 * Split a stored deadline into the day and the clock the controls edit.
 *
 * `null` for "no due date" — and for a stored value that is not a usable
 * timestamp, because a picker that opens on 1970 because the field held
 * garbage is worse than one that opens on today.
 */
export function duePartsOf(dueAt: number | null | undefined): { day: DueDay; time: string } | null {
  if (!validDue(dueAt)) return null;
  const date = new Date(dueAt * 1000);
  if (!Number.isFinite(date.getTime())) return null;
  return { day: localDay(date), time: `${pad(date.getHours())}:${pad(date.getMinutes())}` };
}

/**
 * Join a day and a clock back into a deadline.
 *
 * An unreadable or empty clock falls back to the same hour a bare `due:friday`
 * means, so picking a day and never touching the time matches what typing the
 * day would have done.
 */
export function composeDue(day: DueDay, time: unknown): number | null {
  if (!Number.isSafeInteger(day.year) || !Number.isSafeInteger(day.month) || !Number.isSafeInteger(day.day)) return null;
  let hour = QUICK_ADD_DUE_HOUR;
  let minute = 0;
  if (typeof time === "string") {
    const match = /^(\d{1,2}):(\d{2})$/.exec(time.trim());
    if (match) {
      const h = Number(match[1]);
      const m = Number(match[2]);
      if (h <= 23 && m <= 59) { hour = h; minute = m; }
    }
  }
  const date = new Date(day.year, day.month, day.day, hour, minute, 0, 0);
  if (!Number.isFinite(date.getTime())) return null;
  // Reject a day the constructor would roll forward (2026-02-31 → March).
  if (date.getFullYear() !== day.year || date.getMonth() !== day.month || date.getDate() !== day.day) return null;
  const seconds = Math.floor(date.getTime() / 1000);
  return Number.isSafeInteger(seconds) && seconds > 0 ? seconds : null;
}

/** The month a picker should open on: the deadline's, else the one holding `nowMs`. */
export function viewFor(dueAt: number | null | undefined, nowMs = Date.now()): DueView {
  const parts = duePartsOf(dueAt);
  if (parts) return { year: parts.day.year, month: parts.day.month };
  const now = Number.isFinite(nowMs) ? new Date(nowMs) : new Date();
  return { year: now.getFullYear(), month: now.getMonth() };
}

/** Step the drawn month, carrying the year. */
export function shiftView(view: DueView, step: number): DueView {
  const moved = new Date(view.year, view.month + step, 1);
  return { year: moved.getFullYear(), month: moved.getMonth() };
}

/**
 * Six weeks of cells for one month, Sunday first.
 *
 * Built by walking real `Date`s rather than adding days to a millisecond count,
 * so the grid stays correct across a daylight-saving boundary — a month that
 * contains a 23-hour day would otherwise slip a column.
 */
export function monthGrid(view: DueView, selected: DueDay | null, nowMs = Date.now()): DueCell[] {
  const today = localDay(Number.isFinite(nowMs) ? new Date(nowMs) : new Date());
  const first = new Date(view.year, view.month, 1);
  const cells: DueCell[] = [];
  for (let index = 0; index < GRID_CELLS; index += 1) {
    const date = new Date(view.year, view.month, 1 - first.getDay() + index);
    const day = localDay(date);
    cells.push({
      ...day,
      key: `${day.year}-${pad(day.month + 1)}-${pad(day.day)}`,
      label: day.day,
      inMonth: day.month === view.month && day.year === view.year,
      isToday: sameDay(today, day),
      isSelected: sameDay(selected, day),
    });
  }
  return cells;
}

/** Month and year, for the grid's heading. */
export function monthLabel(view: DueView, locale?: string): string {
  return new Date(view.year, view.month, 1).toLocaleDateString(locale, { month: "long", year: "numeric" });
}

/** The trigger's own text: the deadline written out, or why there is none. */
export function dueLabel(dueAt: number | null | undefined, locale?: string): string {
  if (!validDue(dueAt)) return "No due date";
  const date = new Date(dueAt * 1000);
  if (!Number.isFinite(date.getTime())) return "No due date";
  return date.toLocaleString(locale, { day: "numeric", month: "short", year: "numeric", hour: "2-digit", minute: "2-digit" });
}

/**
 * How the deadline reads beside the date, and how urgently.
 *
 * The `state` is `dueState`'s, not a second opinion: the chip the sheet shows
 * and the lane the board files the task under must agree, and they can only do
 * that by asking the same function.
 */
export function relativeDue(
  dueAt: number | null | undefined,
  nowSec: number,
): { text: string; state: ReturnType<typeof dueState> } {
  const state = dueState(dueAt, nowSec);
  if (!validDue(dueAt) || state === "none") return { text: "", state: "none" };
  const now = Number.isFinite(nowSec) ? nowSec : 0;
  // Whole days between calendar days, not between instants: a deadline at 17:00
  // tomorrow is "tomorrow" all of today, however many hours away it happens
  // to be right now.
  const startOf = (seconds: number) => {
    const date = new Date(seconds * 1000);
    date.setHours(0, 0, 0, 0);
    return date.getTime();
  };
  const days = Math.round((startOf(dueAt) - startOf(now)) / DAY_MS);
  if (days === 0) return { text: "today", state };
  if (days === 1) return { text: "tomorrow", state };
  if (days === -1) return { text: "yesterday", state };
  if (days > 1) return { text: `in ${days} days`, state };
  return { text: `${Math.abs(days)} days ago`, state };
}

/**
 * The picker's one-press options.
 *
 * Each is a word `parseQuickAddDue` already understands rather than its own
 * arithmetic, so a shortcut and the same word typed into the box cannot land on
 * different days.
 */
export const DUE_SHORTCUTS: readonly { id: string; label: string; word: string }[] = Object.freeze([
  { id: "today", label: "Today", word: "today" },
  { id: "tomorrow", label: "Tomorrow", word: "tomorrow" },
  { id: "friday", label: "Friday", word: "friday" },
  { id: "next-week", label: "Next week", word: "next-week" },
  { id: "two-weeks", label: "In 2 weeks", word: "+2w" },
]);

/**
 * Read a typed phrase, keeping the day the reader already picked when the
 * phrase is only a clock.
 *
 * Returns `null` for anything the shared grammar does not accept, which is what
 * lets the box say nothing rather than guess — the same rule `due:` follows.
 */
export function readDuePhrase(
  text: string,
  current: number | null | undefined,
  nowMs = Date.now(),
): number | null {
  const trimmed = text.trim();
  if (!trimmed) return null;
  const direct = parseQuickAddDue(trimmed, nowMs);
  if (direct !== null) return direct;
  // A bare clock keeps the chosen day: "5pm" on a sheet already due Friday
  // means Friday at five, not today at five.
  const parts = duePartsOf(current);
  if (!parts) return null;
  const withDay = parseQuickAddDue(`${parts.day.year}-${pad(parts.day.month + 1)}-${pad(parts.day.day)} ${trimmed}`, nowMs);
  return withDay;
}
