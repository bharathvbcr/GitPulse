import { readFileSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { discardedMutations, mutationNames, scanSvelte, scanTypeScript, untargetedAfterPrompt } from "./mutationScan";
import { svelteFiles, tsSourceFiles } from "./sourceScan";

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
 * The scanner (mutationScan.ts) reads the TypeScript syntax tree, and the
 * mutation names are derived from repoStore.ts by return type — members that
 * return `MutationOutcome` — so a new mutation is covered the day it is
 * written, however it reaches the wrapper.
 *
 * A mutation that follows a prompt, a confirm or a toast action must also name
 * the repository it was asked about: without a target it lands on whichever
 * tab is active when the answer arrives.
 */

const SRC = join(dirname(fileURLToPath(import.meta.url)), "..", "..");
const STORE_SOURCE = readFileSync(join(SRC, "lib", "stores", "repoStore.ts"), "utf8");

const FIXTURE_NAMES = new Set(["createBranch", "discardChanges", "checkoutBranch", "deleteBranch"]);

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
    for (const read of ["refresh", "openRepo", "closeTab", "selectFileDiff", "trustRepo", "setAmending"]) {
      expect(names.has(read)).toBe(false);
    }
  });
});

describe("discarded mutation outcomes", () => {
  const scan = (script: string) => discardedMutations(script, FIXTURE_NAMES);

  it("flags a bare awaited statement, however it is introduced", () => {
    expect(scan("await repoStore.createBranch(name, sha);")).toEqual([1]);
    expect(scan("if (ready) await repoStore.discardChanges(path);")).toEqual([1]);
    expect(scan("if (ready) {\n  await repoStore.createBranch(name);\n}")).toEqual([2]);
    expect(scan("else await repoStore.createBranch(name)")).toEqual([1]);
    expect(scan("const f = async () => {\n  await repoStore.discardChanges(\n    path,\n  );\n};")).toEqual([2]);
  });

  it("flags a bare awaited statement in markup", () => {
    const component = "<script>\n</script>\n<button onclick={async () => { await repoStore.createBranch(n); }}>x</button>";
    expect(scanSvelte(component, FIXTURE_NAMES).discarded).toEqual([3]);
  });

  it("flags a one-line arrow that is a markup event handler, and accepts its voided and handled forms", () => {
    const scanMarkup = (markup: string) => scanSvelte(`<script>\n</script>\n${markup}`, FIXTURE_NAMES).discarded;
    expect(scanMarkup("<button onclick={() => repoStore.checkoutBranch(x)}>x</button>")).toEqual([3]);
    expect(scanMarkup("<button on:click={() => repoStore.checkoutBranch(x)}>x</button>")).toEqual([3]);
    expect(scanMarkup("<button onclick={() => void repoStore.checkoutBranch(x)}>x</button>")).toEqual([]);
    expect(scanMarkup("<button onclick={() => handle(x)}>x</button>")).toEqual([]);
    // Not a handler: the value is rendered or passed on.
    expect(scanMarkup("<Row action={() => repoStore.checkoutBranch(x)} />")).toEqual([]);
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
    expect(scan("repoStore.createBranch(name).then((o) => report(o));")).toEqual([]);
  });

  it("flags the shapes that hide a discard", () => {
    expect(scan("repoStore.checkoutBranch(id);")).toEqual([1]);
    expect(scan("repoStore.createBranch(name).then(() => ok());")).toEqual([1]);
    expect(scan("repoStore.createBranch(name).catch(() => {});")).toEqual([1]);
    expect(scan("const { deleteBranch: del } = repoStore;\nawait del(name);")).toEqual([2]);
    expect(scan("const s = repoStore;\nawait s.createBranch(name);")).toEqual([2]);
    expect(scan("const go = repoStore.createBranch;\nawait go(name);")).toEqual([2]);
    expect(scan("await repoStore.createBranch?.(name);")).toEqual([1]);
    expect(scan("await repoStore['createBranch'](name);")).toEqual([1]);
    expect(scan("(await repoStore.createBranch(name));")).toEqual([1]);
    expect(scan("await repoStore\n  .createBranch(name);")).toEqual([1]);
    expect(scan("await repoStore.createBranch(name), done();")).toEqual([1]);
    expect(scan("await Promise.all(names.map((n) => repoStore.deleteBranch(n)));")).toEqual([1]);
    expect(scan("names.forEach((n) => repoStore.deleteBranch(n));")).toEqual([1]);
    expect(scan("ready && (await repoStore.createBranch(name));")).toEqual([1]);
  });

  it("accepts a combinator whose results are kept", () => {
    expect(scan("const all = await Promise.all(names.map((n) => repoStore.deleteBranch(n)));")).toEqual([]);
  });

  it("ignores reads, comments and strings", () => {
    expect(scan("await repoStore.refresh();")).toEqual([]);
    expect(scan("// await repoStore.createBranch(name);")).toEqual([]);
    expect(scan('const s = "await repoStore.createBranch(name);";')).toEqual([]);
  });
});

