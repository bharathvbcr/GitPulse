import { writable, type Readable } from "svelte/store";
import { formatError } from "../ui/formatError";
import { browserStorage, type StorageLike } from "../repos/persist";
import { escapeRegExp } from "../text/lineSearch";

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
export const APP_BUILD_ID: string =
  typeof __APP_BUILD_ID__ === "string" && __APP_BUILD_ID__ ? __APP_BUILD_ID__ : "unknown";

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
  /** Exact bundle identity; absent on entries saved by older versions. */
  readonly buildId?: string;
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

// The vocabulary is fixed for this build. Construct matchers once; global
// String.replace resets lastIndex before each scan, including nested calls.
const DIAGNOSTIC_SECRET_MATCHERS = DIAGNOSTIC_SECRET_PREFIXES.map(spec => ({
  ...spec,
  matcher: new RegExp(`(^|[^A-Za-z0-9_])(${escapeRegExp(spec.prefix)}[^\\s"',;]*)`, "g"),
}));

/**
 * Field names whose *value* is a credential, whatever syntax carries them.
 *
 * One table, three call sites: a CLI flag (`--client-secret X`), a JSON object
 * key (`{"client_secret": "X"}`) and the contextual `name=value` regexes below
 * must agree, because a name treated as secret in one syntax and ignored in
 * another is precisely how a credential redacted at one gate walks out of the
 * next. `authorization`, `cookie` and `private_key` are here because the
 * contextual stage already treats them as secret-bearing in prose; a JSON key
 * of the same name must not be weaker than the same name in a log line.
 *
 * Mirrored by `SECRET_FIELD_NAMES` in `src-tauri/src/ledger/redact.rs`, and
 * bound to it by `scripts/diagnostics-contract.test.ts` so the two cannot
 * drift apart unnoticed.
 */
export const SECRET_FIELD_NAMES: readonly string[] = [
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
  "authorization",
  "cookie",
  "set_cookie",
  "private_key",
];

/**
 * Trailing words that make a compound name a credential name.
 *
 * `github_token` and `webhook_secret` are credentials for the same reason
 * `token` and `secret` are, and enumerating every vendor prefix is the losing
 * half of that race. Deliberately excludes the bare word `key`, which would
 * swallow `public_key`, `cache_key` and `primary_key` and gut the diagnostic
 * value of the report to redact nothing that is actually secret.
 */
export const SECRET_FIELD_SUFFIXES: readonly string[] = [
  "password",
  "passwd",
  "secret",
  "token",
  "api_key",
  "apikey",
  "credential",
  "credentials",
];

const SEPARATE_SECRET_FLAGS = new Set(SECRET_FIELD_NAMES);

/**
 * A field name in comparable form: `X-Api-Key`, `clientSecret` and
 * `client-secret` all have to reach the table as `x_api_key`, `client_secret`
 * and `client_secret`.
 */
export function normalizeFieldName(key: string): string {
  let out = "";
  let prevWasLowerOrDigit = false;
  for (const character of key) {
    if (character === "-" || character === "_" || character === "." || character === " ") {
      if (out.length > 0 && !out.endsWith("_")) out += "_";
      prevWasLowerOrDigit = false;
      continue;
    }
    if (character >= "A" && character <= "Z") {
      // camelCase boundary: `accessToken` must not normalize to one word.
      if (prevWasLowerOrDigit && !out.endsWith("_")) out += "_";
      out += character.toLowerCase();
      prevWasLowerOrDigit = false;
    } else {
      out += character.toLowerCase();
      prevWasLowerOrDigit =
        (character >= "a" && character <= "z") || (character >= "0" && character <= "9");
    }
  }
  return out.replace(/^_+/, "").replace(/_+$/, "");
}

function isSecretName(normalized: string): boolean {
  if (SEPARATE_SECRET_FLAGS.has(normalized)) return true;
  return SECRET_FIELD_SUFFIXES.some(
    (suffix) => normalized.endsWith(suffix) && normalized.at(-suffix.length - 1) === "_",
  );
}

