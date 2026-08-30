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

/**
 * The build that is running, injected at build time from package.json.
 *
 * Guarded with `typeof` so a runner that does not define it degrades to
 * "unknown" rather than throwing at module load — the diagnostics ring is the
 * last thing that should fail when something else already has.
 */
export const APP_VERSION: string =
  typeof __APP_VERSION__ === "string" && __APP_VERSION__ ? __APP_VERSION__ : "unknown";

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
  /**
   * The app version that recorded this entry.
   *
   * Optional because the ring is persisted and survives upgrades: an entry
   * written before stamping existed has none, and that absence is itself the
   * useful fact. Without this, a log copied after an upgrade presented
   * entries from an older build as if they described the running one — which
   * is exactly how a fixed bug reads as a live one.
   */
  readonly version?: string;
}

export const DIAGNOSTIC_STORAGE_KEY = "gitpulse_diagnostics_v1";
/** Ring-buffer bound; the oldest entries fall off first. */
export const MAX_DIAGNOSTIC_ENTRIES = 500;
/** Per-message cap so one huge payload cannot blow the storage quota. */
const MAX_MESSAGE_CHARS = 2000;
/** Version strings are short; a persisted blob does not get to say otherwise. */
const MAX_VERSION_CHARS = 32;

const SEVERITIES: readonly DiagnosticSeverity[] = ["error", "warning"];

export interface DiagnosticsStore {
  subscribe: (run: (entries: readonly DiagnosticEntry[]) => void) => () => void;
  error(source: string, detail: unknown): void;
  warn(source: string, detail: unknown): void;
  clear(): void;
}

/**
 * Share of the budget given to the head. The tail gets the rest: a command's
 * output usually opens with routine chatter and *ends* with the reason it
 * failed, so the tail is the more valuable half.
 */
const CLAMP_HEAD_SHARE = 0.35;

/**
 * Bounds a message to [`MAX_MESSAGE_CHARS`], keeping both ends and saying how
 * much was dropped.
 *
 * Head-only truncation kept the wrong half. A real coverage failure produced
 * an 18,580-character message whose cause — `bench/stress_test.py:944`,
 * `SystemExit: 0`, "no tests ran" — sat at character 18,356; the 2,000
 * character head held nothing but the aborting script's own `ok …` chatter,
 * and every marker of the cause was discarded. The entry that survived was
 * the one a user would copy to report the problem.
 *
 * Keeping both ends preserves what the head is actually good for (which
 * command, which repository) without throwing away the ending, and the
 * elision is announced so a clipped message is never mistaken for a whole one.
 */
function clampMessage(message: string): string {
  if (message.length <= MAX_MESSAGE_CHARS) return message;
  const marker = (dropped: number) => `\n… ${dropped} characters elided …\n`;
  // The notice is paid for out of the budget, not added to it, so a clamped
  // entry still never exceeds MAX_MESSAGE_CHARS. Reserving against the widest
  // the notice could be is safe: `dropped` cannot have more digits than the
  // message has characters.
  const body = MAX_MESSAGE_CHARS - marker(message.length).length;
  const headChars = Math.floor(body * CLAMP_HEAD_SHARE);
  const tailChars = body - headChars;
  return (
    message.slice(0, headChars) +
    marker(message.length - body) +
    message.slice(message.length - tailChars)
  );
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
  // A missing version is legitimate (an entry from before stamping); a
  // malformed one is not, and must not be echoed back into the report.
  const version =
    typeof record.version === "string" && record.version.trim()
      ? record.version.trim().slice(0, MAX_VERSION_CHARS)
      : undefined;
  return {
    id: record.id,
    at: record.at,
    severity,
    source: clampMessage(record.source),
    message: clampMessage(record.message),
    count,
    ...(version ? { version } : {}),
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
      newest.message === message &&
      // Never fold a new occurrence into an entry recorded by a different
      // build: coalescing rewrites the timestamp, so the merged entry would
      // claim the older build produced something it never saw.
      newest.version === APP_VERSION
    ) {
      // Coalesce repeats (error storms) into one entry with a counter.
      entries = [{ ...newest, count: newest.count + 1, at: now() }, ...entries.slice(1)];
      store.set(entries);
      persist();
      return;
    }
    nextId += 1;
    entries = [
      { id: nextId, at: now(), severity, source, message, count: 1, version: APP_VERSION },
      ...entries,
    ];
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
/**
 * How an entry's recording build relates to the one running.
 *
 * Only entries that did *not* come from the running build are annotated, so
 * the common case stays uncluttered and the annotation means something when
 * it appears.
 */
export function staleBuildNote(
  entryVersion: string | undefined,
  runningVersion: string,
): string {
  if (entryVersion === runningVersion) return "";
  return entryVersion
    ? ` [recorded by ${entryVersion}, now running ${runningVersion}]`
    : ` [recorded by an earlier build, now running ${runningVersion}]`;
}

export function formatDiagnosticReport(
  entries: readonly DiagnosticEntry[],
  generatedAt: Date = new Date(),
  runningVersion: string = APP_VERSION,
): string {
  const occurrences = (severity: DiagnosticSeverity) =>
    entries.reduce((total, entry) => (entry.severity === severity ? total + entry.count : total), 0);
  if (entries.length === 0) {
    return `GitPulse diagnostics — nothing recorded as of ${generatedAt.toISOString()}`;
  }
  const blocks = entries.map((entry) => {
    const repeats = entry.count > 1 ? ` x${entry.count}` : "";
    const header = `[${new Date(entry.at).toISOString()}] ${entry.severity.toUpperCase()}${repeats} (${entry.source})${staleBuildNote(entry.version, runningVersion)}`;
    const body = entry.message
      .split("\n")
      .map((line) => `  ${line}`)
      .join("\n");
    return `${header}\n${body}`;
  });
  return [
    `GitPulse diagnostics — ${occurrences("error")} error(s), ${occurrences("warning")} warning(s), ${entries.length} distinct`,
    `Generated: ${generatedAt.toISOString()} by GitPulse ${runningVersion}`,
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
