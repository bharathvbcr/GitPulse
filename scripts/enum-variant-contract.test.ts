import { mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { parse } from "svelte/compiler";
import {
  createSourceFile, isIdentifier, isLiteralTypeNode, isParenthesizedTypeNode,
  isPropertySignature, isStringLiteral, isTypeAliasDeclaration, isTypeLiteralNode,
  isTypeReferenceNode, isUnionTypeNode, ScriptTarget, type TypeNode,
} from "typescript";
import { describe, expect, it } from "vitest";

/**
 * Serde enums cross the wire as strings, and `check:types` skips them by
 * design — it compares struct fields. So nothing checked that a Rust variant
 * and its TypeScript literal still spell the same thing.
 *
 * This is the quietest drift there is. Rename a variant and TypeScript keeps
 * compiling: the union still lists a valid string, the comparison just stops
 * matching anything, and a branch silently becomes dead. No type error, no
 * test failure, no crash.
 */
const RUST_ROOT = fileURLToPath(new URL("../src-tauri/src/", import.meta.url));
const TS_ROOT = fileURLToPath(new URL("../src/", import.meta.url));

/**
 * Rust enums with no same-named TypeScript union, and why.
 * An entry means "the frontend does not branch on this", not "unchecked is
 * fine" — if a UI starts comparing these strings, it needs the named type.
 */
const NO_TS_MIRROR = new Map<string, string>([
  ["CoverageFormat", "the frontend renders `format` as an opaque label and never branches on it"],
  ["ManviActionKind", "activity labels are produced by the frontend, not parsed from the backend"],
  [
    "PermissionDecision",
    "a `gitpulse-hook` wire type: it is serialized to the agent host's hook protocol on stdout, never to this app's frontend, and its spelling is fixed by that protocol rather than by us",
  ],
  [
    "RebaseActionKind",
    "modelled in the UI as PlannerAction plus a separate wire union, because Reword carries a payload and serializes as an object rather than a bare string",
  ],
  [
    "CapabilityAnswer",
    "process-local cache only; IPC returns ToolStatus.installed / reason, never this enum",
  ],
  [
    "ReleaseAvailability",
    "internal ladder probe; the wizard sees RungStatus.available / block, never this enum",
  ],
  [
    "ConfigPathStaleReason",
    "ToolConfigView.stale.reason is typed as string in TS; the UI does not branch on the variant",
  ],
]);

/** serde's rename_all, for the rules this repo uses. */
function serializedName(variant: string, rule: string | undefined): string {
  if (rule === "lowercase") return variant.toLowerCase();
  if (rule === "snake_case") return variant.replace(/(?<!^)(?=[A-Z])/g, "_").toLowerCase();
  // Used by the hook wire types, whose field and variant names are fixed by the
  // host's hook protocol rather than by this repo.
  if (rule === "camelCase") return variant.charAt(0).toLowerCase() + variant.slice(1);
  if (rule === undefined) return variant;
  throw new Error(`unsupported rename_all on an enum: ${rule}`);
}

function walk(dir: string, exts: string[]): string[] {
  const out: string[] = [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) out.push(...walk(full, exts));
    else if (entry.isFile() && exts.some((e) => entry.name.endsWith(e)) && !/\.(test|spec)\./.test(entry.name)) {
      out.push(full);
    }
  }
  return out;
}

function balanced(source: string, from: number): string {
  const open = source.indexOf("{", from);
  if (open === -1) return "";
  let depth = 0;
  for (let i = open; i < source.length; i += 1) {
    if (source[i] === "{") depth += 1;
    else if (source[i] === "}") {
      depth -= 1;
      if (depth === 0) return source.slice(open + 1, i);
    }
  }
  return "";
}

