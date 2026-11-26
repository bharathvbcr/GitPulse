/**
 * `AskUserQuestion`: the agent asks the person to choose, and waits.
 *
 * The input shape is Claude Code's: `questions` is one to four entries of
 * `{question, header, options: [{label, description}], multiSelect}`. The
 * answer is a map from the question's text to the chosen label (labels joined
 * with ", " for a multi-select question, or the person's own text for "Other").
 *
 * The page sends only that map. The host merges it into the input the agent
 * actually sent and refuses a key that is not one of the questions, so nothing
 * here widens what the page may say on the agent's behalf.
 */
import type { ToolItem } from "./types";

export const ASK_TOOL = "AskUserQuestion";

export interface AskOption {
  readonly label: string;
  readonly description: string;
}

export interface AskQuestion {
  readonly question: string;
  readonly header: string;
  readonly options: readonly AskOption[];
  readonly multiSelect: boolean;
}

/** The longest free-text answer the page sends (the host bounds it too). */
export const MAX_ANSWER_CHARS = 2000;

function text(value: unknown): string {
  return typeof value === "string" ? value : "";
}

/**
 * The questions in a request's input, or null when the input is not a
 * well-formed question set (then the generic permission card is shown, which
 * can only deny).
 */
export function parseQuestions(input: Record<string, unknown>): AskQuestion[] | null {
  const raw = input.questions;
  if (!Array.isArray(raw) || raw.length === 0 || raw.length > 4) return null;
  const out: AskQuestion[] = [];
  for (const entry of raw) {
    if (typeof entry !== "object" || entry === null) return null;
    const q = entry as Record<string, unknown>;
    const question = text(q.question).trim();
    if (!question) return null;
    const options: AskOption[] = [];
    if (Array.isArray(q.options)) {
      for (const o of q.options) {
        if (typeof o !== "object" || o === null) continue;
        const label = text((o as Record<string, unknown>).label).trim();
        if (label) options.push({ label, description: text((o as Record<string, unknown>).description) });
      }
    }
    out.push({ question, header: text(q.header), options, multiSelect: q.multiSelect === true });
  }
  return out;
}

/** What the person has picked for one question. */
export interface Pick {
  /** Chosen option labels. */
  labels: readonly string[];
  /** Free text; when non-empty it is the answer (a single choice) or an addition (a multi-select). */
  other: string;
}

/**
 * The answer for one question: the option label, the labels joined with ", ",
 * or the person's own text. Null when nothing is chosen.
 */
export function answerFor(question: AskQuestion, pick: Pick | undefined): string | null {
  if (!pick) return null;
  const other = pick.other.trim().slice(0, MAX_ANSWER_CHARS);
  if (question.multiSelect) {
    const parts = [...pick.labels];
    if (other) parts.push(other);
    return parts.length ? parts.join(", ") : null;
  }
  if (other) return other;
  return pick.labels[0] ?? null;
}

/** The answers map for every question, or null while one is unanswered. */
export function buildAnswers(
  questions: readonly AskQuestion[],
  picks: readonly (Pick | undefined)[],
): Record<string, string> | null {
  const answers: Record<string, string> = {};
  for (const [i, question] of questions.entries()) {
    const answer = answerFor(question, picks[i]);
    if (answer === null) return null;
    answers[question.question] = answer;
  }
  return answers;
}

/** The `message` the host reads as the answers to a question request. */
export function encodeAnswers(answers: Record<string, string>): string {
  return JSON.stringify({ answers });
}

export interface GivenAnswer {
  readonly question: string;
  readonly answer: string;
}

/**
 * The answers Claude Code reports back in the tool result
 * (`User has answered your questions: "Q"="A", "Q2"="B". ...`). Null when the
 * result does not look like that, so the caller shows the raw result.
 */
export function answersFromResult(result: string): GivenAnswer[] | null {
  const out: GivenAnswer[] = [];
  const pattern = /"([^"]+)"="([^"]*)"/g;
  for (let m = pattern.exec(result); m; m = pattern.exec(result)) out.push({ question: m[1]!, answer: m[2]! });
  return out.length ? out : null;
}

/** A question tool the timeline shows as its own row instead of folding it away. */
export function isQuestionTool(tool: ToolItem): boolean {
  return tool.name === ASK_TOOL && !tool.parentId;
}
