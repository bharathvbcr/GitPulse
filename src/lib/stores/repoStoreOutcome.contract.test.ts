import { readFileSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { directMutatingInvokes, directMutatingInvokesSvelte, discardedMutations, mutatingCommands, mutationNames, scanSvelte, scanTypeScript, untargetedAfterPrompt } from "./mutationScan";
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
    // The palette is held to the target rule like every component: its
    // post-prompt calls name the repository the palette was built for.
    expect(scans.flatMap(({ file, untargeted }) => untargeted.map((line) => at(file, line)))).toEqual([]);
  }, 60_000);
});

describe("components never send a mutating command themselves", () => {
  const commands = mutatingCommands(STORE_SOURCE);

  it("derives the commands from the store's mutations", () => {
    for (const routed of [
      "cmd_prune_worktree",
      "cmd_worktree_cow_sync",
      "cmd_move_path",
      "cmd_delete_path",
      "cmd_create_file",
      "cmd_deadbranch_clean",
      "cmd_deadbranch_restore",
      "cmd_discard_changes",
      "cmd_push",
    ]) {
      expect(commands.has(routed), routed).toBe(true);
    }
    for (const read of ["cmd_list_worktrees", "cmd_deadbranch_scan", "cmd_list_repo_files", "cmd_get_status"]) {
      expect(commands.has(read), read).toBe(false);
    }
  });

  it("flags a planted direct invoke, however it is spelled", () => {
    expect(directMutatingInvokes('await invoke("cmd_prune_worktree", { repoPath });', commands)).toEqual([1]);
    expect(directMutatingInvokes('const r = await invoke<Guarded<null>>("cmd_move_path", { repoPath, from, to });', commands)).toEqual([1]);
    expect(directMutatingInvokes("const go = safeInvoke;\nawait go(`cmd_delete_path`, { repoPath, path });", commands)).toEqual([2]);
    expect(directMutatingInvokes('await tauri.core.invoke("cmd_create_file", {});', commands)).toEqual([1]);
    expect(directMutatingInvokes('await invoke("cmd_list_worktrees", { repoPath });', commands)).toEqual([]);
    expect(directMutatingInvokes('// await invoke("cmd_prune_worktree");\nconst s = "cmd_prune_worktree";', commands)).toEqual([]);
  });

  it("flags a planted direct invoke in a component's script and in its markup", () => {
    const script = '<script lang="ts">\n  import { invoke } from "../ipc/invoke";\n  async function prune() {\n    await invoke("cmd_prune_worktree", { repoPath: "/r" });\n  }\n</script>\n<button onclick={prune}>Prune</button>';
    expect(directMutatingInvokesSvelte(script, commands)).toEqual([4]);
    const markup = '<script lang="ts">\n  import { invoke } from "../ipc/invoke";\n</script>\n<button onclick={() => invoke("cmd_delete_path", { repoPath: "/r", path: "a" })}>Delete</button>';
    expect(directMutatingInvokesSvelte(markup, commands)).toEqual([4]);
    expect(directMutatingInvokesSvelte('<script lang="ts">\n  const n = 1;\n</script>', commands)).toEqual([]);
  });

  it("reads a command name held in a same-file constant, or built from a cmd_ prefix", () => {
    expect(directMutatingInvokes('const PRUNE = "cmd_prune_worktree";\nawait invoke(PRUNE, { repoPath });', commands)).toEqual([2]);
    expect(directMutatingInvokes('const PRUNE = `cmd_prune_worktree` as const;\nawait invoke(PRUNE);', commands)).toEqual([2]);
    expect(directMutatingInvokes('await invoke("cmd_" + verb, { repoPath });', commands)).toEqual([1]);
    expect(directMutatingInvokes("await invoke(`cmd_${verb}`, { repoPath });", commands)).toEqual([1]);
    expect(directMutatingInvokes('await invoke("cmd_list_" + what, { repoPath });', commands)).toEqual([]);
    expect(directMutatingInvokes('const READ = "cmd_list_worktrees";\nawait invoke(READ);', commands)).toEqual([]);
  });

  it("flags an invoke whose command cannot be read, through every spelling of the bridge", () => {
    expect(directMutatingInvokes('import { invoke as call } from "@tauri-apps/api/core";\nawait call(cmdName, {});', commands)).toEqual([2]);
    expect(directMutatingInvokes('import { invoke } from "../ipc/invoke";\nawait invoke(cmdName, {});', commands)).toEqual([2]);
    expect(directMutatingInvokes("await window.__TAURI__.core.invoke(cmdName, {});", commands)).toEqual([1]);
    expect(directMutatingInvokes('await window.__TAURI__.core["invoke"](cmdName, {});', commands)).toEqual([1]);
    expect(directMutatingInvokes('const { invoke: go } = await import("@tauri-apps/api/core");\nawait go(cmdName);', commands)).toEqual([2]);
    expect(directMutatingInvokes('const m = await import("../ipc/invoke");\nawait m.invoke(cmdName);', commands)).toEqual([2]);
    expect(directMutatingInvokes("const call = window.__TAURI__.core.invoke;\nawait call(cmdName);", commands)).toEqual([2]);
    // A call that is not the bridge is not read for a command it cannot name.
    expect(directMutatingInvokes("await save(fileName, {});", commands)).toEqual([]);
  });

  it("reads a component whose script holds the command and whose markup sends it", () => {
    const held = '<script lang="ts">\n  import { invoke as call } from "../ipc/invoke";\n  const PRUNE = "cmd_prune_worktree";\n</script>\n<button onclick={() => call(PRUNE, {})}>Prune</button>';
    expect(directMutatingInvokesSvelte(held, commands)).toEqual([5]);
    const unread = '<script lang="ts">\n  import { invoke } from "../ipc/invoke";\n  let name = $state("x");\n</script>\n<button onclick={() => invoke(name)}>Go</button>';
    expect(directMutatingInvokesSvelte(unread, commands)).toEqual([5]);
    expect(directMutatingInvokesSvelte('<script lang="ts">\n  import { listen } from "@tauri-apps/api/event";\n  listen(EVENT, () => {});\n</script>', commands)).toEqual([]);
  });

  it("reads a command named in any argument, not only the first", () => {
    expect(directMutatingInvokes('await sendWithRetry(3, "cmd_push", { repoPath });', commands)).toEqual([1]);
  });

  it("cannot see a mutating command the store does not send, and says so", () => {
    // The scanner's set is the store's. These are sent from WorktreesPanel
    // itself; until the store owns them they are invisible here by design.
    for (const outside of ["cmd_add_worktree", "cmd_remove_worktree", "cmd_lock_worktree", "cmd_unlock_worktree", "cmd_merge_and_teardown"]) {
      expect(commands.has(outside), outside).toBe(false);
    }
  });

  it("holds for every production module under src/lib, except the known unrouted ones", () => {
    const owners = new Set([join(SRC, "lib", "stores", "repoStore.ts")]);
    const hits = tsSourceFiles(join(SRC, "lib"))
      .filter((file) => !owners.has(file))
      .flatMap((file) => directMutatingInvokes(readFileSync(file, "utf8"), commands).map((line) => `${relative(SRC, file)}:${line}`));
    // attemptWorktree deletes the agent branch with a direct cmd_delete_branch:
    // a known bypass outside the scanned component dirs, left for its own task.
    expect(hits.map((hit) => hit.replace(/:\d+$/, ""))).toEqual(["lib/workbench/attemptWorktree.ts"]);
  }, 60_000);

  it("holds for every component and palette module", () => {
    const files = [
      ...svelteFiles(SRC).map((file) => ({ file, lines: directMutatingInvokesSvelte(readFileSync(file, "utf8"), commands) })),
      ...[join(SRC, "lib", "components"), join(SRC, "lib", "palette")]
        .flatMap(tsSourceFiles)
        .map((file) => ({ file, lines: directMutatingInvokes(readFileSync(file, "utf8"), commands) })),
    ];
    expect(files.flatMap(({ file, lines }) => lines.map((line) => `${relative(SRC, file)}:${line}`))).toEqual([]);
  }, 60_000);
});
