/**
 * A session's saved output, read back from the terminal host
 * (`cmd_terminal_transcript`).
 *
 * `endedBuffer.ts` keeps a 32 KiB plain-text tail of an ended tab in memory,
 * which is gone when the app quits. The host also writes every session's output
 * to disk, so a finished run's output can be reopened after a quit, a crash or a
 * host restart. This module is the renderer's whole contract with that read:
 * ask, validate what came back, and decide what the viewer must be told.
 *
 * Five outcomes, kept apart by type so a caller cannot show one as another:
 *
 * - `found`: output, with the facts that qualify it (clipped, gaps, masked,
 *   still running, not fully written yet, recording stopped).
 * - `missing`: the host has no transcript for the session. Never recorded,
 *   evicted for space, or aged out.
 * - `unreadable`: one exists or should, and the host could not read it.
 * - `unavailable`: this app runs terminals itself, so nothing is recorded.
 * - `failed`: the question was not answered (the call failed, timed out, or the
 *   answer was malformed). Not "missing": the output may well be there.
 */

import { invoke } from "../ipc/invoke";
import { terminalDeadline } from "./inputQueue";

/** The most the host returns in one read; asking for more is clamped there. */
export const TRANSCRIPT_TAIL_BYTES = 128 * 1024;
/** How long the read may take, the host finishing its queued writes included. */
export const TRANSCRIPT_READ_MS = 12_000;

export interface FoundTranscript {
  readonly kind: "found";
  /** The output as text, escape sequences still in it. See `transcriptPlainText`. */
  readonly text: string;
  readonly bytes: number;
  /** Bytes of the session's output still on disk; more than `bytes` when clipped. */
  readonly retained: number;
  /** The start of the session's output is not in `text` (`rotated` or `clipped`). */
  readonly truncated: boolean;
  /** Earlier output was rotated off the disk to keep the transcript within its limit. */
  readonly rotated: boolean;
  /** The read was cut to its byte bound, so older saved output exists but is not in `text`. */
  readonly clipped: boolean;
  /** Places where the host dropped output it could not keep up with. */
  readonly gapMarkers: number;
  /**
   * Bytes the host dropped that no marker in `text` accounts for yet: pending
   * (the marker is written when a chunk fits or the session ends) or lost for
   * good because the host was already stopping. Not "none" when zero is
   * absent: the host always says.
   */
  readonly unrecordedBytes: number;
  /** Secrets were masked in the host. */
  readonly redacted: boolean;
  /** The session is still running, so more output may follow. */
  readonly live: boolean;
  /** The host had written everything it was given; false means `text` may lag. */
  readonly settled: boolean;
  /**
   * Why the saved output may stop short: a write failed, or the host that wrote
   * it ended without closing it, so its end is unknown.
   */
  readonly recordingError: string | null;
}

export type SavedTranscript =
  | FoundTranscript
  | { readonly kind: "missing" }
  | { readonly kind: "unreadable"; readonly reason: string }
  | { readonly kind: "unavailable"; readonly reason: string }
  | { readonly kind: "failed"; readonly reason: string };

export interface TranscriptReadOptions {
  /** Most bytes of the tail to ask for. Default: the host's ceiling. */
  readonly maxBytes?: number;
  /** Have the host mask secrets before the bytes leave it. */
  readonly redact?: boolean;
}

export interface TranscriptDeps {
  /** `cmd_terminal_transcript`. */
  readonly read: (args: { sessionId: string; maxBytes: number; redact: boolean }) => Promise<unknown>;
  readonly deadlineMs: number;
}

const defaultDeps: TranscriptDeps = {
  read: (args) => invoke<unknown>("cmd_terminal_transcript", args),
  deadlineMs: TRANSCRIPT_READ_MS,
};

/** Reads `sessionId`'s saved output. Never throws: a failure is `failed`. */
export async function readSavedTranscript(
  sessionId: string,
  options: TranscriptReadOptions = {},
  deps: TranscriptDeps = defaultDeps,
): Promise<SavedTranscript> {
  try {
    const raw = await terminalDeadline(
      deps.read({
        sessionId,
        maxBytes: options.maxBytes ?? TRANSCRIPT_TAIL_BYTES,
        redact: options.redact ?? false,
      }),
      deps.deadlineMs,
      "Reading saved terminal output",
    );
    return parseSavedTranscript(raw);
  } catch (error) {
    return { kind: "failed", reason: error instanceof Error ? error.message : String(error) };
  }
}

function failed(reason: string): SavedTranscript {
  return { kind: "failed", reason };
}

function nonNegativeInteger(value: unknown): number | null {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0 ? value : null;
}

/** Validates the host's answer at the boundary. A malformed answer is `failed`. */
export function parseSavedTranscript(raw: unknown): SavedTranscript {
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) return failed("The terminal host's answer was not an object.");
  const record = raw as Record<string, unknown>;
  switch (record.state) {
    case "missing":
      return { kind: "missing" };
    case "unreadable":
    case "unavailable": {
      if (typeof record.reason !== "string" || record.reason.length === 0) {
        return failed(`The terminal host said ${record.state} without saying why.`);
      }
      return { kind: record.state, reason: record.reason };
    }
    case "found":
      return parseFound(record);
    default:
      return failed("The terminal host's answer named a state this version does not know.");
  }
}

