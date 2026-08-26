import { writable } from "svelte/store";
import { formatError } from "../ui/formatError";
import { browserStorage, type StorageLike } from "../repos/persist";

/**
 * Central capture of everything that goes wrong while the app runs: uncaught
 * errors, unhandled promise rejections, pane crashes, console.error/warn
 * calls, and user-facing errors. Entries land in a bounded, persisted ring
 * buffer so a crash can be diagnosed after the relaunch, and the Diagnostics
 * panel renders (or copies) the whole log for fixing.
 */

export type DiagnosticSeverity = "error" | "warning";

export interface DiagnosticEntry {
  /** Monotonic sequence id; higher is newer. */
  readonly id: number;
  /** Epoch milliseconds of the most recent occurrence. */
  readonly at: number;
  readonly severity: DiagnosticSeverity;
  /** Short tag naming where the entry came from ("console", "pane-crash", …). */
  readonly source: string;
  readonly message: string;
  /** Occurrences after coalescing identical consecutive repeats. */
  readonly count: number;
}

export const DIAGNOSTIC_STORAGE_KEY = "gitpulse_diagnostics_v1";
/** Ring-buffer bound; the oldest entries fall off first. */
export const MAX_DIAGNOSTIC_ENTRIES = 500;
/** Per-message cap so one huge payload cannot blow the storage quota. */
const MAX_MESSAGE_CHARS = 2000;

const SEVERITIES: readonly DiagnosticSeverity[] = ["error", "warning"];

export interface DiagnosticsStore {
  subscribe: (run: (entries: readonly DiagnosticEntry[]) => void) => () => void;
  error(source: string, detail: unknown): void;
  warn(source: string, detail: unknown): void;
  clear(): void;
}

function clampMessage(message: string): string {
  return message.length > MAX_MESSAGE_CHARS
    ? `${message.slice(0, MAX_MESSAGE_CHARS)}…`
    : message;
}

/**
 * Webview-host and Vite-HMR chatter that is not a GitPulse failure.
 *
 * WKWebView cannot apply Vite's ESM/CSS hot swap (`module.default`); the
 * page then reloads while Rust still holds IPC callbacks, and Tauri falls
 * back from the custom protocol to postMessage. Those messages would
 * otherwise drown the diagnostics ring (and survive relaunch via the
 * persisted blob).
 */
export function isHostRuntimeNoise(message: string): boolean {
  const text = message.trim();
  if (text.startsWith("[TAURI] Couldn't find callback id ")) return true;
  if (
    text.includes(
      "IPC custom protocol failed, Tauri will now use the postMessage interface instead",
    )
  ) {
    return true;
  }
  if (text.startsWith("[hmr] Failed to reload ")) return true;
  if (text === "Importing a module script failed." || text === "Importing a module script failed") {
    return true;
  }
  if (text.includes("(evaluating 'module.default')")) return true;
  return false;
}

function sanitizeEntry(raw: unknown): DiagnosticEntry | null {
  if (!raw || typeof raw !== "object") return null;
  const record = raw as Record<string, unknown>;
  if (
    typeof record.id !== "number" ||
    !Number.isFinite(record.id) ||
    typeof record.at !== "number" ||
    !Number.isFinite(record.at) ||
    typeof record.source !== "string" ||
    typeof record.message !== "string" ||
    !record.message
  ) {
    return null;
  }
  const severity = SEVERITIES.find((candidate) => candidate === record.severity);
  if (!severity) return null;
  const count =
    typeof record.count === "number" && Number.isInteger(record.count) && record.count >= 1
      ? Math.min(record.count, Number.MAX_SAFE_INTEGER)
      : 1;
  return {
    id: record.id,
    at: record.at,
    severity,
    source: clampMessage(record.source),
    message: clampMessage(record.message),
    count,
  };
}

