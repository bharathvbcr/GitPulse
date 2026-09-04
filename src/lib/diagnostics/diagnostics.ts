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
  /**
   * Set when the folded occurrences were not textually identical.
   *
   * Coalescing groups by {@link diagnosticFingerprint}, which deliberately
   * ignores per-run detail, so `count` can cover occurrences that differed in
   * a duration or a timestamp. Without this flag `x3` would claim three
   * verbatim repeats — a summary reading as more exact than its evidence.
   */
  readonly varied?: boolean;
}

export const DIAGNOSTIC_STORAGE_KEY = "gitpulse_diagnostics_v1";
/** Ring-buffer bound; the oldest entries fall off first. */
export const MAX_DIAGNOSTIC_ENTRIES = 500;
/** Per-message cap so one huge payload cannot blow the storage quota. */
const MAX_MESSAGE_CHARS = 2000;
/** Version strings are short; a persisted blob does not get to say otherwise. */
const MAX_VERSION_CHARS = 32;

const SEVERITIES: readonly DiagnosticSeverity[] = ["error", "warning"];

/**
 * Credential shapes mirrored from the native dc-verify redaction boundary.
 *
 * The frontend ring is synchronous and can be written before Tauri IPC is
 * available (including during boot failures), so it cannot delegate this
 * first write to Rust. Native logs and the ledger still use dc-verify as the
 * canonical detector; this defensive mirror keeps the same common vendor
 * tokens out of localStorage before any asynchronous boundary exists.
 */
const DIAGNOSTIC_SECRET_PREFIXES: readonly {
  prefix: string;
  minLength: number;
  alphanumericBody?: boolean;
}[] = [
  { prefix: "sk-ant-", minLength: 24 },
  { prefix: "sk-proj-", minLength: 24 },
  { prefix: "sk_live_", minLength: 24 },
  { prefix: "github_pat_", minLength: 40 },
  { prefix: "glpat-", minLength: 26 },
  { prefix: "xoxb-", minLength: 30 },
  { prefix: "xoxp-", minLength: 30 },
  { prefix: "xoxa-", minLength: 30 },
  { prefix: "xapp-", minLength: 30 },
  { prefix: "ghp_", minLength: 30 },
  { prefix: "gho_", minLength: 30 },
  { prefix: "ghs_", minLength: 30 },
  { prefix: "ghu_", minLength: 30 },
  { prefix: "ghr_", minLength: 30 },
  { prefix: "npm_", minLength: 36 },
  { prefix: "hf_", minLength: 30 },
  { prefix: "AIza", minLength: 30 },
  { prefix: "AKIA", minLength: 20, alphanumericBody: true },
  { prefix: "ASIA", minLength: 20, alphanumericBody: true },
  { prefix: "xai-", minLength: 20 },
  { prefix: "sk-", minLength: 45, alphanumericBody: true },
];

const SEPARATE_SECRET_FLAGS = new Set([
  "password",
  "passwd",
  "api_key",
  "apikey",
  "access_token",
  "refresh_token",
  "client_secret",
  "secret",
  "token",
  "auth_token",
  "oauth_token",
  "oauth2_bearer",
  "aws_secret_access_key",
  "aws_session_token",
  "aws_access_key_id",
]);