interface RustEnum {
  /** Unit variants, as they appear on the wire. */
  unit: string[];
  /** Variants carrying data — serialized as `{ Name: ... }`, not a string. */
  withData: string[];
  /**
   * The `#[serde(tag = "...")]` field, for internally tagged enums.
   *
   * These serialize a struct variant FLAT — `{"kind":"add","name":...}` — so
   * the variant name still crosses the wire as a bare string, just in a named
   * field rather than as the whole value. Without knowing that, a tagged enum
   * looks like drift in both directions at once: every variant reads as
   * data-carrying on the Rust side while the TS union plainly lists its tag
   * literal, so the checker reports "TS accepts add; Rust never sends them"
   * about a pair that agrees exactly.
   */
  tag?: string;
}

function rustEnums(): Map<string, RustEnum> {
  const found = new Map<string, RustEnum>();
  for (const file of walk(RUST_ROOT, [".rs"])) {
    const source = readFileSync(file, "utf8");
    for (const match of source.matchAll(/((?:#\[[^\]]*\]\s*)*)pub enum (\w+)\s*\{/g)) {
      if (!match[1].includes("Serialize")) continue;
      const rule = /rename_all\s*=\s*"([^"]+)"/.exec(match[1])?.[1];
      const tag = /\btag\s*=\s*"([^"]+)"/.exec(match[1])?.[1];
      const body = balanced(source, match.index ?? 0);
      const unit: string[] = [];
      const withData: string[] = [];
      for (const variant of body.matchAll(/^\s*([A-Z]\w*)(\s*[({])?/gm)) {
        // An internally tagged enum puts every variant's name in the tag
        // field as a plain string, so all of them are compared as unit
        // variants regardless of the data they carry alongside.
        const carriesData = Boolean(variant[2]) && tag === undefined;
        (carriesData ? withData : unit).push(serializedName(variant[1], rule));
      }
      found.set(match[2], { unit, withData, tag });
    }
  }
  return found;
}

/** Resolve local union aliases without confusing payload strings with variants. */
function typeAliasLiterals(source: string, name: string, tag?: string): Set<string> | null {
  const parsed = createSourceFile("enum-contract.ts", source, ScriptTarget.Latest);
  const aliases = new Map(parsed.statements.filter(isTypeAliasDeclaration)
    .map((declaration) => [declaration.name.text, declaration.type]));
  const root = aliases.get(name);
  if (!root) return null;
  const literals = new Set<string>();
  const active = new Set([name]);
  let visited = 0;
  const visit = (node: TypeNode, tagField: string | undefined, depth: number): void => {
    if (++visited > 10_000) throw new Error(`${name}: union expansion exceeds the 10000-node budget`);
    if (depth > 64) throw new Error(`${name}: union expansion exceeds depth 64`);
    if (isUnionTypeNode(node)) {
      node.types.forEach((child) => visit(child, tagField, depth + 1));
    } else if (isParenthesizedTypeNode(node)) {
      visit(node.type, tagField, depth + 1);
    } else if (isLiteralTypeNode(node) && isStringLiteral(node.literal)) {
      literals.add(node.literal.text);
    } else if (isTypeReferenceNode(node) && isIdentifier(node.typeName) && !node.typeArguments?.length) {
      const reference = node.typeName.text;
      if (active.has(reference)) throw new Error(`${name}: cyclic union alias ${reference}`);
      const target = aliases.get(reference);
      if (!target) throw new Error(`${name}: unresolved local union alias ${reference}`);
      active.add(reference);
      visit(target, tagField, depth + 1);
      active.delete(reference);
    } else if (isTypeLiteralNode(node)) {
      // Untagged objects are payload variants; neither their keys nor their
      // field values represent bare enum strings.
      if (tagField === undefined) return;
      const member = node.members.find((candidate) => isPropertySignature(candidate)
        && (isIdentifier(candidate.name) || isStringLiteral(candidate.name))
        && candidate.name.text === tagField);
      if (!member || !isPropertySignature(member) || !member.type || member.questionToken) {
        throw new Error(`${name}: object variant is missing required tag ${tagField}`);
      }
      visit(member.type, undefined, depth + 1);
    } else {
      throw new Error(`${name}: unsupported enum union member ${node.getText(parsed)}`);
    }
  };
  visit(root, tag, 0);
  return literals;
}

describe("TypeScript enum contract extraction", () => {
  it("follows nested and parenthesized local aliases", () => {
    const source = `type Base = 'Ours' | "Theirs";
      type Whole = (Base | "WorkingTree");
      export type Choice = { Chunks: string[] } | Whole | "StageOnly";`;
    expect([...typeAliasLiterals(source, "Choice") ?? []].sort())
      .toEqual(["Ours", "StageOnly", "Theirs", "WorkingTree"]);
  });

  it("does not mistake quoted payload keys or values for bare enum variants", () => {
    expect([...typeAliasLiterals(`type Choice = "Unit" | { "Chunks": "payload" };`, "Choice") ?? []])
      .toEqual(["Unit"]);
  });

  it("extracts only the declared tag across multiline object variants", () => {
    const source = `type Kind = "add" | 'remove';
      type Edit = { "kind": Kind; text: "not;a;tag" } | { kind: "rename"; name: string };`;
    expect([...typeAliasLiterals(source, "Edit", "kind") ?? []].sort()).toEqual(["add", "remove", "rename"]);
  });

  it("reports unresolved aliases and cycles rather than treating them as empty coverage", () => {
    expect(() => typeAliasLiterals(`type Choice = Missing | "Unit";`, "Choice")).toThrow(/Missing/);
    expect(() => typeAliasLiterals(`type A = B; type B = A;`, "A")).toThrow(/cyclic/i);
  });

  it("bounds expansion depth and distinguishes a missing declaration", () => {
    const source = Array.from({ length: 100 }, (_, i) => `type T${i} = T${i + 1};`).join("\n") + `type T100 = "end";`;
    expect(() => typeAliasLiterals(source, "T0")).toThrow(/depth/i);
    expect(typeAliasLiterals(`type Other = "Unit";`, "Choice")).toBeNull();
  });

  it("bounds repeated alias expansion as well as depth", () => {
    const source = Array.from({ length: 14 }, (_, i) => `type T${i} = T${i + 1} | T${i + 1};`).join("\n") + `type T14 = "end";`;
    expect(() => typeAliasLiterals(source, "T0")).toThrow(/budget/i);
  });
});

/** String literals in the TS union named `name`, if one exists. */
function tsUnion(name: string, tag?: string, root = TS_ROOT): { literals: Set<string>; file: string } | null {
  for (const file of walk(root, [".ts", ".svelte"])) {
    const source = readFileSync(file, "utf8");
    if (!new RegExp(`\\btype\\s+${name}\\s*=`).test(source)) continue;
    let script = source;
    if (file.endsWith(".svelte")) {
      // Only Svelte's top-level module/instance scripts declare component
      // types. Comments, nested markup and similarly named tags do not.
      const component = parse(source, { modern: true, filename: file });
      script = [component.module, component.instance].flatMap((block) => {
        if (!block) return [];
        const content = block.content;
        if (!("start" in content) || typeof content.start !== "number"
          || !("end" in content) || typeof content.end !== "number") {
          throw new Error(`Missing Svelte script source span in ${file}`);
        }
        return [source.slice(content.start, content.end)];
      }).join("\n");
    }
    const literals = typeAliasLiterals(script, name, tag);
    if (literals === null) continue;
    return {
      literals,
      file: path.relative(root, file),
    };
  }
  return null;
}

describe("Svelte enum contract extraction", () => {
  it.each([
    { name: "plain TypeScript", file: "Choice.ts", source: `type Choice = "Unit";`, expected: ["Unit"] },
    { name: "instance script", source: `<script lang="ts">type Choice = "Unit";</script>`, expected: ["Unit"] },
    { name: "module and instance aliases", source: `<script module lang="ts">type Base = "Unit";</script>
      <script lang="ts">type Choice = Base | "Other";</script>`, expected: ["Other", "Unit"] },
    { name: "quoted greater-than attribute", source: `<script lang="ts" data-note="a > b">type Choice = "Unit";</script>`, expected: ["Unit"] },
    { name: "closing-tag whitespace", source: `<script lang="ts">type Choice = "Unit";</script >`, expected: ["Unit"] },
    { name: "commented-out script", source: `<!-- <script lang="ts">type Choice = "Fake";</script> -->`, expected: null },
    { name: "comment after a real script", source: `<script lang="ts">type Choice = "Unit";</script>
      <!-- <script lang="ts">type Choice = "Fake";</script> -->`, expected: ["Unit"] },
    { name: "nested script in markup", source: `<div><script>type Choice = "Fake";</script></div>`, expected: null },
    { name: "uppercase component, not a Svelte script", source: `<SCRIPT>type Choice = "Fake";</SCRIPT>`, expected: null },
    { name: "similarly named element", source: `<script-example>type Choice = "Fake";</script-example>`, expected: null },
    { name: "no script", source: `<p>type Choice = "Fake";</p>`, expected: null },
    { name: "empty component", source: "", expected: null },
  ])("reads $name using component syntax", ({ file = "Choice.svelte", source, expected }) => {
    const root = mkdtempSync(path.join(tmpdir(), "gitpulse-enum-contract-"));
    try {
      writeFileSync(path.join(root, file), source);
      const result = tsUnion("Choice", undefined, root);
      expect(result ? [...result.literals].sort() : null).toEqual(expected);
      if (result) expect(result.file).toBe(file);
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });

  it("reports malformed components instead of silently skipping their enum", () => {
    const root = mkdtempSync(path.join(tmpdir(), "gitpulse-enum-contract-"));
    try {
      writeFileSync(path.join(root, "Choice.svelte"), `<script lang="ts">type Choice = "Unit";`);
      expect(() => tsUnion("Choice", undefined, root)).toThrow(/left open/);
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });
});

describe("serde enum variants match their TypeScript unions", () => {
  const enums = rustEnums();

  it("finds the enums to check at all", () => {
    expect(enums.size).toBeGreaterThan(8);
  });

  it("spells every unit variant the same on both sides", () => {
    const drift: string[] = [];
    for (const [name, { unit, tag }] of enums) {
      if (NO_TS_MIRROR.has(name)) continue;
      const ts = tsUnion(name, tag);
      if (!ts) {
        drift.push(`${name}: no TypeScript union of this name, and no documented reason`);
        continue;
      }
      const missing = unit.filter((v) => !ts.literals.has(v));
      const extra = [...ts.literals].filter((v) => !unit.includes(v));
      if (missing.length > 0) drift.push(`${name}: Rust sends ${missing.join(", ")}; TS does not accept them`);
      if (extra.length > 0) drift.push(`${name}: TS accepts ${extra.join(", ")}; Rust never sends them`);
    }
    expect(drift).toEqual([]);
  });

  it("does not let a data-carrying variant masquerade as a bare string", () => {
    // `Reword(String)` serializes as `{ "Reword": "..." }`, never as
    // `"Reword"`. A TS union listing it as a plain literal would typecheck and
    // then fail to deserialize backend-side.
    const wrong: string[] = [];
    for (const [name, { withData, tag }] of enums) {
      if (NO_TS_MIRROR.has(name)) continue;
      const ts = tsUnion(name, tag);
      if (!ts) continue;
      for (const variant of withData) {
        if (ts.literals.has(variant)) {
          wrong.push(`${name}.${variant} carries data but ${ts.file} lists it as a bare string`);
        }
      }
    }
    expect(wrong).toEqual([]);
  });

  it("keeps the no-mirror list from outliving its enums", () => {
    const stale = [...NO_TS_MIRROR.keys()].filter((name) => !enums.has(name));
    expect(stale, "these enums no longer exist").toEqual([]);
  });

  it("gives every no-mirror entry a real reason", () => {
    for (const [name, reason] of NO_TS_MIRROR) {
      expect(reason.length, `${name} needs a reason`).toBeGreaterThan(20);
    }
  });
});
