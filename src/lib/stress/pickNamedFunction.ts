/**
 * Dynamic dispatch for a caller-supplied name, fail-closed.
 *
 * `table[name]()` with a URL/query string as `name` is an unvalidated
 * dynamic method call: `constructor`, `__proto__`, or any inherited
 * `Function` can run. The name must be on an explicit allowlist *and* an
 * own function on the table before it is invoked.
 */

export function pickNamedFunction<T extends Record<string, unknown>>(
  table: T,
  name: string,
  allowed: readonly string[],
): T[keyof T] {
  if (!allowed.includes(name)) {
    throw new Error(`unsupported harness component: ${name}`);
  }
  if (!Object.prototype.hasOwnProperty.call(table, name)) {
    throw new Error(`unsupported harness component: ${name}`);
  }
  const fn = table[name];
  if (typeof fn !== "function") {
    throw new Error(`unsupported harness component: ${name}`);
  }
  return fn as T[keyof T];
}
