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
 * Render any rejection value as a human-readable string.
 *
 * IPC rejections arrive as strings, Error instances, or plain objects from
 * the Rust side; `String(err)` turns the object forms into "[object Object]".
 * Order of preference:
 *   1. string input → itself (trimmed; blank becomes "Unknown error")
 *   2. object with a non-blank `.message` string → that message (trimmed)
 *   3. anything else → stable JSON (or "Unknown error" if even that fails)
 */
export function formatError(err: unknown): string {
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