function loadPersisted(storage: StorageLike | null): {
  entries: DiagnosticEntry[];
  nextId: number;
  rewritten: boolean;
} {
  if (!storage) return { entries: [], nextId: 0, rewritten: false };
  let parsed: unknown = null;
  try {
    const raw = storage.getItem(DIAGNOSTIC_STORAGE_KEY);
    parsed = raw ? (JSON.parse(raw) as unknown) : null;
  } catch {
    /* corrupt blob behaves like an empty log */
  }
  if (!Array.isArray(parsed)) return { entries: [], nextId: 0, rewritten: false };
  const sanitized = parsed
    .map(sanitizeEntry)
    .filter((entry): entry is DiagnosticEntry => entry !== null);
  const entries = sanitized
    .filter((entry) => !isHostRuntimeNoise(entry.message))
    // Newest first regardless of how the blob was written.
    .sort((a, b) => b.id - a.id)
    .slice(0, MAX_DIAGNOSTIC_ENTRIES);
  const nextId = entries.length > 0 ? entries[0].id : 0;
  const rewritten = sanitized.length !== parsed.length || entries.length !== sanitized.length;
  return { entries, nextId, rewritten };
}

export function createDiagnostics(deps: { storage?: StorageLike | null; now?: () => number } = {}): DiagnosticsStore {
  // `undefined` means "pick the browser storage"; explicit null disables it.
  const storage = deps.storage !== undefined ? deps.storage : browserStorage();
  const now = deps.now ?? (() => Date.now());
  const restored = loadPersisted(storage);

  let entries: DiagnosticEntry[] = restored.entries;
  let nextId = restored.nextId;
  const store = writable<readonly DiagnosticEntry[]>(entries);

  function persist() {
    if (!storage) return;
    try {
      storage.setItem(DIAGNOSTIC_STORAGE_KEY, JSON.stringify(entries));
    } catch {
      /* quota / private mode — the in-memory ring stays authoritative */
    }
  }

  if (restored.rewritten) persist();

  function record(severity: DiagnosticSeverity, source: string, detail: unknown) {
    const message = clampMessage(formatError(detail));
    if (isHostRuntimeNoise(message)) return;
    const newest = entries[0];
    if (
      newest &&
      newest.severity === severity &&
      newest.source === source &&
      newest.message === message
    ) {
      // Coalesce repeats (error storms) into one entry with a counter.
      entries = [{ ...newest, count: newest.count + 1, at: now() }, ...entries.slice(1)];
      store.set(entries);
      persist();
      return;
    }
    nextId += 1;
    entries = [{ id: nextId, at: now(), severity, source, message, count: 1 }, ...entries];
    if (entries.length > MAX_DIAGNOSTIC_ENTRIES) {
      entries = entries.slice(0, MAX_DIAGNOSTIC_ENTRIES);
    }
    store.set(entries);
    persist();
  }

  return {
    subscribe: store.subscribe,
    error: (source, detail) => record("error", source, detail),
    warn: (source, detail) => record("warning", source, detail),
    clear: () => {
      entries = [];
      nextId = 0;
      store.set(entries);
      if (storage) {
        try {
          storage.removeItem(DIAGNOSTIC_STORAGE_KEY);
        } catch {
          /* ignore removal failures; memory is already cleared */
        }
      }
    },
  };
}

/** App-wide singleton; safe to import from stores and components alike. */
export const diagnostics: DiagnosticsStore = createDiagnostics();

/**
 * Render entries as a plain-text report for pasting into an issue or handing
 * to a fixer. Input order is preserved (newest first).
 */
export function formatDiagnosticReport(
  entries: readonly DiagnosticEntry[],
  generatedAt: Date = new Date(),
): string {
  const occurrences = (severity: DiagnosticSeverity) =>
    entries.reduce((total, entry) => (entry.severity === severity ? total + entry.count : total), 0);
  if (entries.length === 0) {
    return `GitPulse diagnostics — nothing recorded as of ${generatedAt.toISOString()}`;
  }
  const blocks = entries.map((entry) => {
    const repeats = entry.count > 1 ? ` x${entry.count}` : "";
    const header = `[${new Date(entry.at).toISOString()}] ${entry.severity.toUpperCase()}${repeats} (${entry.source})`;
    const body = entry.message
      .split("\n")
      .map((line) => `  ${line}`)
      .join("\n");
    return `${header}\n${body}`;
  });
  return [
    `GitPulse diagnostics — ${occurrences("error")} error(s), ${occurrences("warning")} warning(s), ${entries.length} distinct`,
    `Generated: ${generatedAt.toISOString()}`,
    "",
    ...blocks,
  ].join("\n");
}

