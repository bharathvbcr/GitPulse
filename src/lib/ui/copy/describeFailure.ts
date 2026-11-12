/**
 * The three message templates of UI_RULES §3, as functions:
 *
 *   Error:        Couldn't <verb> <object>. <Reason>. [Try again]
 *   Destructive:  <Verb> <object>?  one sentence of consequence  [<Verb>] [Cancel]
 *   Unknown:      Couldn't check <object>  <reason>  [Rescan]
 *
 * The wording lives here once so a surface cannot drift from the template.
 * Backend text is never the headline: it is the `detail`, passed through
 * `formatError` (the one place that turns IPC rejections, Error objects and
 * plain objects into a string) and then made safe to print on one line.
 *
 * Error paths must not throw, so `describeFailure` and `unknownState` fall
 * back to a plain wording when the caller supplies a blank verb or object.
 * `confirmDestructive` throws instead: a destructive dialog with a blank title
 * or no stated consequence must never reach the screen.
 */

import { formatError } from "../formatError";

/** Longest `detail` / `reason` / destructive `body`, in characters, ellipsis included. */
export const DETAIL_MAX_CHARS = 240;

/** Longest verb or object in a headline or title, in characters, ellipsis included. */
export const OBJECT_MAX_CHARS = 80;

const ELLIPSIS = "…";
const ANSI_ESCAPE = /\u001b\[[0-9;?]*[ -/]*[@-~]/g;
const CONTROL_OR_SPACE = /[\s\u0000-\u001f\u007f]+/g;
const TRAILING_STOP = /[\s.!?:;,]+$/;

export interface FailureAction {
  /** The verb as a bare lowercase word: "fetch", "delete", "open". */
  readonly verb: string;
  /** What it was done to: "branches", "the worktree", "origin". */
  readonly object: string;
}

export interface FailureOptions {
  /** Cap for `detail`, in characters, ellipsis included. Default `DETAIL_MAX_CHARS`. */
  readonly maxDetailChars?: number;
}

export interface FailureCopy {
  /** Exactly `Couldn't <verb> <object>.` and never backend text. */
  readonly headline: string;
  /** The reason, from `formatError`; absent only when it reduces to nothing. */
  readonly detail?: string;
  readonly retryLabel: "Try again";
}

export interface DestructiveConfirmInput {
  /** "Delete", "Discard", "Remove": the button's own word. */
  readonly verb: string;
  readonly object: string;
  /** One sentence saying what will be lost. */
  readonly consequence: string;
}

export interface DestructiveConfirmCopy {
  readonly title: string;
  readonly body: string;
  readonly confirmLabel: string;
  readonly cancelLabel: "Cancel";
  readonly variant: "danger";
}

export interface UnknownStateCopy {
  readonly title: string;
  readonly reason: string;
  readonly actionLabel: "Rescan";
}

/**
 * One line of printable text: ANSI colour codes dropped, every run of
 * whitespace and control characters (newlines included) collapsed to a single
 * space, trimmed, and cut to `max` characters with a trailing `…`. The cut
 * counts code points, so an emoji or a surrogate pair is never split.
 */
export function toOneLine(text: string, max: number = DETAIL_MAX_CHARS): string {
  const limit = Number.isFinite(max) ? Math.max(1, Math.floor(max)) : DETAIL_MAX_CHARS;
  const collapsed = text.replace(ANSI_ESCAPE, "").replace(CONTROL_OR_SPACE, " ").trim();
  const points = Array.from(collapsed);
  if (points.length <= limit) return collapsed;
  return points.slice(0, limit - 1).join("").trimEnd() + ELLIPSIS;
}

function bareWord(text: string, fallback: string): string {
  const cleaned = toOneLine(text, OBJECT_MAX_CHARS).replace(TRAILING_STOP, "");
  return cleaned.length > 0 ? cleaned : fallback;
}

function capitalize(word: string): string {
  const [first, ...rest] = Array.from(word);
  return first === undefined ? word : first.toUpperCase() + rest.join("");
}

/**
 * Build the error copy for a failed action. `err` is whatever was thrown or
 * rejected. The headline is built only from `action`; the reason is
 * `formatError(err)`, flattened to one line and capped.
 */
export function describeFailure(action: FailureAction, err: unknown, opts: FailureOptions = {}): FailureCopy {
  const verb = bareWord(action.verb, "finish").toLowerCase();
  const object = bareWord(action.object, "that");
  const headline = `Couldn't ${verb} ${object}${object.endsWith(ELLIPSIS) ? "" : "."}`;
  const detail = toOneLine(formatError(err), opts.maxDetailChars ?? DETAIL_MAX_CHARS);
  return detail.length > 0
    ? { headline, detail, retryLabel: "Try again" }
    : { headline, retryLabel: "Try again" };
}

/**
 * A failure headline and its reason as one sentence pair, for a toast that has
 * no room for a second line: `Couldn't <verb> <object>. <Reason>.` The headline
 * is the caller's own wording; the reason is flattened to one line, starts with
 * a capital and ends with a stop. A missing reason says what is true, that the
 * action did not finish, rather than leaving the headline to read as a verdict.
 */
export function failureLine(headline: string, reason?: string | null): string {
  const lead = `${bareWord(headline, "Couldn't finish")}.`;
  const detail = toOneLine(reason ?? "", DETAIL_MAX_CHARS) || "It did not complete.";
  const sentence = capitalize(detail);
  return `${lead} ${/[.!?…]$/.test(sentence) ? sentence : `${sentence}.`}`;
}

/**
 * Build a destructive confirmation. The title is `<Verb> <object>?`, the
 * button repeats the verb, and `variant: "danger"` is never the primary
 * gradient. Throws `TypeError` when the verb, object or consequence is blank.
 */
export function confirmDestructive(input: DestructiveConfirmInput): DestructiveConfirmCopy {
  const verb = capitalize(bareWord(input.verb, ""));
  const object = bareWord(input.object, "");
  const consequence = toOneLine(input.consequence, DETAIL_MAX_CHARS);
  if (verb.length === 0 || object.length === 0 || consequence.length === 0) {
    throw new TypeError("confirmDestructive: verb, object and consequence must all be non-blank");
  }
  return {
    title: `${verb} ${object}?`,
    body: /[.!?…]$/.test(consequence) ? consequence : `${consequence}.`,
    confirmLabel: verb,
    cancelLabel: "Cancel",
    variant: "danger",
  };
}

/**
 * Copy for a state that could not be determined: "Couldn't check <object>",
 * the reason, and a Rescan action. Never "clean", "none" or "0": a check that
 * could not run must not look like one that passed.
 */
export function unknownState(object: string, reason: string): UnknownStateCopy {
  return {
    title: `Couldn't check ${bareWord(object, "this")}`,
    reason: toOneLine(reason, DETAIL_MAX_CHARS) || "No reason was reported.",
    actionLabel: "Rescan",
  };
}