/** Whether a JSON object key declares its value to be a credential. */
export function isSecretFieldName(key: string): boolean {
  return isSecretName(normalizeFieldName(key));
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
  const flag = normalizeCliFlag(value);
  return flag === null ? false : isSecretName(flag);
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

/**
 * Redacts credentials that appear as object *keys* rather than values.
 *
 * The value side of this seam was blind to its keys; the key side was blind to
 * its own contents. A token is a token wherever it sits, and
 * `{"ghp_…": {…}}` — a cache or rate-limit map keyed by the credential — put
 * one in the diagnostics report in full while the identical token one
 * character to the right was redacted.
 *
 * A rename that would collide with a key already present is disambiguated
 * rather than allowed to overwrite: two distinct tokens of the same length
 * redact to the same text, and silently dropping one entry would make the
 * document claim there was only ever one.
 */
function redactObjectKeys(value: Record<string, unknown>, depth: number): boolean {
  const keys = Object.keys(value);
  const renames = new Map<string, string>();
  for (const key of keys) {
    const redacted = redactValueAtDepth(key, depth + 1);
    if (redacted !== key) renames.set(key, redacted);
  }
  if (renames.size === 0) return false;
  const rebuilt = new Map<string, unknown>();
  // Next ordinal per colliding base name. A linear probe from 2 on every
  // insertion is quadratic exactly when the attack is cheapest to mount: N
  // distinct tokens of equal length all redact to the SAME text, so key n pays
  // n probes. Measured at 20k such keys: 31s probing, 24ms with this counter.
  const nextOrdinal = new Map<string, number>();
  for (const key of keys) {
    const replacement = renames.get(key) ?? key;
    let candidate = replacement;
    let ordinal = nextOrdinal.get(replacement) ?? 2;
    // The counter alone can still land on a literal key already present
    // (a document containing both `x` and `x #2`), so confirm before using it.
    while (rebuilt.has(candidate)) {
      candidate = `${replacement} #${ordinal}`;
      ordinal += 1;
    }
    nextOrdinal.set(replacement, ordinal);
    rebuilt.set(candidate, value[key]);
    delete value[key];
  }
  for (const [key, entry] of rebuilt) {
    // `value[key] = entry` silently loses the entry when `key` is `__proto__`:
    // the own property was just deleted, so the assignment reaches
    // Object.prototype's setter and sets the prototype instead of storing
    // anything. JSON.parse creates `__proto__` as a genuine own property, so a
    // document can carry one, and dropping it would quietly shrink the report.
    Object.defineProperty(value, key, {
      value: entry,
      writable: true,
      enumerable: true,
      configurable: true,
    });
  }
  return true;
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
    let changed = redactObjectKeys(value as Record<string, unknown>, depth);
    for (const [key, valueAtKey] of Object.entries(value)) {
      // A key that names a credential says what its value is, and it says so
      // for every shape the value can take. Recursing instead would hand the
      // value to the contextual stage stripped of the only context that
      // identified it — which is how `{"access_token": "<opaque>"}`, the shape
      // every OAuth response has, reached the diagnostics report in full.
      if (isSecretFieldName(key)) {
        if (valueAtKey !== "<redacted>") {
          (value as Record<string, unknown>)[key] = "<redacted>";
          changed = true;
        }
        continue;
      }
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

function redactSerializedCliValues(value: string, depth = 0): { text: string; parsed: boolean } {
  // Only objects, arrays, and quoted strings can carry nested credentials.
  // Ordinary navigation text must not allocate an exception on every read.
  // JSON scalar literals cannot contain credentials and use the text path.
  if (depth >= MAX_SERIALIZED_NESTING || !/^\s*[[{"]/.test(value)) return { text: value, parsed: false };
  try {
    const parsed: unknown = JSON.parse(value);
    const redacted = redactCliJsonValue(parsed, depth);
    return { text: redacted.changed ? JSON.stringify(redacted.value) : value, parsed: true };
  } catch {
    return { text: value, parsed: false };
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
    out += redactSerializedCliValues(value.slice(start, end), depth).text;
    cursor = end;
  }
  return out + value.slice(cursor);
}

function redactValueAtDepth(value: string, depth: number): string {
  const serialized = redactSerializedCliValues(value, depth);
  // Parsed JSON has already been traversed string-by-string. Applying the
  // contextual regexes to its escaped serialization can consume an inner
  // closing quote and silently damage the nested document.
  if (serialized.parsed) return serialized.text;
  return redactContextualDiagnosticText(redactEmbeddedCliArrays(serialized.text, depth));
}

// Use the same credential vocabulary for JSON, argv, and plain assignments.
const SECRET_ASSIGNMENT_NAMES = SECRET_FIELD_NAMES
  // Header-specific stages above preserve auth schemes and consume cookies
  // through end-of-line. Reprocessing them as scalar fields loses that shape.
  .filter(name => !["authorization", "cookie", "set_cookie"].includes(name))
  .map(name => escapeRegExp(name).replaceAll("_", "[_-]?"))
  .join("|");

const SECRET_EMBEDDED_DOUBLE_ASSIGNMENT = new RegExp(
  String.raw`((?:${SECRET_ASSIGNMENT_NAMES})\s*[:=]\s*)((?:\\.|[^"\\\r\n])+?)(")`, "gi",
);
const SECRET_EMBEDDED_ASSIGNMENT = new RegExp(
  String.raw`((?:${SECRET_ASSIGNMENT_NAMES})\s*[:=]\s*)([^"'\\\r\n]+?)(\\?["'])`, "gi",
);
const SECRET_SCALAR_ASSIGNMENT = new RegExp(
  String.raw`(["']?(?:${SECRET_ASSIGNMENT_NAMES})["']?\s*[:=]\s*)("[^"\r\n]*"|'[^'\r\n]*'|[^\s,;&\\"'\]]+)`, "gi",
);

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
      // Start once per scheme-shaped run, not once per character in a long
      // log line. Preserve even malformed numeric prefixes before the scheme.
      /(?<![a-z0-9+.-])([0-9+.-]*[a-z][a-z0-9+.-]*:\/\/[^/\s:@]+:)[^@\s/?#]+@/gi,
      "$1<redacted>@",
    )
    .replace(
      SECRET_EMBEDDED_DOUBLE_ASSIGNMENT,
      redactEmbeddedAssignment,
    )
    .replace(
      SECRET_EMBEDDED_ASSIGNMENT,
      redactEmbeddedAssignment,
    )
    .replace(
      SECRET_SCALAR_ASSIGNMENT,
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

  for (const { prefix, minLength, alphanumericBody, matcher } of DIAGNOSTIC_SECRET_MATCHERS) {
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

export interface DiagnosticsHealth {
  persistence: "ready" | "saved" | "memory-only";
  persistenceError: string | null;
  restorationError: string | null;
  suppressedRuntimeEvents: number;
}

export interface DiagnosticsStore {
  subscribe: (run: (entries: readonly DiagnosticEntry[]) => void) => () => void;
  readonly health: Readable<DiagnosticsHealth>;
  /** Retry explicitly after unavailable/quota-limited storage; never loop per error. */
  retryPersistence(): void;
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
  // The UI timer probe stamps a fresh count and max every 30s. Those are
  // samples of one condition (`max_delay_ms=629` is not a duration token —
  // the unit sits in the key), so leaving them unmasked turned overnight
  // WKWebView coalescing into dozens of "distinct" warnings.
  [/\d+ delayed UI timer sample\(s\)/g, "⟨n⟩ delayed UI timer sample(s)"],
  [/\d+ long observation gap\(s\)/g, "⟨n⟩ long observation gap(s)"],
  [/max_delay_ms=\d+(?:\.\d+)?/g, "max_delay_ms=⟨n⟩"],
  [/max_gap_ms=\d+(?:\.\d+)?/g, "max_gap_ms=⟨n⟩"],
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

function sameOccurrence(a: DiagnosticEntry, b: DiagnosticEntry): boolean {
  return a.severity === b.severity
    && a.source === b.source
    && a.version === b.version
    && a.buildId === b.buildId
    && diagnosticFingerprint(a.message) === diagnosticFingerprint(b.message);
}

/**
 * Fold a consecutive run of the same observation, newest-first.
 *
 * Restore is the only caller: live recording already folds the head, so a
 * blob written by this build should be a no-op. Older blobs that stamped a
 * fresh `max_delay_ms` on every 30s sample are the case this exists for.
 */
function coalesceConsecutive(entries: DiagnosticEntry[]): DiagnosticEntry[] {
  if (entries.length < 2) return entries;
  const out: DiagnosticEntry[] = [entries[0]];
  let changed = false;
  for (let i = 1; i < entries.length; i += 1) {
    const newer = out[out.length - 1];
    const older = entries[i];
    if (!sameOccurrence(newer, older)) {
      out.push(older);
      continue;
    }
    const varied = newer.varied === true || older.varied === true || newer.message !== older.message;
    out[out.length - 1] = {
      ...newer,
      count: Math.min(Number.MAX_SAFE_INTEGER, newer.count + older.count),
      ...(varied ? { varied: true as const } : {}),
    };
    changed = true;
  }
  return changed ? out : entries;
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
export function isHostRuntimeNoise(message: string, development = import.meta.env.DEV): boolean {
  if (!development) return false;
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
  // A module-load or observer error also occurs in real product failures.
  // Only identifiable development reload chatter is safe to suppress.
  return false;
}

/**
 * Exact browser delivery-limit messages. They do not prove layout convergence.
 * Only delivery notices without an application exception may be classified this
 * way; TypeError/DOMException and explicit console/store errors remain errors.
 */
export function isBrowserObserverNoise(message: string): boolean {
  const text = message.trim();
  return /^ResizeObserver loop (?:limit exceeded|completed with undelivered notifications)\.?$/.test(text);
}

/** Document URLs Chromium/WebKit/Tauri stamps on ResizeObserver delivery ErrorEvents. */
function isDocumentUrlFilename(filename: string): boolean {
  return /^(?:https?:|file:|about:|tauri:|asset:|ipc:)/i.test(filename.trim());
}

/**
 * True when the event carries a real script frame. Chromium fills `filename`
 * with the document URL and leaves lineno/colno at 0 for ResizeObserver
 * delivery notices — that stamp is not a script location.
 */
function isScriptLocatedDiagnosticEvent(event: DiagnosticEventLike): boolean {
  const hasLine = typeof event.lineno === "number" && event.lineno !== 0;
  const hasCol = typeof event.colno === "number" && event.colno !== 0;
  if (hasLine || hasCol) return true;
  if (typeof event.filename === "string" && event.filename.trim() !== "") {
    if (isDocumentUrlFilename(event.filename)) return false;
    return true;
  }
  return false;
}

function exceptionName(detail: unknown): string | null {
  if (detail == null || typeof detail !== "object") return null;
  try {
    const name = (detail as { name?: unknown }).name;
    return typeof name === "string" && name ? name : null;
  } catch {
    return null;
  }
}

/** TypeError / DOMException / etc. — never the Resize Observer delivery ErrorEvent. */
function isNamedApplicationException(detail: unknown): boolean {
  const name = exceptionName(detail);
  return name !== null && name !== "Error";
}

function observerNoiseFromValue(value: unknown): string | null {
  if (typeof value === "string") {
    return isBrowserObserverNoise(value) ? value.trim() : null;
  }
  if (isNamedApplicationException(value)) return null;
  if (value && typeof value === "object") {
    try {
      const message = (value as { message?: unknown }).message;
      if (typeof message === "string" && isBrowserObserverNoise(message)) return message.trim();
    } catch {
      return null;
    }
  }
  return null;
}

/**
 * Chromium/WKWebView fires an ErrorEvent whose `message` is the spec text.
 * Chromium stamps the document URL into `filename` with lineno/colno 0 and
 * often leaves `error` null (WKWebView may attach a same-text `Error`, or wrap
 * `message` as `Uncaught Error: …`). That is not an application exception.
 * A TypeError, a prefixed message with no such Error, or a real script frame
 * (module path, or non-zero line/column) still is.
 */
function browserObserverDeliveryText(event: DiagnosticEventLike): string | null {
  if (isScriptLocatedDiagnosticEvent(event)) return null;
  if (event.error == null) {
    return typeof event.message === "string" && isBrowserObserverNoise(event.message)
      ? event.message.trim()
      : null;
  }
  return observerNoiseFromValue(event.error);
}

function scriptLocationLabel(event: DiagnosticEventLike): string {
  const file =
    typeof event.filename === "string" && event.filename.trim() ? event.filename.trim() : "?";
  const line = typeof event.lineno === "number" ? event.lineno : 0;
  const column = typeof event.colno === "number" ? event.colno : 0;
  return `${file}:${line}:${column}`;
}

function uncaughtEventDetail(event: DiagnosticEventLike): unknown {
  if (isNamedApplicationException(event.error)) {
    const name = exceptionName(event.error);
    try {
      const message = (event.error as { message?: unknown }).message;
      if (name && typeof message === "string" && message.trim()) {
        return `${name}: ${message.trim()}`;
      }
    } catch {
      return event.error ?? event.message;
    }
  }
  const base = event.error ?? event.message;
  if (
    isScriptLocatedDiagnosticEvent(event) &&
    isBrowserObserverNoise(formatDiagnosticFailure(base))
  ) {
    return `${formatDiagnosticFailure(base)} (${scriptLocationLabel(event)})`;
  }
  return base;
}

function namedObserverMessage(detail: unknown, formatted: string): string {
  if (!isNamedApplicationException(detail) || !isBrowserObserverNoise(formatted)) return formatted;
  const name = exceptionName(detail);
  return name ? clampMessage(`${name}: ${formatted}`) : formatted;
}

function isSuppressedObserverRecord(
  severity: DiagnosticSeverity,
  source: string,
  message: string,
  detail: unknown,
): boolean {
  if (!isBrowserObserverNoise(message) || isNamedApplicationException(detail)) return false;
  if (source === "browser-resize-observer" && severity === "warning") return true;
  // Chromium dump: installer used to record this as uncaught-error ERROR.
  return source === "uncaught-error";
}

function isRestoredObserverDump(entry: DiagnosticEntry): boolean {
  if (!isBrowserObserverNoise(entry.message)) return false;
  return entry.source === "browser-resize-observer" || entry.source === "uncaught-error";
}

function sanitizeEntry(raw: unknown): DiagnosticEntry | null {
  if (!raw || typeof raw !== "object") return null;
  const record = raw as Record<string, unknown>;
  if (
    typeof record.id !== "number" ||
    !Number.isSafeInteger(record.id) || record.id < 1 ||
    typeof record.at !== "number" ||
    !Number.isFinite(record.at) || !Number.isFinite(new Date(record.at).getTime()) ||
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
    ...(typeof record.buildId === "string" && record.buildId.trim()
      ? { buildId: redactDiagnosticText(record.buildId).slice(0, 96) } : {}),
    // Strictly `true`; a truthy string from a hostile blob must not become a
    // disclosure the app never made.
    ...(record.varied === true ? { varied: true } : {}),
  };
}

function loadPersisted(storage: StorageLike | null, development: boolean): {
  entries: DiagnosticEntry[];
  nextId: number;
  rewritten: boolean;
  restorationError: string | null;
} {
  const empty = { entries: [], nextId: 0, rewritten: false, restorationError: null };
  if (!storage) return empty;
  let parsed: unknown = null;
  try {
    const raw = storage.getItem(DIAGNOSTIC_STORAGE_KEY);
    if (raw && raw.length > 16 * 1024 * 1024) {
      return { ...empty, restorationError: "Saved diagnostics exceeded the 16 MiB read limit." };
    }
    parsed = raw ? (JSON.parse(raw) as unknown) : null;
  } catch (error) {
    return { ...empty, restorationError: clampMessage(formatDiagnosticFailure(error)) };
  }
  if (parsed === null) return empty;
  if (!Array.isArray(parsed)) return { ...empty, restorationError: "Saved diagnostics are not a log array." };
  const sanitized = parsed
    .map(sanitizeEntry)
    .filter((entry): entry is DiagnosticEntry => entry !== null);
  let entries = sanitized
    .filter((entry) => !isHostRuntimeNoise(entry.message, development))
    .filter((entry) => !isRestoredObserverDump(entry))
    // Newest first regardless of how the blob was written.
    .sort((a, b) => b.id - a.id)
    .slice(0, MAX_DIAGNOSTIC_ENTRIES);
  // Persisted IDs are untrusted. Keep every valid event, repairing duplicate
  // or exhausted IDs before they can crash Diagnostics' keyed list.
  const repairIds = new Set(entries.map(entry => entry.id)).size !== entries.length ||
    (entries[0]?.id ?? 0) >= Number.MAX_SAFE_INTEGER - MAX_DIAGNOSTIC_ENTRIES;
  if (repairIds) entries = entries.map((entry, index) => ({ ...entry, id: entries.length - index }));
  // Live coalescing only folds the head, and only after fingerprint masking
  // exists. A blob written before that (or by an older build) can still hold
  // a consecutive run of the same observation — collapse it here so restore
  // does not reopen as dozens of "distinct" warnings.
  const coalesced = coalesceConsecutive(entries);
  const coalescedChanged = coalesced.length !== entries.length;
  entries = coalesced;
  const nextId = entries[0]?.id ?? 0;
  const sanitizedChanged = sanitized.some(
    (entry, index) => JSON.stringify(entry) !== JSON.stringify(parsed[index]),
  );
  const rewritten =
    repairIds || sanitized.length !== parsed.length || entries.length !== sanitized.length || sanitizedChanged
    || coalescedChanged;
  const invalid = parsed.length - sanitized.length;
  return { entries, nextId, rewritten, restorationError: invalid > 0 ? `${invalid} invalid saved diagnostic entries could not be restored.` : null };
}

export function createDiagnostics(deps: { storage?: StorageLike | null; now?: () => number; development?: boolean; buildId?: string } = {}): DiagnosticsStore {
  const buildId = redactDiagnosticText(deps.buildId ?? APP_BUILD_ID).slice(0, 96);
  // `undefined` means "pick the browser storage"; explicit null disables it.
  const storage = deps.storage !== undefined ? deps.storage : browserStorage();
  const now = deps.now ?? (() => Date.now());
  const development = deps.development ?? import.meta.env.DEV;
  const restored = loadPersisted(storage, development);

  let entries: DiagnosticEntry[] = restored.entries;
  let nextId = restored.nextId;
  // Only the newest entry can absorb a repeat, so one cached key is enough —
  // and it keeps an error storm from re-normalizing the head on every event.
  let headKey: string | null =
    entries.length > 0 ? diagnosticFingerprint(entries[0].message) : null;
  const store = writable<readonly DiagnosticEntry[]>(entries);
  const health = writable<DiagnosticsHealth>({
    persistence: storage && !restored.restorationError ? "ready" : "memory-only",
    persistenceError: storage ? null : "Persistent storage is unavailable.",
    restorationError: restored.restorationError,
    suppressedRuntimeEvents: 0,
  });
  let persistenceBlocked = false;

  function persist(force = false) {
    if (!storage || (persistenceBlocked && !force)) return;
    try {
      storage.setItem(DIAGNOSTIC_STORAGE_KEY, JSON.stringify(entries));
      persistenceBlocked = false;
      health.update(state => ({ ...state, persistence: "saved", persistenceError: null }));
    } catch (error) {
      persistenceBlocked = true;
      health.update(state => ({ ...state, persistence: "memory-only", persistenceError: clampMessage(formatDiagnosticFailure(error)) }));
    }
  }

  if (restored.rewritten) persist();

  function record(severity: DiagnosticSeverity, source: string, detail: unknown) {
    const safeSource = clampMessage(redactDiagnosticText(source));
    const message = namedObserverMessage(detail, clampMessage(formatDiagnosticFailure(detail)));
    if (
      isHostRuntimeNoise(message, development) ||
      isSuppressedObserverRecord(severity, safeSource, message, detail)
    ) {
      health.update(state => ({ ...state, suppressedRuntimeEvents: Math.min(Number.MAX_SAFE_INTEGER, state.suppressedRuntimeEvents + 1) }));
      return;
    }
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
      newest.version === APP_VERSION && newest.buildId === buildId
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
          count: Math.min(Number.MAX_SAFE_INTEGER, newest.count + 1),
          at: now(),
          ...(varied ? { varied: true as const } : {}),
        },
        ...entries.slice(1),
      ];
      store.set(entries);
      persist();
      return;
    }
    if (nextId >= Number.MAX_SAFE_INTEGER - 1) {
      entries = entries.map((entry, index) => ({ ...entry, id: entries.length - index }));
      nextId = entries.length;
    }
    nextId += 1;
    entries = [
      { id: nextId, at: now(), severity, source: safeSource, message, count: 1, version: APP_VERSION, buildId },
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
    health: { subscribe: health.subscribe },
    retryPersistence: () => persist(true),
    error: (source, detail) => record("error", source, detail),
    warn: (source, detail) => record("warning", source, detail),
    clear: () => {
      entries = [];
      nextId = 0;
      headKey = null;
      store.set(entries);
      health.update(state => ({ ...state, restorationError: null, suppressedRuntimeEvents: 0 }));
      if (storage) {
        try {
          storage.removeItem(DIAGNOSTIC_STORAGE_KEY);
          persistenceBlocked = false;
          health.update(state => ({ ...state, persistence: "saved", persistenceError: null }));
        } catch (error) {
          persistenceBlocked = true;
          health.update(state => ({ ...state, persistence: "memory-only", persistenceError: clampMessage(formatDiagnosticFailure(error)) }));
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
  health?: DiagnosticsHealth,
): string {
  const storageNotes = health ? [
    diagnosticPersistenceLabel(health),
    ...(health.persistenceError ? [`Saving unavailable: ${formatDiagnosticFailure(health.persistenceError)}`] : []),
    ...(health.restorationError ? [`Saved history incomplete: ${formatDiagnosticFailure(health.restorationError)}`] : []),
    ...(health.suppressedRuntimeEvents ? [`Runtime messages suppressed this session: ${health.suppressedRuntimeEvents}`] : []),
  ] : [];
  const occurrences = (severity: DiagnosticSeverity) =>
    entries.reduce((total, entry) => (entry.severity === severity ? total + entry.count : total), 0);
  if (entries.length === 0) {
    return [`GitPulse diagnostics — nothing recorded as of ${generatedAt.toISOString()}`, ...storageNotes].join("\n");
  }
  const blocks = entries.map((entry) => {
    const repeats = entry.count > 1 ? ` x${entry.count}` : "";
    // `x3` on its own would read as three verbatim repeats.
    const spread =
      entry.count > 1 && entry.varied ? " [occurrences differed; showing the most recent]" : "";
    const header = `[${new Date(entry.at).toISOString()}] ${entry.severity.toUpperCase()}${repeats} (${redactDiagnosticText(entry.source)})${spread}${staleBuildNote(entry.version, runningVersion)} [build ${redactDiagnosticText(entry.buildId ?? "unknown")}]`;
    const body = redactDiagnosticText(entry.message)
      .split("\n")
      .map((line) => `  ${line}`)
      .join("\n");
    return `${header}\n${body}`;
  });
  return [
    `GitPulse diagnostics — ${occurrences("error")} error(s), ${occurrences("warning")} warning(s), ${entries.length} distinct`,
    `Generated: ${generatedAt.toISOString()} by GitPulse ${runningVersion}`,
    `Running build: ${APP_BUILD_ID}`,
    ...storageNotes,
    "",
    ...blocks,
  ].join("\n");
}

export function diagnosticPersistenceLabel(health: DiagnosticsHealth): string {
  switch (health.persistence) {
    case "saved": return "App diagnostics saved locally";
    case "ready": return "App diagnostics storage ready";
    case "memory-only": return "App diagnostics are memory only — may be lost on restart";
  }
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
  filename?: unknown;
  lineno?: unknown;
  colno?: unknown;
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
    note("error", "unhandled-rejection", [detail]);
    originalError(`[gitpulse] unhandled promise rejection: ${detail}`);
  };
  const onUncaughtError = (event: DiagnosticEventLike) => {
    // Delivery-limit ErrorEvents are not application exceptions. Chromium often
    // attaches a same-text `Error` (and wraps `message` as `Uncaught Error: …`).
    // A TypeError, a prefixed-only message, or a script location still is.
    // https://drafts.csswg.org/resize-observer/#deliver-resize-loop-error
    const observerText = browserObserverDeliveryText(event);
    if (observerText !== null) {
      note("warning", "browser-resize-observer", [observerText]);
      return;
    }
    const detail = uncaughtEventDetail(event);
    note("error", "uncaught-error", [detail]);
    originalError(`[gitpulse] uncaught error: ${formatDiagnosticFailure(detail)}`);
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
