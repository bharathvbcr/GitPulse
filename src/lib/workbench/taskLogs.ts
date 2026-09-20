/**
 * Raw logs pasted onto a task sheet.
 *
 * Notes are a drafting surface: they become a title and description. A stack
 * trace pasted there would be title-extracted, 64 KB-capped as description,
 * and rewritten by Manvi. Logs are evidence for a later copy or agent
 * handoff, so they have their own field, their own bound, and they never
 * feed title extraction.
 *
 * The bound matches the terminal output window (256 KiB). A larger paste is
 * kept up to that budget and the cut is announced in the stored text — a
 * silent trim would read to an agent as the whole dump.
 */

export const LOGS_CAP_BYTES = 256 * 1024;
const NOTICE_RESERVE = 96;
const CONTROLS = /[\u0000-\u0008\u000B\u000C\u000E-\u001F\u007F\u0080-\u009F]/g;

export interface SanitizedLogs {
  text: string;
  truncated: boolean;
  originalBytes: number;
  keptBytes: number;
  notice: string;
}

function empty(originalBytes = 0): SanitizedLogs {
  return { text: "", truncated: false, originalBytes, keptBytes: 0, notice: "" };
}

function utf8Bytes(text: string): number {
  return new TextEncoder().encode(text).length;
}

function stripAnsi(text: string): string {
  return text
    .replace(/\u001b\][^\u0007\u001b]*(?:\u0007|\u001b\\)/g, "")
    .replace(/\u001b[PX^_].*?(?:\u001b\\|\u0007)/g, "")
    .replace(/\u001b\[[\x30-\x3f]*[\x20-\x2f]*[\x40-\x7e]/g, "")
    .replace(/\u009b[\x30-\x3f]*[\x20-\x2f]*[\x40-\x7e]/g, "")
    .replace(/\u001b[\(\)][\x20-\x7e]/g, "")
    .replace(/\u001b./g, "");
}

function takeBytes(text: string, cap: number): { text: string; bytes: number } {
  const encoded = new TextEncoder().encode(text);
  if (encoded.length <= cap) return { text, bytes: encoded.length };
  const decoder = new TextDecoder("utf-8", { fatal: false });
  let slice = encoded.subarray(0, cap);
  // Drop a trailing incomplete UTF-8 sequence rather than emitting U+FFFD
  // into stored evidence.
  let end = slice.length;
  while (end > 0 && (slice[end - 1]! & 0xc0) === 0x80) end -= 1;
  if (end > 0 && slice[end - 1]! >= 0x80) end -= 1;
  slice = slice.subarray(0, end);
  const taken = decoder.decode(slice);
  return { text: taken, bytes: utf8Bytes(taken) };
}

function cap(value: unknown): number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 32 ? value : LOGS_CAP_BYTES;
}

/**
 * Make pasted terminal output safe to store and to hand to an agent.
 *
 * Non-strings, NULs and other C0/C1 controls (except tab and newline) are
 * dropped rather than stored. ANSI/OSC sequences are stripped so a color
 * dump is still readable. CRLF and lone CR become LF. The result is bounded
 * in UTF-8 bytes; a cut is named in the returned text.
 */
export function sanitizeLogs(value: unknown, byteCap = LOGS_CAP_BYTES): SanitizedLogs {
  const limit = cap(byteCap);
  if (typeof value !== "string") return empty();
  const originalBytes = utf8Bytes(value);
  const normalized = stripAnsi(value.replace(/\r\n/g, "\n").replace(/\r/g, "\n"))
    .replace(CONTROLS, "")
    .replace(/[\u202a-\u202e\u2066-\u2069]/g, "");
  if (!normalized.replace(/\s/g, "")) return empty(originalBytes);
  if (utf8Bytes(normalized) <= limit) {
    return {
      text: normalized,
      truncated: false,
      originalBytes,
      keptBytes: utf8Bytes(normalized),
      notice: "",
    };
  }
  const budget = Math.max(32, limit - NOTICE_RESERVE);
  const taken = takeBytes(normalized, budget);
  const notice = `[logs truncated: ${taken.bytes} of ${utf8Bytes(normalized)} bytes kept]`;
  const text = `${taken.text}\n\n${notice}`;
  return {
    text,
    truncated: true,
    originalBytes,
    keptBytes: utf8Bytes(text),
    notice,
  };
}

/** Fence that cannot be closed by backticks already in the logs. */
export function fenceLogs(text: string): string {
  if (typeof text !== "string" || !text) return "";
  let ticks = 3;
  let run = 0;
  for (const ch of text) {
    if (ch === "`") {
      run += 1;
      if (run + 1 > ticks) ticks = run + 1;
    } else {
      run = 0;
    }
  }
  const mark = "`".repeat(ticks);
  return `${mark}\n${text}\n${mark}`;
}

/**
 * Markdown section for a brief or an unsaved agent copy.
 *
 * Empty after sanitizing: no section. A missing heading must not be read as
 * "there were no logs" when the field was never examined — callers that did
 * not look should not call this.
 */
export function formatLogsSection(value: unknown): string | null {
  const { text } = sanitizeLogs(value);
  if (!text) return null;
  return [
    "## Raw logs",
    "Pasted evidence. Keep stack frames, timestamps, error codes and quoted text exactly as written.",
    "",
    fenceLogs(text),
  ].join("\n");
}
