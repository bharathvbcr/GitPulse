const UNKNOWN_ERROR = "Unknown error";

/**
 * JSON.stringify that cannot throw: cycles, throwing getters, and BigInt
 * values are all survived. Object keys are emitted in sorted order so the
 * same failure always renders the same string (stable for logs/tests).
 */
function stableStringify(value: unknown): string | null {
  try {
    return JSON.stringify(value, (_key, item) => {
      if (typeof item === "bigint") return item.toString();
      if (item instanceof Error) {
        return item.message ? `${item.name}: ${item.message}` : item.name;
      }
      if (item instanceof Map) {
        // Entries flow back through this replacer, so nested values get the
        // same BigInt/Error/sorted-key treatment.
        return Object.fromEntries(item);
      }
      if (item instanceof Set) {
        return [...item];
      }
      if (typeof item === "object" && item !== null && !Array.isArray(item)) {
        const source = item as Record<string, unknown>;
        const sorted: Record<string, unknown> = {};
        for (const key of Object.keys(source).sort()) {
          sorted[key] = source[key];
        }
        return sorted;
      }
      return item;
    });
  } catch {
    // Cyclic structure or a getter that threw mid-walk.
    return null;
  }
}

/**
 * What a person reads in place of a JavaScript runtime fault. The fault's own
 * text ("Cannot read properties of undefined (reading 'invoke')") names
 * internals and means nothing to them; it goes to Diagnostics instead.
 */
export const INTERNAL_ERROR_COPY = "Something went wrong inside Waylode. Details are in Diagnostics.";

/** Where a runtime fault's raw error goes when it is replaced by `INTERNAL_ERROR_COPY`. */
export type ProgrammingErrorSink = (source: string, err: unknown) => void;

let programmingErrorSink: ProgrammingErrorSink | null = null;

/**
 * Faults already recorded, by identity. A template that renders `formatError(e)`
 * runs on every update, and a panel may log a fault itself before it shows it;
 * neither may add a second Diagnostics entry for the same error object.
 */
const recordedFaults = new WeakSet<object>();

/**
 * Register the destination for runtime faults, or `null` to unregister. This is
 * a seam rather than an import because Diagnostics formats its own entries
 * through this module (`diagnostics.ts` registers itself at module init).
 */
export function setProgrammingErrorSink(sink: ProgrammingErrorSink | null): void {
  programmingErrorSink = sink;
}

/** Note that `err` has been recorded by someone else, so `formatError` will not record it again. */
export function markProgrammingErrorRecorded(err: unknown): void {
  if (typeof err === "object" && err !== null) recordedFaults.add(err);
}

function recordProgrammingError(source: string, err: unknown): void {
  if (typeof err === "object" && err !== null) {
    if (recordedFaults.has(err)) return;
    recordedFaults.add(err);
  }
  try {
    if (programmingErrorSink) programmingErrorSink(source, err);
    else console.error(`[gitpulse] ${source}:`, err);
  } catch {
    // A failing sink must not turn a display call into a second fault.
  }
}

/**
 * The text to show a person for any thrown or rejected value.
 *
 * What the backend said (strings, `Error`s such as `IpcError`, plain objects)
 * renders as `formatErrorRaw` does. A JavaScript runtime fault in this page
 * (`isProgrammingError`) renders as `INTERNAL_ERROR_COPY` instead, and the raw
 * error is handed to the registered sink under `source` first, once per error
 * object, so it is never silently dropped.
 *
 * Anything that must carry the fault's own text (a log, Diagnostics, an export,
 * a "copy details" action) calls `formatErrorRaw`.
 */
export function formatError(err: unknown, source: string = "ui"): string {
  if (!isProgrammingError(err)) return formatErrorRaw(err);
  recordProgrammingError(source, err);
  return INTERNAL_ERROR_COPY;
}

/**
 * Render any rejection value as a human-readable string, verbatim.
 *
 * IPC rejections arrive as strings, Error instances, or plain objects from
 * the Rust side; `String(err)` turns the object forms into "[object Object]".
 * Order of preference:
 *   1. string input → itself (trimmed; blank becomes "Unknown error")
 *   2. object with a non-blank `.message` string → that message (trimmed)
 *   3. anything else → stable JSON (or "Unknown error" if even that fails)
 *
 * For text that goes to a log or is meant to carry the raw error; for text a
 * person reads, use `formatError`.
 */
export function formatErrorRaw(err: unknown): string {
  if (typeof err === "string") {
    const trimmed = err.trim();
    return trimmed.length > 0 ? trimmed : UNKNOWN_ERROR;
  }
  if (err === null || err === undefined) return UNKNOWN_ERROR;
  if (
    typeof err === "number" ||
    typeof err === "boolean" ||
    typeof err === "bigint" ||
    typeof err === "symbol"
  ) {
    return String(err);
  }
  if (typeof err !== "object") return UNKNOWN_ERROR;

  let message: unknown;
  try {
    message = (err as { message?: unknown }).message;
  } catch {
    return UNKNOWN_ERROR;
  }
  if (typeof message === "string") {
    const trimmed = message.trim();
    if (trimmed.length > 0) return trimmed;
  }

  const json = stableStringify(err);
  return json ?? UNKNOWN_ERROR;
}

/**
 * True for a JavaScript runtime fault (`TypeError`, `ReferenceError`,
 * `RangeError`, `SyntaxError`): a bug in this page, never something the
 * backend said. IPC and command failures arrive as strings, plain objects or
 * `IpcError` / `WorkbenchError` (plain `Error` subclasses), so none of them
 * qualifies.
 */
export function isProgrammingError(err: unknown): boolean {
  return (
    err instanceof TypeError ||
    err instanceof ReferenceError ||
    err instanceof RangeError ||
    err instanceof SyntaxError
  );
}

/**
 * True when a formatted backend error is the stable missing-file signal from
 * `get_file_blob` / `get_file_content`. Coverage treats that as an empty
 * state rather than a diagnostics warning. Match anywhere in the string so a
 * wrapped IPC message still qualifies; `startsWith` misses those.
 */
export function isMissingFileError(reason: string): boolean {
  // `\b` so "profile not found:" (which contains the letters "file not found:")
  // cannot qualify. The backend signal is the token `File not found:`.
  return /\bfile not found:/i.test(reason);
}
