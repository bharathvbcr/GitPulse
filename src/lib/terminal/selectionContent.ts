/**
 * What "send to agent" puts in front of an agent for a selection of a file:
 * a reference to it (`@path#Lx-Ly`), or the lines themselves in a fenced block.
 *
 * The block is composed here, once, so every surface that sends content shares
 * its bound and its escaping. It is inserted into a terminal and not
 * submitted, so a person reads it first; it is still built as if nobody would,
 * because the text is a file's and a file's text is not trusted.
 */

import { AGENT_MESSAGE_MAX_BYTES } from "../workbench/sendToAgent";
import { detectLanguageFromPath } from "../files/syntaxHighlight";
import { safeBlock, safeText } from "./agentPromptText";

const encoder = new TextEncoder();
const byteLength = (text: string) => encoder.encode(text).length;

/** `@path`, `@path#Ln` or `@path#Lx-Ly`, with the trailing space an agent needs to end the mention. */
export function formatSelectionRef(path: string, startLine: number | null, endLine: number | null): string {
  if (startLine === null) return `@${path} `;
  const end = endLine ?? startLine;
  const lo = Math.min(startLine, end);
  const hi = Math.max(startLine, end);
  return lo === hi ? `@${path}#L${lo} ` : `@${path}#L${lo}-L${hi} `;
}

export interface SelectionContentInput {
  /** Repository-relative path of the file. */
  path: string;
  /** The checkout (worktree) the file is in, by short name. */
  checkoutName: string;
  /** 1-based number of the first of `lines`. */
  startLine: number;
  /** The selected lines, without their line breaks. */
  lines: readonly string[];
  /** The lines are an unsaved buffer, not the file as it is on disk. */
  unsaved?: boolean;
}

export interface SelectionContent {
  /** The text to insert; empty when there was nothing to send. Never longer than `AGENT_MESSAGE_MAX_BYTES` bytes of UTF-8. */
  text: string;
  /** How many of the selected lines the bound left out (the last ones). */
  omittedLines: number;
}

/** What a lone surrogate becomes: the character it would be encoded as on its way to the terminal anyway. */
const REPLACEMENT = String.fromCharCode(0xfffd);
const LONE_SURROGATE = /[\ud800-\udbff](?![\udc00-\udfff])|(?<![\ud800-\udbff])[\udc00-\udfff]/g;

/** A fence longer than any run of backticks in `body`, so the body cannot close it. */
function fenceFor(body: string): string {
  let longest = 0;
  for (const run of body.match(/`+/g) ?? []) longest = Math.max(longest, run.length);
  return "`".repeat(Math.max(3, longest + 1));
}

/**
 * The fenced block for a selection, within `AGENT_MESSAGE_MAX_BYTES` counting
 * everything (header, fences, marker) after escaping. Past the bound it keeps
 * whole leading lines and ends with a marker outside the fence that says how
 * many lines were left out; it never shortens a line or drops lines unmarked.
 */
export function composeSelectionContent(input: SelectionContentInput): SelectionContent {
  if (input.lines.length === 0) return { text: "", omittedLines: 0 };
  // A line holds no break of its own. The CR a CRLF file leaves at a line's end
  // is part of the break, not the text; one inside a line is shown as `\r`
  // (safeBlock), so one source line stays one line and the numbers stay true.
  const lines = input.lines.map((line) => safeBlock(line.replace(LONE_SURROGATE, REPLACEMENT).replace(/\r$/, "")));
  const first = input.startLine;
  const last = first + lines.length - 1;
  const where = first === last ? `line ${first}` : `lines ${first}-${last}`;
  const header =
    `File: ${safeText(input.path)}, ${where}, checkout ${safeText(input.checkoutName)}` +
    (input.unsaved ? " (unsaved buffer)" : "");
  const language = detectLanguageFromPath(input.path);
  const info = language === "plaintext" ? "" : language;

  const build = (count: number): string => {
    const body = lines.slice(0, count).join("\n");
    const fence = fenceFor(body);
    const block = `${header}\n${fence}${info}\n${body}\n${fence}`;
    if (count === lines.length) return block;
    const left = lines.length - count;
    const range = `lines ${first + count}-${last}`;
    return count === 0
      ? `${header}\n[GitPulse: ${left} ${left === 1 ? "line" : "lines"} not included (${range}); the first line alone exceeds the ${AGENT_MESSAGE_MAX_BYTES}-byte limit]`
      : `${block}\n[GitPulse: ${left} more ${left === 1 ? "line" : "lines"} not included (${range}); ${AGENT_MESSAGE_MAX_BYTES}-byte limit]`;
  };

  // Lines past the bound cannot fit, so only a prefix of them is a candidate.
  let candidate = 0;
  let running = 0;
  while (candidate < lines.length && running + byteLength(lines[candidate]) + 1 <= AGENT_MESSAGE_MAX_BYTES) {
    running += byteLength(lines[candidate]) + 1;
    candidate += 1;
  }
  let lo = 0;
  let hi = candidate;
  while (lo < hi) {
    const mid = Math.ceil((lo + hi) / 2);
    if (byteLength(build(mid)) <= AGENT_MESSAGE_MAX_BYTES) lo = mid;
    else hi = mid - 1;
  }
  let text = build(lo);
  if (byteLength(text) > AGENT_MESSAGE_MAX_BYTES) {
    // Only a header too long to fit even alone gets here (a path of thousands of characters).
    const note = `[GitPulse: ${lines.length} lines not included]`;
    text = `${safeText(input.path).slice(0, 512)}\n${note}`;
    return { text, omittedLines: lines.length };
  }
  return { text, omittedLines: lines.length - lo };
}
