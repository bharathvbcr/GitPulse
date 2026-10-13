/**
 * The one "is this a plain object" guard for untrusted wire data: not null,
 * not an array. Parsers that validate backend and storage payloads narrow with
 * this before reading fields, so a `null` or a list never reaches a property
 * access. Class instances and `Date`s pass too: it is a shape check for parsed
 * JSON, not a prototype check.
 */
export function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

/** `value` as a record, or null when it is not one. */
export function asRecord(value: unknown): Record<string, unknown> | null {
  return isRecord(value) ? value : null;
}