/** Local-clock rendering for the panel list: time only for today, date+time otherwise. */
export function formatDiagnosticTime(at: number, now: number = Date.now()): string {
  const date = new Date(at);
  const reference = new Date(now);
  const time = date.toLocaleTimeString([], { hour12: false });
  return date.toDateString() === reference.toDateString()
    ? time
    : `${date.toLocaleDateString()} ${time}`;
}

/** The slices of window/console the global installer needs. */
interface DiagnosticEventLike {
  reason?: unknown;
  error?: unknown;
  message?: unknown;
}
interface DiagnosticEventTarget {
  addEventListener(type: string, listener: (event: DiagnosticEventLike) => void): void;
  removeEventListener(type: string, listener: (event: DiagnosticEventLike) => void): void;
}
interface ConsoleLike {
  error(...args: unknown[]): void;
  warn(...args: unknown[]): void;
}

/**
 * Route every failure channel into a diagnostics sink while keeping today's
 * devtools behavior (the original console call still runs with untouched
 * arguments). Returns an uninstaller that restores both surfaces exactly.
 *
 * The re-entrancy flag stops loops: recording must never trigger another
 * recorded console call (e.g. a sink that logs its own failure).
 */
export function installGlobalDiagnostics(
  sink: Pick<DiagnosticsStore, "error" | "warn">,
  deps: { target?: DiagnosticEventTarget | null; console?: ConsoleLike } = {},
): () => void {
  const target =
    deps.target !== undefined ? deps.target : typeof window !== "undefined" ? window : null;
  const con = deps.console ?? console;
  const originalError = con.error.bind(con);
  const originalWarn = con.warn.bind(con);
  let forwarding = false;

  function note(severity: DiagnosticSeverity, source: string, parts: unknown[]) {
    if (forwarding) return;
    const message = parts.map((part) => formatError(part)).join(" ");
    if (isHostRuntimeNoise(message)) return;
    forwarding = true;
    try {
      // Severity names ("warning") differ from sink method names ("warn").
      if (severity === "error") sink.error(source, message);
      else sink.warn(source, message);
    } finally {
      forwarding = false;
    }
  }

  const wrappedError = (...args: unknown[]) => {
    note("error", "console", args);
    originalError(...args);
  };
  const wrappedWarn = (...args: unknown[]) => {
    note("warning", "console", args);
    originalWarn(...args);
  };

  const onUnhandledRejection = (event: DiagnosticEventLike) => {
    const detail = formatError(event.reason);
    if (isHostRuntimeNoise(detail)) return;
    note("error", "unhandled-rejection", [detail]);
    originalError(`[gitpulse] unhandled promise rejection: ${detail}`);
  };
  const onUncaughtError = (event: DiagnosticEventLike) => {
    const detail = formatError(event.error ?? event.message);
    if (isHostRuntimeNoise(detail)) return;
    note("error", "uncaught-error", [detail]);
    originalError(`[gitpulse] uncaught error: ${detail}`);
  };

  con.error = wrappedError;
  con.warn = wrappedWarn;
  target?.addEventListener("unhandledrejection", onUnhandledRejection);
  target?.addEventListener("error", onUncaughtError);

  return () => {
    target?.removeEventListener("unhandledrejection", onUnhandledRejection);
    target?.removeEventListener("error", onUncaughtError);
    if (con.error === wrappedError) con.error = originalError;
    if (con.warn === wrappedWarn) con.warn = originalWarn;
  };
}