function parseFound(record: Record<string, unknown>): SavedTranscript {
  const bytes = nonNegativeInteger(record.bytes);
  const retained = nonNegativeInteger(record.retained);
  const gapMarkers = nonNegativeInteger(record.gap_markers);
  const unrecordedBytes = nonNegativeInteger(record.unrecorded_bytes);
  const flags = ["truncated", "rotated", "clipped", "redacted", "live", "settled"] as const;
  if (typeof record.data_b64 !== "string" || bytes === null || retained === null || gapMarkers === null || unrecordedBytes === null) {
    return failed("The terminal host's transcript was malformed.");
  }
  for (const flag of flags) {
    if (typeof record[flag] !== "boolean") return failed("The terminal host's transcript was malformed.");
  }
  const recordingError = record.recording_error;
  if (recordingError !== null && recordingError !== undefined && typeof recordingError !== "string") {
    return failed("The terminal host's transcript was malformed.");
  }
  const decoded = decodeBase64Utf8(record.data_b64);
  if (decoded === null) return failed("The terminal host's transcript was not valid base64.");
  if (decoded.byteLength !== bytes) {
    // The count is the host's claim about what it sent; a mismatch means the
    // bytes were damaged on the way, and showing them as complete would lie.
    return failed("The terminal host's transcript did not match its own length.");
  }
  return {
    kind: "found",
    text: new TextDecoder("utf-8").decode(decoded),
    bytes,
    retained,
    truncated: record.truncated as boolean,
    rotated: record.rotated as boolean,
    clipped: record.clipped as boolean,
    gapMarkers,
    unrecordedBytes,
    redacted: record.redacted as boolean,
    live: record.live as boolean,
    settled: record.settled as boolean,
    recordingError: typeof recordingError === "string" ? recordingError : null,
  };
}

function decodeBase64Utf8(data: string): Uint8Array | null {
  try {
    const binary = atob(data);
    const out = new Uint8Array(binary.length);
    for (let i = 0; i < binary.length; i += 1) out[i] = binary.charCodeAt(i);
    return out;
  } catch {
    return null;
  }
}

/**
 * Everything a viewer must say beside the output, so none of it is left to the
 * caller to remember. Each line is a fact about what the text is not.
 */
export function transcriptNotices(found: FoundTranscript): string[] {
  const notices: string[] = [];
  if (found.rotated) {
    notices.push("Earlier output was rotated away to keep the saved transcript within its size limit.");
  }
  if (found.clipped) {
    // `bytes` is counted after masking and `retained` before it, so with
    // masking on the two are not the same unit and are not set side by side.
    notices.push(
      found.redacted
        ? `Showing only the newest ${found.bytes.toLocaleString("en-US")} bytes of the saved output; earlier output is not shown.`
        : `Showing the last ${found.bytes.toLocaleString("en-US")} of ${found.retained.toLocaleString("en-US")} saved bytes; earlier output is not shown.`,
    );
  }
  if (found.gapMarkers > 0) {
    notices.push(
      `${found.gapMarkers} stretch${found.gapMarkers === 1 ? "" : "es"} of output could not be saved fast enough and ${found.gapMarkers === 1 ? "is" : "are"} marked in the text.`,
    );
  }
  if (found.unrecordedBytes > 0) {
    notices.push(
      `${found.unrecordedBytes.toLocaleString("en-US")} more byte${found.unrecordedBytes === 1 ? " was" : "s were"} dropped because the host could not save output fast enough, and the text does not mark where.`,
    );
  }
  if (found.recordingError) notices.push(`The saved output may be incomplete: ${found.recordingError}`);
  if (found.live) notices.push("This session is still running; more output may follow.");
  if (!found.settled) notices.push("The host was still writing output when it answered; the end may be missing.");
  if (found.redacted) notices.push("Secrets were masked.");
  return notices;
}

// CSI (including private-mode and intermediate bytes), OSC ended by BEL or ST,
// DCS/SOS/PM/APC strings ended by ST, and the two-byte escapes.
const ESCAPES =
  // eslint-disable-next-line no-control-regex
  /\u001b\[[0-?]*[ -/]*[@-~]|\u001b\][^\u0007\u001b]*(?:\u0007|\u001b\\)|\u001b[PX^_][^\u001b]*\u001b\\|\u001b[@-Z\\-_]|\u001b[ -/]+[0-~]/g;

/**
 * Plain text for a viewer that is not a terminal: escape sequences removed,
 * a carriage return replacing its line with what follows the way a progress bar
 * intends (a shorter later segment does not leave the rest of the earlier one
 * showing, because the erase-to-end-of-line a program sends with it is among
 * the sequences removed), backspace erasing, and other control characters
 * dropped. Colour and cursor movement are lost; the words and their order are
 * not.
 */
export function transcriptPlainText(text: string): string {
  const lines = text
    .replace(ESCAPES, "")
    .replace(/\r\n/g, "\n")
    .split("\n")
    .map((line) => {
      // A line's text is its last non-empty segment between carriage returns.
      // The empty ones are not overwrites: the tty turns a program's "\r\n"
      // into "\r\r\n", and a tail can end on a bare "\r" (a run killed mid
      // progress bar); taking the empty segment after either would blank a
      // line that has words on it.
      const shown = line.split("\r").filter((part) => part.length > 0);
      return eraseBackspaces(shown[shown.length - 1] ?? "");
    });
  // eslint-disable-next-line no-control-regex
  return lines.join("\n").replace(/[\u0000-\u0008\u000b-\u001f\u007f]/g, "");
}

function eraseBackspaces(line: string): string {
  if (!line.includes("\b")) return line;
  const out: string[] = [];
  for (const char of line) {
    if (char === "\b") out.pop();
    else out.push(char);
  }
  return out.join("");
}
