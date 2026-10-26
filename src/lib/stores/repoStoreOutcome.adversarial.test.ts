import { readFileSync } from "node:fs";
import { dirname, join, relative, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { codeOnly, svelteFiles, tsSourceFiles } from "./sourceScan";
import { discardedMutations, mutationNames } from "./mutationScan";

/**
 * Adversarial review of repoStoreOutcome.contract.test.ts (ft-1629347b).
 *
 * `mutationNames` and `discardedMutations` are the contract test's own
 * scanner, imported from mutationScan.ts (they were verbatim copies of its
 * old private helpers until the scanner was rebuilt on the compiler API).
 */

const SRC = join(dirname(fileURLToPath(import.meta.url)), "..", "..");

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

  // Parses all of repoStore.ts with the TypeScript compiler: vitest's 5 s
  // default timed it out in a full ci:local at load ~90 though it passes alone.
  it("BREAK: the derived set contains a non-mutation, because the last member's body runs on into runMutating's own definition", () => {
    const names = mutationNames(readFileSync(join(SRC, "lib", "stores", "repoStore.ts"), "utf8"));
    expect(names.has("setAmending")).toBe(false);
  }, 60_000);
});

describe("attack 3: the repository scan is blind where the real discards are", () => {
  it("BREAK: no real component calls a mutation as a bare floating statement", () => {
    const names = mutationNames(readFileSync(join(SRC, "lib", "stores", "repoStore.ts"), "utf8"));
    names.delete("setAmending");
    const call = new RegExp(`(?:^|[;{])\\s*repoStore\\.(${[...names].join("|")})\\s*\\(`, "gm");
    const files = [...svelteFiles(SRC), ...tsSourceFiles(join(SRC, "lib"))];
    const offenders = files.flatMap((file) => {
      const text = readFileSync(file, "utf8");
      return [...text.matchAll(call)].map((m) => `${relative(SRC, file).split(sep).join("/")}:${text.slice(0, m.index).split("\n").length}: ${m[0].trim()}`);
    });
    expect(offenders).toEqual([]);
  });
});