function escapeRegExp(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

function redactAssignedValue(_match: string, prefix: string, raw: string): string {
  const quoted = raw.startsWith('"') && raw.endsWith('"')
    ? '"<redacted>"'
    : raw.startsWith("'") && raw.endsWith("'")
      ? "'<redacted>'"
      : "<redacted>";
  return `${prefix}${quoted}`;
}

function redactAuthorizationValue(
  _match: string,
  prefix: string,
  scheme: string | undefined,
  raw: string,
): string {
  const quoted = raw.startsWith('"') && raw.endsWith('"')
    ? '"<redacted>"'
    : raw.startsWith("'") && raw.endsWith("'")
      ? "'<redacted>'"
      : "<redacted>";
  return `${prefix}${scheme ? `${scheme} ` : ""}${quoted}`;
}

function redactEmbeddedAssignment(
  _match: string,
  prefix: string,
  _raw: string,
  suffix: string,
): string {
  return `${prefix}<redacted>${suffix}`;
}

function normalizeCliFlag(value: string): string | null {
  if (!value.startsWith("-")) return null;
  return value.replace(/^-+/, "").replaceAll("-", "_").toLowerCase();
}

function isSeparateSecretFlag(value: string): boolean {
  return SEPARATE_SECRET_FLAGS.has(normalizeCliFlag(value) ?? "");
}

function isUserinfoFlag(value: string): boolean {
  const flag = normalizeCliFlag(value);
  return flag === "user" || flag === "userpass" || flag === "proxy_user" || value === "-u";
}

function redactUserinfo(value: string): string | null {
  const delimiter = value.indexOf(":");
  if (delimiter < 0) return null;
  const password = value.slice(delimiter + 1);
  if (!password || password === "<redacted>") return null;
  return `${value.slice(0, delimiter)}:<redacted>`;
}

function redactCliArray(values: unknown[]): boolean {
  let changed = false;
  for (let index = 0; index < values.length; index += 1) {
    const argument = values[index];
    if (typeof argument !== "string") continue;

    const equals = argument.indexOf("=");
    if (equals >= 0) {
      const flag = argument.slice(0, equals);
      const inlineValue = argument.slice(equals + 1);
      let replacement: string | null = null;
      if (isSeparateSecretFlag(flag) && inlineValue !== "<redacted>") {
        replacement = `${flag}=<redacted>`;
      } else if (isUserinfoFlag(flag)) {
        const redacted = redactUserinfo(inlineValue);
        if (redacted) replacement = `${flag}=${redacted}`;
      }
      if (replacement) {
        values[index] = replacement;
        changed = true;
      }
      continue;
    }

    if (index + 1 >= values.length) continue;
    const next = values[index + 1];
    if (typeof next !== "string") continue;
    if (isSeparateSecretFlag(argument) && next !== "<redacted>") {
      values[index + 1] = "<redacted>";
      changed = true;
      index += 1;
    } else if (isUserinfoFlag(argument)) {
      const replacement = redactUserinfo(next);
      if (replacement) {
        values[index + 1] = replacement;
        changed = true;
      }
      index += 1;
    }
  }
  return changed;
}

const MAX_SERIALIZED_NESTING = 32;

interface RedactedCliJsonValue {
  value: unknown;
  changed: boolean;
}

function redactCliJsonValue(value: unknown, depth = 0): RedactedCliJsonValue {
  if (depth >= MAX_SERIALIZED_NESTING) {
    // Reaching the work bound means the remaining subtree was not inspected.
    // Replace it rather than treating "not checked" as "safe". Keeping the
    // replacement inside the parsed value preserves every enclosing JSON layer.
    return value === "<redacted>"
      ? { value, changed: false }
      : { value: "<redacted>", changed: true };
  }
  if (Array.isArray(value)) {
    let changed = redactCliArray(value);
    for (let index = 0; index < value.length; index += 1) {
      const nested = redactCliJsonValue(value[index], depth + 1);
      if (nested.changed) {
        value[index] = nested.value;
        changed = true;
      }
    }
    return { value, changed };
  }
  if (value && typeof value === "object") {
    let changed = false;
    for (const [key, valueAtKey] of Object.entries(value)) {
      const nested = redactCliJsonValue(valueAtKey, depth + 1);
      if (nested.changed) {
        (value as Record<string, unknown>)[key] = nested.value;
        changed = true;
      }
    }
    return { value, changed };
  }
  if (typeof value === "string") {
    const redacted = redactValueAtDepth(value, depth + 1);
    return { value: redacted, changed: redacted !== value };
  }
  return { value, changed: false };
}

function redactSerializedCliValues(value: string, depth = 0): string {
  if (depth >= MAX_SERIALIZED_NESTING) return value;
  try {
    const parsed: unknown = JSON.parse(value);
    const redacted = redactCliJsonValue(parsed, depth);
    return redacted.changed ? JSON.stringify(redacted.value) : value;
  } catch {
    return value;
  }
}

function jsonArrayEnd(value: string, start: number): number | null {
  let depth = 0;
  let inString = false;
  let escaped = false;
  for (let index = start; index < value.length; index += 1) {
    const character = value[index];
    if (inString) {
      if (escaped) {
        escaped = false;
      } else if (character === "\\") {
        escaped = true;
      } else if (character === '"') {
        inString = false;
      }
      continue;
    }
    if (character === '"') {
      inString = true;
    } else if (character === "[") {
      depth += 1;
    } else if (character === "]") {
      depth -= 1;
      if (depth === 0) return index + 1;
      if (depth < 0) return null;
    }
  }
  return null;
}

/**
 * Native log records wrap JSON argv in a timestamp/category prefix. Redact
 * every complete array fragment instead of requiring the entire record to be
 * JSON, while leaving malformed fragments untouched for later diagnostics.
 */
function redactEmbeddedCliArrays(value: string, depth = 0): string {
  let out = "";
  let cursor = 0;
  while (cursor < value.length) {
    const start = value.indexOf("[", cursor);
    if (start < 0) break;
    const end = jsonArrayEnd(value, start);
    if (end === null) {
      out += value.slice(cursor, start + 1);
      cursor = start + 1;
      continue;
    }
    out += value.slice(cursor, start);
    out += redactSerializedCliValues(value.slice(start, end), depth);
    cursor = end;
  }
  return out + value.slice(cursor);
}

function redactValueAtDepth(value: string, depth: number): string {
  let isJson = false;
  if (depth < MAX_SERIALIZED_NESTING) {
    try {
      JSON.parse(value);
      isJson = true;
    } catch {
      // Wrapper prose continues through the embedded-array and contextual
      // stages below.
    }
  }
  const serialized = redactSerializedCliValues(value, depth);
  // Parsed JSON has already been traversed string-by-string. Applying the
  // contextual regexes to its escaped serialization can consume an inner
  // closing quote and silently damage the nested document.
  if (isJson) return serialized;
  return redactContextualDiagnosticText(redactEmbeddedCliArrays(serialized, depth));
}

function redactContextualDiagnosticText(value: string): string {
  let out = value
    // A private-key body has no reliable prefix of its own, so remove the
    // entire block before line-oriented token matching.
    .replace(
      /-----BEGIN[^\r\n]*?PRIVATE KEY(?: BLOCK)?-----[\s\S]*?(?:-----END[^\r\n]*?PRIVATE KEY(?: BLOCK)?-----|$)/gi,
      "<private key redacted>",
    )
    // Quoted JSON/log fields have an explicit boundary. Handle them before
    // line-oriented headers so a following `}` is not swallowed.
    .replace(
      /(["']?authorization["']?\s*[:=]\s*)("[^"\r\n]*"|'[^'\r\n]*')/gi,
      (_match, prefix: string, raw: string) =>
        redactAuthorizationValue(_match, prefix, undefined, raw),
    )
    // Serialized argv and nested diagnostic fields put the header inside a
    // quoted string. Consume escaped characters until the real closing quote
    // so `\\n` or `\\"` cannot terminate redaction early.
    .replace(
      /(authorization\s*[:=]\s*)(?:(basic|bearer|digest|token|negotiate|aws4-hmac-sha256)\s+)?((?:\\.|[^"\\\r\n])+?)(")/gi,
      (
        match: string,
        prefix: string,
        scheme: string | undefined,
        raw: string,
        suffix: string,
      ) => `${redactAuthorizationValue(match, prefix, scheme, raw)}${suffix}`,
    )
    .replace(
      /(authorization\s*[:=]\s*)(?:(basic|bearer|digest|token|negotiate|aws4-hmac-sha256)\s+)?([^"'\\\r\n]+?)(\\?["'])/gi,
      (
        match: string,
        prefix: string,
        scheme: string | undefined,
        raw: string,
        suffix: string,
      ) => `${redactAuthorizationValue(match, prefix, scheme, raw)}${suffix}`,
    )
    // Header values normally run to end-of-line. Diagnostic prose sometimes
    // appends a URL on that same line; retain that separately so its password
    // can pass through the URL redactor instead of disappearing with the auth
    // value. Everything else on the header line remains fail-closed.
    .replace(
      /(["']?authorization["']?\s*[:=]\s*)(?:(basic|bearer|digest|token|negotiate|aws4-hmac-sha256)\s+)?([^"'\r\n]*?)(\s+[a-z][a-z0-9+.-]*:\/\/[^\r\n]*)?$/gim,
      (
        match: string,
        prefix: string,
        scheme: string | undefined,
        raw: string,
        suffix: string | undefined,
      ) => `${redactAuthorizationValue(match, prefix, scheme, raw)}${suffix ?? ""}`,
    )
    .replace(
      /(["']?(?:set-)?cookie["']?\s*[:=]\s*)("[^"\r\n]*"|'[^'\r\n]*')/gi,
      redactAssignedValue,
    )
    .replace(
      /((?:set-)?cookie\s*[:=]\s*)((?:\\.|[^"\\\r\n])+?)(")/gi,
      redactEmbeddedAssignment,
    )
    .replace(
      /((?:set-)?cookie\s*[:=]\s*)([^"'\\\r\n]+?)(\\?["'])/gi,
      redactEmbeddedAssignment,
    )
    .replace(
      /(["']?(?:set-)?cookie["']?\s*[:=]\s*)([^"'\\\r\n]+)$/gim,
      redactAssignedValue,
    )
    .replace(
      /([a-z][a-z0-9+.-]*:\/\/[^/\s:@]+:)[^@\s/?#]+@/gi,
      "$1<redacted>@",
    )
    .replace(
      /((?:password|passwd|api[_-]?key|access[_-]?token|refresh[_-]?token|client[_-]?secret|secret|aws[_-]?secret[_-]?access[_-]?key|aws[_-]?session[_-]?token|aws[_-]?access[_-]?key[_-]?id)\s*[:=]\s*)((?:\\.|[^"\\\r\n])+?)(")/gi,
      redactEmbeddedAssignment,
    )
    .replace(
      /((?:password|passwd|api[_-]?key|access[_-]?token|refresh[_-]?token|client[_-]?secret|secret|aws[_-]?secret[_-]?access[_-]?key|aws[_-]?session[_-]?token|aws[_-]?access[_-]?key[_-]?id)\s*[:=]\s*)([^"'\\\r\n]+?)(\\?["'])/gi,
      redactEmbeddedAssignment,
    )
    .replace(
      /(["']?(?:password|passwd|api[_-]?key|access[_-]?token|refresh[_-]?token|client[_-]?secret|secret|aws[_-]?secret[_-]?access[_-]?key|aws[_-]?session[_-]?token|aws[_-]?access[_-]?key[_-]?id)["']?\s*[:=]\s*)("[^"\r\n]*"|'[^'\r\n]*'|[^\s,;&\\"'\]]+)/gi,
      redactAssignedValue,
    )
    .replace(
      /((?:--password|--passwd|--api-key|--apikey|--access-token|--refresh-token|--client-secret|--secret|--token|--auth-token|--oauth-token|--oauth2-bearer|--aws-secret-access-key|--aws-session-token|--aws-access-key-id)\s+)("[^"\r\n]*"|'[^'\r\n]*'|[^\s"'\\]+)/gi,
      redactAssignedValue,
    )
    .replace(
      /((?:--user|--userpass|--proxy-user|-u)\s+)("[^"\r\n]*:[^"\r\n]*"|'[^'\r\n]*:[^'\r\n]*'|[^\s"'\\:]+:[^\s"'\\]+)/gi,
      redactAssignedValue,
    );

  for (const { prefix, minLength, alphanumericBody } of DIAGNOSTIC_SECRET_PREFIXES) {
    const matcher = new RegExp(
      `(^|[^A-Za-z0-9_])(${escapeRegExp(prefix)}[^\\s"',;]*)`,
      "g",
    );
    out = out.replace(matcher, (match, boundary: string, token: string) => {
      if (token.length < minLength) return match;
      const body = token.slice(prefix.length);
      if (alphanumericBody && !/^[A-Za-z0-9]+$/.test(body)) return match;
      return `${boundary}${prefix}… (${token.length} chars)`;
    });
  }
  return out;
}

/** Redacts credentials before a diagnostic can reach memory or persistence. */
export function redactDiagnosticText(value: string): string {
  return redactValueAtDepth(value, 0);
}

/** Formats untrusted thrown values without allowing a hostile getter to fail diagnostics. */
export function formatDiagnosticFailure(detail: unknown): string {
  try {
    return redactDiagnosticText(formatError(detail));
  } catch {
    return "Unknown error";
  }
}

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
 * Spans that differ between runs of the *same* failure.
 *
 * Coalescing used to demand byte equality, which any tool that stamps its own
 * runtime into its output defeats: three runs of one unchanged pytest failure
 * landed as three "distinct" entries because the trailer read `17.30s`,
 * `17.00s`, `17.99s`. That is benign for three manual runs and dangerous for a
 * storm — 500 near-identical entries flush every older, unrelated failure out
 * of the ring, which is precisely what coalescing exists to prevent.
 *
 * Each pattern here names *when* or *where* something happened. None of them
 * can distinguish one failure from another, so masking them cannot merge two
 * genuine problems. Everything that does discriminate — exit codes, file
 * paths, line numbers, error types, repository names — is left untouched.
 */
const VOLATILE_SPANS: readonly (readonly [RegExp, string])[] = [
  // GitPulse's own elision notice: its count tracks the message length, so
  // two clamped copies of one failure disagree on it.
  [/… \d+ characters elided …/g, "… ⟨n⟩ characters elided …"],
  [/\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})?/g, "⟨timestamp⟩"],
  [
    /\b[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}\b/g,
    "⟨uuid⟩",
  ],
  [/\b\d{1,2}:\d{2}:\d{2}(?:\.\d+)?\b/g, "⟨time⟩"],
  // Elapsed times. The unit is required, so bare numbers — exit codes, line
  // numbers, counts — are never touched.
  [/\b\d+(?:\.\d+)?\s?(?:ns|µs|us|ms|s|m|h)\b/g, "⟨duration⟩"],
  [/\b0x[0-9a-fA-F]+\b/g, "⟨address⟩"],
  [/(?:\/private)?\/(?:var\/folders|tmp)\/\S+/g, "⟨tmp⟩"],
];

/**
 * The identity of a failure, with per-run detail masked out.
 *
 * Two messages sharing a fingerprint are the same problem happening again;
 * the entry keeps the most recent text and counts the occurrences.
 */
export function diagnosticFingerprint(message: string): string {
  let key = message;
  for (const [pattern, placeholder] of VOLATILE_SPANS) {
    key = key.replace(pattern, placeholder);
  }
  return key;
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
  if (text.includes("ResizeObserver loop completed with undelivered notifications")) return true;
  if (text.includes("ResizeObserver loop limit exceeded")) return true;
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
    source: clampMessage(redactDiagnosticText(record.source)),
    message: clampMessage(redactDiagnosticText(record.message)),
    count,
    ...(version ? { version } : {}),
    // Strictly `true`; a truthy string from a hostile blob must not become a
    // disclosure the app never made.
    ...(record.varied === true ? { varied: true } : {}),
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
  const sanitizedChanged = sanitized.some(
    (entry, index) => JSON.stringify(entry) !== JSON.stringify(parsed[index]),
  );
  const rewritten =
    sanitized.length !== parsed.length || entries.length !== sanitized.length || sanitizedChanged;
  return { entries, nextId, rewritten };
}

export function createDiagnostics(deps: { storage?: StorageLike | null; now?: () => number } = {}): DiagnosticsStore {
  // `undefined` means "pick the browser storage"; explicit null disables it.
  const storage = deps.storage !== undefined ? deps.storage : browserStorage();
  const now = deps.now ?? (() => Date.now());
  const restored = loadPersisted(storage);

  let entries: DiagnosticEntry[] = restored.entries;
  let nextId = restored.nextId;
  // Only the newest entry can absorb a repeat, so one cached key is enough —
  // and it keeps an error storm from re-normalizing the head on every event.
  let headKey: string | null =
    entries.length > 0 ? diagnosticFingerprint(entries[0].message) : null;
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
    const safeSource = clampMessage(redactDiagnosticText(source));
    const message = clampMessage(formatDiagnosticFailure(detail));
    if (isHostRuntimeNoise(message)) return;
    const key = diagnosticFingerprint(message);
    const newest = entries[0];
    if (
      newest &&
      newest.severity === severity &&
      newest.source === safeSource &&
      headKey === key &&
      // Never fold a new occurrence into an entry recorded by a different
      // build: coalescing rewrites the timestamp, so the merged entry would
      // claim the older build produced something it never saw.
      newest.version === APP_VERSION
    ) {
      // Coalesce repeats (error storms) into one entry with a counter. The
      // retained text is the newest occurrence's, because `at` moves to the
      // newest too: keeping the first would date one occurrence and quote
      // another.
      const varied = newest.varied === true || newest.message !== message;
      entries = [
        {
          ...newest,
          message,
          count: newest.count + 1,
          at: now(),
          ...(varied ? { varied: true as const } : {}),
        },
        ...entries.slice(1),
      ];
      store.set(entries);
      persist();
      return;
    }
    nextId += 1;
    entries = [
      { id: nextId, at: now(), severity, source: safeSource, message, count: 1, version: APP_VERSION },
      ...entries,
    ];
    headKey = key;
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
      headKey = null;
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
    // `x3` on its own would read as three verbatim repeats.
    const spread =
      entry.count > 1 && entry.varied ? " [occurrences differed; showing the most recent]" : "";
    const header = `[${new Date(entry.at).toISOString()}] ${entry.severity.toUpperCase()}${repeats} (${redactDiagnosticText(entry.source)})${spread}${staleBuildNote(entry.version, runningVersion)}`;
    const body = redactDiagnosticText(entry.message)
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
    const message = parts.map(formatDiagnosticFailure).join(" ");
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
    const detail = formatDiagnosticFailure(event.reason);
    if (isHostRuntimeNoise(detail)) return;
    note("error", "unhandled-rejection", [detail]);
    originalError(`[gitpulse] unhandled promise rejection: ${detail}`);
  };
  const onUncaughtError = (event: DiagnosticEventLike) => {
    const detail = formatDiagnosticFailure(event.error ?? event.message);
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
