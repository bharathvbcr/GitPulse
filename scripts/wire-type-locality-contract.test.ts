import { readdirSync, readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

/**
 * Wire-shaped types belong in modules, not inside components.
 *
 * A payload type declared in a `.svelte` file cannot be reached by
 * `check:types`, which needs a module to point at. Worse, the same shape then
 * tends to get declared more than once — and structural copies cannot fail on
 * drift, because each is assignable to whatever the others say. Every instance
 * found so far had gone stale:
 *
 *   - TerminalRunResult, declared three times (twice named, once inline).
 *   - GitHubContext, in two components with disjoint halves of one struct.
 *   - ConflictChunk, in the component, in its test, and in Rust — both
 *     TypeScript copies missing seven CRLF fields Rust had always sent.
 *   - CommitDetailsPayload, four fields behind the canonical CommitDetails.
 *   - FileBlobPayload, a byte-identical copy of FileBlob.
 *
 * The heuristic is snake_case property names. The frontend writes camelCase,
 * so two or more snake_case fields in one declaration means it is mirroring a
 * serde struct.
 */
const COMPONENT_ROOT = fileURLToPath(new URL("../src/", import.meta.url));

/**
 * Components allowed to declare a wire-shaped type locally, with why.
 * Empty on purpose: every case found so far was a bug, so a new entry should
 * take an argument, not a shrug.
 */
const ALLOWED = new Map<string, string>();

const DECLARATION =
  /(?:^|\n)\s*(?:export\s+)?(?:interface|type)\s+(\w+)\s*(?:extends\s+[\w,\s]+)?[={]/g;

function svelteFiles(dir: string): string[] {
  const out: string[] = [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) out.push(...svelteFiles(full));
    else if (entry.isFile() && entry.name.endsWith(".svelte")) out.push(full);
  }
  return out;
}

/** Balanced `{...}` body starting at or after `from`, or "" if unbalanced. */
function body(source: string, from: number): string {
  const open = source.indexOf("{", from);
  if (open === -1) return "";
  let depth = 0;
  for (let i = open; i < source.length; i += 1) {
    if (source[i] === "{") depth += 1;
    else if (source[i] === "}") {
      depth -= 1;
      if (depth === 0) return source.slice(open, i);
    }
  }
  return "";
}

function wireShapedTypesIn(source: string): string[] {
  const found: string[] = [];
  for (const match of source.matchAll(DECLARATION)) {
    const fields = new Set(
      [...body(source, match.index ?? 0).matchAll(/^\s*(\w+_\w+)\s*\??\s*:/gm)].map((f) => f[1]),
    );
    if (fields.size >= 2) found.push(match[1]);
  }
  return found;
}

/**
 * The anonymous form of the same bug: an inline object literal as the type
 * argument to invoke() or listen(), which mirrors a payload without naming it.
 * Every one found was a partial mirror of a type that already had a canonical
 * interface — `invoke<{ text: string }>` for the 15-field AiGeneration,
 * `invoke<{ rows: ... }>` for CommitGraphPayload, and three event payloads.
 */
const INLINE_PAYLOAD = /\b(?:invoke|listen)\s*<\s*\{/g;

/** Every .ts and .svelte file that is not a test. */
function frontendFiles(dir: string): string[] {
  const out: string[] = [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) out.push(...frontendFiles(full));
    else if (
      entry.isFile() &&
      (entry.name.endsWith(".svelte") || entry.name.endsWith(".ts")) &&
      !/\.(test|spec)\.ts$/.test(entry.name)
    ) {
      out.push(full);
    }
  }
  return out;
}

describe("IPC and event payloads are named, not inlined", () => {
  it("has no invoke or listen call typed with an inline object literal", () => {
    const offenders: string[] = [];
    for (const file of frontendFiles(COMPONENT_ROOT)) {
      const source = readFileSync(file, "utf8");
      for (const match of source.matchAll(INLINE_PAYLOAD)) {
        const line = source.slice(0, match.index).split("\n").length;
        offenders.push(`${path.relative(COMPONENT_ROOT, file)}:${line}`);
      }
    }
    expect(
      offenders,
      "name the payload in a types module so check:types can compare it",
    ).toEqual([]);
  });

  it("detects an inline payload when there is one", () => {
    // The heuristic guards itself, as above.
    expect('await invoke<{ a: string }>("cmd_x")'.match(INLINE_PAYLOAD)).toHaveLength(1);
    expect('await listen<{ a: string }>("evt")'.match(INLINE_PAYLOAD)).toHaveLength(1);
    expect('await invoke<NamedPayload>("cmd_x")'.match(INLINE_PAYLOAD)).toBeNull();
  });
});

describe("wire types live in modules, not components", () => {
  const components = svelteFiles(COMPONENT_ROOT);

  it("finds components to scan at all", () => {
    expect(components.length).toBeGreaterThan(30);
  });

  it("detects a wire-shaped declaration when there is one", () => {
    // Guards the heuristic itself: without this, a broken regex would report
    // a clean tree forever.
    expect(
      wireShapedTypesIn("<script>\n  interface P {\n    a_b: string;\n    c_d: number;\n  }\n</script>"),
    ).toEqual(["P"]);
    // camelCase is the frontend's own vocabulary and must not trip it.
    expect(
      wireShapedTypesIn("<script>\n  interface Q {\n    aB: string;\n    cD: number;\n  }\n</script>"),
    ).toEqual([]);
  });

  it("has no component declaring a serde payload shape", () => {
    const offenders: string[] = [];
    for (const file of components) {
      const rel = path.relative(COMPONENT_ROOT, file);
      if (ALLOWED.has(rel)) continue;
      for (const name of wireShapedTypesIn(readFileSync(file, "utf8"))) {
        offenders.push(`${rel}: ${name}`);
      }
    }
    expect(
      offenders,
      "move these into a types module and add them to check:types' CONTRACTS",
    ).toEqual([]);
  });

  it("keeps the exemption list from outliving its components", () => {
    const known = new Set(components.map((f) => path.relative(COMPONENT_ROOT, f)));
    expect([...ALLOWED.keys()].filter((f) => !known.has(f))).toEqual([]);
  });
});
