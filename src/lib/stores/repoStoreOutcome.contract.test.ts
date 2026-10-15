import { readFileSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { codeOnly, svelteFiles, tsSourceFiles } from "./sourceScan";

/**
 * A repoStore mutation's outcome is never thrown away.
 *
 * `runMutating` never rejects: a refused or failed mutation comes back as
 * `{ ok: false }`. A caller that writes `await repoStore.createBranch(...)` as
 * a bare statement therefore cannot tell a failure from a success, and the
 * success toast after it lies. Every caller keeps the outcome (`const o =
 * await`, `return await`, `if (!(await …).ok)`), or says it does not care with
 * an explicit `void repoStore.x()`, whose failure still reaches the session's
 * error banner.
 *
 * The mutation names are derived from repoStore.ts — the members whose body
 * runs `runMutating(` or `runStageBatch(` — so a new mutation is covered the
 * day it is written.
 */

const SRC = join(dirname(fileURLToPath(import.meta.url)), "..", "..");
const STORE_SOURCE = readFileSync(join(SRC, "lib", "stores", "repoStore.ts"), "utf8");

/** Members of the store literal whose body runs one of the mutation wrappers. */
function mutationNames(storeSource: string): Set<string> {
  const start = storeSource.indexOf("const store = {");
  const end = storeSource.indexOf("return store;", start);
  const literal = storeSource.slice(start, end);
  const heads = [...literal.matchAll(/^ {4}(\w+): /gm)];
  const names = new Set<string>();
  heads.forEach((head, index) => {
    const body = literal.slice(head.index, heads[index + 1]?.index ?? literal.length);
    if (/\b(?:runMutating|runStageBatch)\(/.test(body)) names.add(head[1]);
  });
  return names;
}

/**
 * `.svelte` source with each `<script>` body reduced to its code (comments and
 * strings blanked) and each `<style>` body blanked. Offsets are preserved.
 */
function svelteCode(source: string): string {
  return source
    .replace(/(<script\b[^>]*>)([\s\S]*?)(<\/script>)/g, (_all, open, body, close) => open + codeOnly(body) + close)
    .replace(/(<style\b[^>]*>)([\s\S]*?)(<\/style>)/g, (_all, open, body, close) => open + body.replace(/[^\n]/g, " ") + close);
}

/** Lines of statement-level `await repoStore.<mutation>(…)` whose value is dropped. */
function discardedMutations(code: string, names: ReadonlySet<string>): number[] {
  const lines: number[] = [];
  for (const call of code.matchAll(/\bawait\s+repoStore\.(\w+)\s*\(/g)) {
    if (!names.has(call[1])) continue;
    const before = code.slice(0, call.index).trimEnd();
    const startsStatement =
      before === "" || ";{})".includes(before.slice(-1)) || /\b(?:else|do)$/.test(before);
    if (!startsStatement) continue;
    let at = (call.index ?? 0) + call[0].length;
    for (let depth = 1; at < code.length && depth > 0; at += 1) {
      if (code[at] === "(") depth += 1;
      else if (code[at] === ")") depth -= 1;
    }
    // Anything still chained on the line (`.ok`, `?.`, `&&`, `,`) uses the value.
    const next = /^[ \t]*(.?)/.exec(code.slice(at))?.[1] ?? "";
    if (next === "" || next === ";" || next === "}") {
      lines.push(code.slice(0, call.index).split("\n").length);
    }
  }
  return lines;
}

const FIXTURE_NAMES = new Set(["createBranch", "discardChanges"]);

describe("mutation names are derived from repoStore.ts", () => {
  it("includes every mutation callers use, and none of the reads", () => {
    const names = mutationNames(STORE_SOURCE);
    const floor = [
      "discardChanges",
      "commit",
      "quickCommit",
      "checkoutBranch",
      "createBranch",
      "renameBranch",
      "deleteBranch",
      "resetTo",
      "createTag",
      "deleteTag",
      "stageFile",
      "unstageFile",
      "stageAll",
      "unstageAll",
      "stageSelectivePatch",
      "stashSave",
      "stashPop",
      "push",
      "pull",
      "fetch",
    ];
    expect(floor.filter((name) => !names.has(name))).toEqual([]);
    for (const read of ["refresh", "openRepo", "closeTab", "selectFileDiff", "trustRepo"]) {
      expect(names.has(read)).toBe(false);
    }
  });
});

describe("discarded mutation outcomes", () => {
  const scan = (script: string) => discardedMutations(codeOnly(script), FIXTURE_NAMES);

  it("flags a bare awaited statement, however it is introduced", () => {
    expect(scan("await repoStore.createBranch(name, sha);")).toEqual([1]);
    expect(scan("if (ready) await repoStore.discardChanges(path);")).toEqual([1]);
    expect(scan("if (ready) {\n  await repoStore.createBranch(name);\n}")).toEqual([2]);
    expect(scan("else await repoStore.createBranch(name)")).toEqual([1]);
    expect(scan("const f = async () => {\n  await repoStore.discardChanges(\n    path,\n  );\n};")).toEqual([2]);
  });

  it("flags a bare awaited statement in markup", () => {
    const code = svelteCode("<button onclick={async () => { await repoStore.createBranch(n); }}>x</button>");
    expect(discardedMutations(code, FIXTURE_NAMES)).toEqual([1]);
  });

  it("accepts a kept, returned, tested or explicitly abandoned outcome", () => {
    expect(scan("const o = await repoStore.createBranch(name);")).toEqual([]);
    expect(scan("o = await repoStore.createBranch(name);")).toEqual([]);
    expect(scan("return await repoStore.createBranch(name);")).toEqual([]);
    expect(scan("if (!(await repoStore.createBranch(name)).ok) return;")).toEqual([]);
    expect(scan("const ok = (await repoStore.createBranch(name)).ok;")).toEqual([]);
    expect(scan("const f = () => repoStore.createBranch(name);")).toEqual([]);
    expect(scan("void repoStore.createBranch(name);")).toEqual([]);
    expect(scan("const f = async () => await repoStore.createBranch(name);")).toEqual([]);
  });

  it("ignores reads, comments and strings", () => {
    expect(scan("await repoStore.refresh();")).toEqual([]);
    expect(scan("// await repoStore.createBranch(name);")).toEqual([]);
    expect(scan('const s = "await repoStore.createBranch(name);";')).toEqual([]);
  });
});

describe("components never drop a mutation outcome", () => {
  it("holds for every component and palette module", () => {
    const names = mutationNames(STORE_SOURCE);
    const sources = [
      ...svelteFiles(SRC).map((file) => ({ file, code: svelteCode(readFileSync(file, "utf8")) })),
      ...[join(SRC, "lib", "components"), join(SRC, "lib", "palette")]
        .flatMap(tsSourceFiles)
        .map((file) => ({ file, code: codeOnly(readFileSync(file, "utf8")) })),
    ];
    const offenders = sources.flatMap(({ file, code }) =>
      discardedMutations(code, names).map((line) => `${relative(SRC, file)}:${line}`),
    );
    expect(offenders).toEqual([]);
  });
});
