import { readFileSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { codeOnly, svelteFiles, tsSourceFiles } from "./sourceScan";

/**
 * Adversarial review of repoStoreOutcome.contract.test.ts (ft-1629347b).
 *
 * `mutationNames` and `discardedMutations` below are verbatim copies of the
 * contract test's private helpers (they are not exported; importing the test
 * file would re-run it). If the contract is hardened these copies should be
 * replaced by imports from a shared module.
 */

const SRC = join(dirname(fileURLToPath(import.meta.url)), "..", "..");

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

function discardedMutations(code: string, names: ReadonlySet<string>): number[] {
  const lines: number[] = [];
  for (const call of code.matchAll(/\bawait\s+repoStore\.(\w+)\s*\(/g)) {
    if (!names.has(call[1])) continue;
    const before = code.slice(0, call.index).trimEnd();
    const startsStatement = before === "" || ";{})".includes(before.slice(-1)) || /\b(?:else|do)$/.test(before);
    if (!startsStatement) continue;
    let at = (call.index ?? 0) + call[0].length;
    for (let depth = 1; at < code.length && depth > 0; at += 1) {
      if (code[at] === "(") depth += 1;
      else if (code[at] === ")") depth -= 1;
    }
    const next = /^[ \t]*(.?)/.exec(code.slice(at))?.[1] ?? "";
    if (next === "" || next === ";" || next === "}") {
      lines.push(code.slice(0, call.index).split("\n").length);
    }
  }
  return lines;
}

const NAMES = new Set(["createBranch", "deleteBranch", "discardChanges", "checkoutBranch"]);
const scan = (script: string) => discardedMutations(codeOnly(script), NAMES);

describe("attack 3: shapes that discard a mutation outcome and must be flagged", () => {
  it("BREAK: .then() chain without await", () => {
    expect(scan("repoStore.createBranch(name).then(() => toastStore.success('Created'));")).not.toEqual([]);
  });

  it("BREAK: destructured method", () => {
    expect(scan("const { deleteBranch } = repoStore;\nawait deleteBranch(name);")).not.toEqual([]);
  });

  it("BREAK: aliased store", () => {
    expect(scan("const s = repoStore;\nawait s.createBranch(name);")).not.toEqual([]);
  });

  it("BREAK: optional call", () => {
    expect(scan("await repoStore.createBranch?.(name);")).not.toEqual([]);
  });

  it("BREAK: computed member access", () => {
    expect(scan("await repoStore['createBranch'](name);")).not.toEqual([]);
  });

  it("BREAK: parenthesised await", () => {
    expect(scan("(await repoStore.createBranch(name));")).not.toEqual([]);
  });

  it("BREAK: member on its own line (prettier chain style)", () => {
    expect(scan("await repoStore\n  .createBranch(name);")).not.toEqual([]);
  });

  it("BREAK: comma expression drops the outcome", () => {
    expect(scan("await repoStore.createBranch(name), toastStore.success('Created');")).not.toEqual([]);
  });

  it("BREAK: Promise.all over mutations drops every outcome", () => {
    expect(scan("await Promise.all(names.map((n) => repoStore.deleteBranch(n)));")).not.toEqual([]);
  });

  it("BREAK: a floating, un-awaited, un-voided call (a lie-prone success toast can follow it)", () => {
    expect(scan("closeMenu(); repoStore.checkoutBranch(row.id);\ntoastStore.success('Checked out');")).not.toEqual([]);
  });

  it("BREAK: a new mutation written as a method shorthand is not derived, and is attributed to the member before it", () => {
    const store = [
      "  const store = {",
      "    deleteTag: async (tagName: string) =>",
      "      runMutating('tag-delete', tagName, (path) => invokeFn('cmd_delete_tag', { path })),",
      "    prune(remote: string) {",
      "      return runMutating('prune', remote, (path) => invokeFn('cmd_prune', { path }));",
      "    },",
      "  };",
      "  return store;",
    ].join("\n");
    expect(mutationNames(store).has("prune")).toBe(true);
  });

  it("BREAK: a new mutation that reaches the wrapper through a helper is not derived", () => {
    const store = [
      "  const store = {",
      "    prune: async (remote: string) => mutate('prune', remote),",
      "  };",
      "  return store;",
    ].join("\n");
    expect(mutationNames(store).has("prune")).toBe(true);
  });
});

describe("attack 3: control and derivation", () => {
  it("HOLDS (control): the copied scanner still flags a bare awaited statement", () => {
    expect(scan("await repoStore.createBranch(name);")).toEqual([1]);
  });

  it("BREAK: the derived set contains a non-mutation, because the last member's body runs on into runMutating's own definition", () => {
    const names = mutationNames(readFileSync(join(SRC, "lib", "stores", "repoStore.ts"), "utf8"));
    expect(names.has("setAmending")).toBe(false);
  });
});

describe("attack 3: the repository scan is blind where the real discards are", () => {
  it("BREAK: no real component calls a mutation as a bare floating statement", () => {
    const names = mutationNames(readFileSync(join(SRC, "lib", "stores", "repoStore.ts"), "utf8"));
    names.delete("setAmending");
    const call = new RegExp(`(?:^|[;{])\\s*repoStore\\.(${[...names].join("|")})\\s*\\(`, "gm");
    const files = [...svelteFiles(SRC), ...tsSourceFiles(join(SRC, "lib"))];
    const offenders = files.flatMap((file) => {
      const text = readFileSync(file, "utf8");
      return [...text.matchAll(call)].map((m) => `${relative(SRC, file)}:${text.slice(0, m.index).split("\n").length}: ${m[0].trim()}`);
    });
    expect(offenders).toEqual([]);
  });
});