describe("a mutation after a prompt names its repository", () => {
  it("flags a post-prompt call with no target, and a target that is explicitly undefined", () => {
    expect(untargetedAfterPrompt("async function f() {\n  const n = await askText({});\n  await repoStore.createBranch(n, sha);\n}")).toEqual([3]);
    expect(untargetedAfterPrompt("async function f() {\n  await askConfirm({});\n  const o = await repoStore.discardChanges(path, undefined);\n}")).toEqual([3]);
    expect(untargetedAfterPrompt("toastStore.action('x', 'Undo', async () => {\n  await repoStore.createBranch(n, sha);\n});")).toEqual([2]);
  });

  it("sees through an aliased store, a destructured method and an aliased prompt", () => {
    expect(untargetedAfterPrompt("async function f() {\n  const s = repoStore;\n  await askConfirm({});\n  await s.discardChanges(path);\n}")).toEqual([4]);
    expect(untargetedAfterPrompt("async function f() {\n  const { deleteBranch: del } = repoStore;\n  await askConfirm({});\n  await del(name);\n}")).toEqual([4]);
    expect(untargetedAfterPrompt("import { askConfirm as ask } from './modalStore';\nasync function f() {\n  await ask({});\n  await repoStore.discardChanges(path);\n}")).toEqual([4]);
    expect(untargetedAfterPrompt("async function f(target) {\n  const s = repoStore;\n  await askConfirm({});\n  await s.discardChanges(path, target);\n}")).toEqual([]);
  });

  it("accepts a captured target, and a call that follows no prompt", () => {
    expect(untargetedAfterPrompt("async function f() {\n  const target = repoStore.captureTarget();\n  await askConfirm({});\n  await repoStore.discardChanges(path, target);\n}")).toEqual([]);
    expect(untargetedAfterPrompt("async function f() {\n  await repoStore.createBranch(name);\n}")).toEqual([]);
  });
});

describe("components never drop a mutation outcome", () => {
  it("holds for every component and palette module", () => {
    const names = mutationNames(STORE_SOURCE);
    const scans = [
      ...svelteFiles(SRC).map((file) => ({ file, ...scanSvelte(readFileSync(file, "utf8"), names) })),
      ...[join(SRC, "lib", "components"), join(SRC, "lib", "palette")]
        .flatMap(tsSourceFiles)
        .map((file) => ({ file, ...scanTypeScript(readFileSync(file, "utf8"), names) })),
    ];
    const at = (file: string, line: number) => `${relative(SRC, file)}:${line}`;
    expect(scans.flatMap(({ file, discarded }) => discarded.map((line) => at(file, line)))).toEqual([]);
    // The palette guards its post-prompt calls with `checkOrigin` instead of a
    // target (catalog.ts); moving it onto a target is a recorded follow-up, so
    // the target rule covers the components this card converted.
    const covered = scans.filter(({ file }) => !relative(SRC, file).startsWith(join("lib", "palette")));
    expect(covered.flatMap(({ file, untargeted }) => untargeted.map((line) => at(file, line)))).toEqual([]);
  });
});
