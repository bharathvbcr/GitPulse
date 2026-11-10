import { readFileSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";
import { get } from "svelte/store";
import { describe, expect, it } from "vitest";
import { filterPathsByFileQuery, parseFileQuery } from "../files/fileQuery";
import { mergeListedAndStatusPaths } from "../files/fileStatus";
import { memoryStorage } from "../repos/persist";
import { createRepoStore, type InvokeFn } from "./repoStore";
import { codeOnly, svelteFiles } from "./sourceScan";

/**
 * No `$effect` reads `$repoStore`.
 *
 * repoStore publishes a fresh object for every change it makes — a status
 * poll, a loading flag, an error — and Svelte treats every object-valued store
 * set as a change. An effect that reads `$repoStore.x` therefore re-runs on
 * all of them, whatever `x` is. That is how a file tree re-listed the
 * repository per poll, an armed "click again to remove" disarmed itself, a
 * GitHub spinner stayed on after its answer landed, and "Show more" snapped
 * back to the first page. Each was patched with a hand-copied `prev*` guard
 * until the next effect forgot one.
 *
 * Effects read `repoStore.currentPath` (the canonical repository key, which
 * notifies only on a switch) or a `$derived` of the field they need, which
 * compares by value; the read of `$repoStore` then lives outside the effect.
 */

const SRC = join(dirname(fileURLToPath(import.meta.url)), "..", "..");

interface EffectRead {
  line: number;
  read: string;
}

/** Every `$repoStore` read inside an `$effect` / `$effect.pre` body. */
function effectStoreReads(source: string): EffectRead[] {
  const reads: EffectRead[] = [];
  const scripts = /<script\b[^>]*>([\s\S]*?)<\/script>/g;
  let block: RegExpExecArray | null;
  while ((block = scripts.exec(source)) !== null) {
    const offset = block.index + block[0].indexOf(">") + 1;
    const code = codeOnly(block[1]);
    const effects = /\$effect(?:\.pre)?\s*\(/g;
    let effect: RegExpExecArray | null;
    while ((effect = effects.exec(code)) !== null) {
      let end = effect.index + effect[0].length;
      for (let parens = 1; end < code.length && parens > 0; end += 1) {
        if (code[end] === "(") parens += 1;
        else if (code[end] === ")") parens -= 1;
      }
      const body = code.slice(effect.index, end);
      for (const read of body.matchAll(/\$repoStore\b[\w.?]*/g)) {
        const at = offset + effect.index + (read.index ?? 0);
        reads.push({ line: source.slice(0, at).split("\n").length, read: read[0] });
      }
    }
  }
  return reads;
}

/**
 * The Code view's file tree is rebuilt whenever the array feeding it changes
 * identity, so the chain from `repoStore.statuses` to `buildFileTree` must
 * hand the same array down while nothing in it changed. vitest does not run
 * `$derived`, so the chain is checked where it is written: each link's
 * identity, the store's own `statuses` identity, and the panel's source.
 */
describe("file tree array identity", () => {
  const listed = ["a.ts", "src/b.ts"];

  it("merging no new status path hands back the listing itself", () => {
    expect(mergeListedAndStatusPaths(listed, [])).toBe(listed);
    expect(mergeListedAndStatusPaths(listed, ["a.ts", "src/b.ts"])).toBe(listed);
    expect(mergeListedAndStatusPaths(listed, ["new.ts"])).not.toBe(listed);
  });

  it("an unfiltered query hands back the paths themselves", () => {
    expect(filterPathsByFileQuery(listed, parseFileQuery("")).paths).toBe(listed);
    expect(filterPathsByFileQuery(listed, parseFileQuery("b")).paths).not.toBe(listed);
  });

  it("a selection publish keeps the statuses array; a status change replaces it", async () => {
    let statuses = [{ path: "a.ts", status_code: "M", is_staged: false, is_conflicted: false, additions: 1, deletions: 0 }];
    const invoke: InvokeFn = async (cmd, args) => {
      switch (cmd) {
        case "cmd_resolve_repo": return { path: String(args?.repoPath), name: "r", is_bare: false } as never;
        case "cmd_list_branches": return [] as never;
        case "cmd_get_status": return statuses as never;
        case "cmd_list_tags": return { tags: [], truncated: false } as never;
        case "cmd_stash_list": return { entries: [], truncated: false } as never;
        case "cmd_repo_operation": return null as never;
        case "cmd_watch_repo": return String(args?.repoPath) as never;
        case "cmd_unwatch_repo": case "cmd_note_tab_activated": case "cmd_set_recent_menu": return undefined as never;
        default: throw new Error(`unexpected command ${cmd}`);
      }
    };
    const store = createRepoStore({
      invoke,
      storage: memoryStorage(),
      caseInsensitive: true,
      graph: { showRepo: () => {}, loadGraph: async () => {}, evict: () => {} },
    });
    await store.openRepo("/r/identity");
    const before = get(store).statuses;
    expect(before).toHaveLength(1);

    store.selectFilePath("a.ts");
    expect(get(store).selectedFilePath).toBe("a.ts");
    expect(get(store).statuses).toBe(before);

    statuses = [...statuses, { path: "b.ts", status_code: "??", is_staged: false, is_conflicted: false, additions: 0, deletions: 0 }];
    await store.refresh("/r/identity", "status");
    expect(get(store).statuses).not.toBe(before);
    expect(get(store).statuses).toHaveLength(2);
  });

  it("FileTreePanel feeds its derived chain from one identity-stable statuses read", () => {
    const panel = readFileSync(join(SRC, "lib", "components", "files", "FileTreePanel.svelte"), "utf8");
    const script = codeOnly(/<script\b[^>]*>([\s\S]*?)<\/script>/.exec(panel)?.[1] ?? "");
    expect(script).toMatch(/const statuses = \$derived\(\$repoStore\.statuses\)/);
    // A fresh array built straight from the whole store defeats it: a derived
    // that maps `$repoStore.statuses` is a new array on every publish.
    expect(script).not.toMatch(/\$repoStore\.statuses\s*\.\s*map\(/);
    expect(script.match(/\$derived\(\s*statusPathKey\(statuses\)\s*\)/)).not.toBeNull();
  });
});

/**
 * A component that walks a list out of `$repoStore` — in a `$derived`, in
 * markup, in a handler — is subscribed to every publish, and the walk (a
 * `.map`, `.filter`, a spread) builds a fresh array each time, so everything
 * downstream of it re-runs too. The field stores (`repoStore.statuses`,
 * `.branches`, `.openTabs`, the counts) notify only when the value is
 * replaced.
 *
 * This is a ratchet. `KNOWN` is what the tree does today, per file; a new
 * whole-store array read in a component fails here, and so does fixing one
 * without lowering its count, so the list can only shrink. Migrate a file to
 * the field stores and delete its line.
 */
describe("array operations on $repoStore", () => {
  const LISTS = "statuses|branches|openTabs|tags|stashEntries|recentRepos";
  // Anything that walks, searches, copies or re-shapes the array.
  const METHODS =
    "map|filter|flatMap|reduce|reduceRight|slice|splice|sort|toSorted|toReversed|toSpliced|reverse|concat|find|findIndex|findLast|findLastIndex|some|every|forEach|includes|indexOf|lastIndexOf|at|join|flat|with|entries|keys|values";
  // `$repoStore.statuses` or `$repoStore["statuses"]`, optionally closed by `)` for `($repoStore.statuses).x`.
  const LIST = `\\$repoStore\\s*(?:\\.\\s*(?:${LISTS})\\b|\\[\\s*["'](?:${LISTS})["']\\s*\\])`;
  const SHAPES = [
    // $repoStore.statuses.filter(...), ?.filter(...), ($repoStore.statuses).filter(...)
    `${LIST}\\s*\\)?\\s*(?:\\?\\.|\\.)\\s*(?:${METHODS})\\s*\\(`,
    // [...$repoStore.statuses]
    `\\.\\.\\.\\s*${LIST}`,
    // Array.from($repoStore.statuses), Array.from(($repoStore.statuses), ...)
    `Array\\s*\\.\\s*from\\s*\\(\\s*\\(?\\s*${LIST}`,
    // for (const f of $repoStore.statuses)
    `\\bof\\s+\\(?\\s*${LIST}`,
  ];
  const reads = (source: string): number =>
    SHAPES.reduce((total, shape) => total + (source.match(new RegExp(shape, "g")) ?? []).length, 0);

  it("counts every shape that walks or copies a list, and nothing else", () => {
    expect(reads("{#each $repoStore.statuses.filter(f) as s}")).toBe(1);
    expect(reads("const x = $derived([...$repoStore.branches, b]);")).toBe(1);
    // The shapes the first version let through:
    expect(reads("const i = $repoStore.branches.findIndex((b) => b.is_current);")).toBe(1);
    expect(reads("const a = Array.from($repoStore.statuses);")).toBe(1);
    expect(reads("const a = ($repoStore.tags).filter((t) => t.name);")).toBe(1);
    expect(reads("const a = $repoStore?.statuses?.map(f);")).toBe(0); // `$repoStore?.` is not a shape the store has
    expect(reads('const a = $repoStore["statuses"].map(f);')).toBe(1);
    for (const method of ["includes", "indexOf", "lastIndexOf", "at", "join", "flat", "reduceRight", "findLast", "toSorted", "keys"]) {
      expect(reads(`const a = $repoStore.stashEntries.${method}(x);`), method).toBe(1);
    }
    expect(reads("for (const f of $repoStore.statuses) use(f);")).toBe(1);
    expect(reads("for (const f of ($repoStore.openTabs)) use(f);")).toBe(1);
    // Not walks: a plain read, a length, another field, the field stores.
    expect(reads("const s = $derived($repoStore.statuses);")).toBe(0);
    expect(reads("{$repoStore.statuses.length} {$repoStore.currentPath}")).toBe(0);
    expect(reads("const n = $statuses.filter(f);")).toBe(0);
    expect(reads("const n = $statusesStore.find(f);")).toBe(0);
    expect(reads("const n = $repoStore.currentBranch.includes('x');")).toBe(0);
  });

  const KNOWN: Record<string, number> = {
    "App.svelte": 1,
    "lib/components/BlameViewer.svelte": 2,
    "lib/components/BranchList.svelte": 6,
    "lib/components/CommandPalette.svelte": 1,
    "lib/components/CommitComposer.svelte": 1,
    "lib/components/ConflictEditor.svelte": 2,
    "lib/components/DiffViewer.svelte": 1,
    "lib/components/FileViewer.svelte": 3,
    "lib/components/FleetView.svelte": 1,
    "lib/components/GitHubPanel.svelte": 2,
    "lib/components/RepoPanel.svelte": 1,
    "lib/components/RepoTabBar.svelte": 12,
    "lib/components/StatusBar.svelte": 3,
    "lib/components/TaskAgentPanel.svelte": 1,
    "lib/components/TaskBoard.svelte": 1,
    "lib/components/TerminalDock.svelte": 2,
    "lib/components/WorkView.svelte": 1,
    "lib/components/WorkspaceActions.svelte": 1,
    "lib/components/WorktreesPanel.svelte": 3,
    "lib/components/files/FileTreePanel.svelte": 1,
    "lib/components/pulse/PulseView.svelte": 1,
  };

  it("matches the known list: no new whole-store array reads, none left uncounted", () => {
    const found: Record<string, number> = {};
    for (const file of svelteFiles(SRC)) {
      const count = reads(readFileSync(file, "utf8"));
      if (count > 0) found[relative(SRC, file)] = count;
    }
    expect(found).toEqual(KNOWN);
  });
});

/** The conflicted and staged counts are taken once per statuses array in the projection. */
describe("counts come from the projection", () => {
  const COMPONENTS = [
    "App.svelte",
    "lib/components/CommitComposer.svelte",
    "lib/components/AgentCommitMenu.svelte",
    "lib/components/WorkspaceView.svelte",
  ];
  const recount = /statuses\s*\.\s*filter\(\s*\(?\s*\w+\s*\)?\s*=>\s*\w+\.is_(?:conflicted|staged)\s*\)\s*\.\s*length/;

  it.each(COMPONENTS)("%s does not recount statuses itself", (file) => {
    expect(readFileSync(join(SRC, file), "utf8")).not.toMatch(recount);
  });

  it("every count the projection takes has a consumer", () => {
    const consumers = (field: string) =>
      svelteFiles(SRC).filter((file) => new RegExp(`\\$repoStore\\.${field}\\b|repoStore\\.${field}\\b`).test(readFileSync(file, "utf8")));
    expect(consumers("conflictedCount").length).toBeGreaterThan(0);
    expect(consumers("stagedCount").length).toBeGreaterThan(0);
  });
});

describe("effects never read $repoStore", () => {
  it("finds a read in an effect, and nothing in comments, strings or outside effects", () => {
    const fixture = [
      "<script>",
      "  const path = $derived($repoStore.currentPath);",
      "  // $effect(() => $repoStore.currentPath) in a comment",
      "  $effect(() => {",
      "    /* $repoStore.branches in a block comment */",
      "    const label = \"$repoStore.x) in a string\";",
      "    const re = /\\)$/;",
      "    const tpl = `${$repoStore.generation} and $repoStore.notCode`;",
      "    if (path) void $repoStore.currentPath;",
      "  });",
      "  $effect.pre(() => { const s = $repoStore; void s; });",
      "  $effect(() => { void $currentRepoPath; });",
      "</script>",
      "<p>{$repoStore.currentPath} — markup is not an effect</p>",
    ].join("\n");

    expect(effectStoreReads(fixture)).toEqual([
      { line: 8, read: "$repoStore.generation" },
      { line: 9, read: "$repoStore.currentPath" },
      { line: 11, read: "$repoStore" },
    ]);
  });

  it("keeps one repository key: no component derives its own from the store", () => {
    // `repoStore.currentPath` is the canonical key. A private
    // `$derived($repoStore.currentPath)` behaves the same today and drifts
    // tomorrow — each copy grew its own guard beside it.
    const copies = svelteFiles(SRC).filter((file) =>
      /\$derived\(\s*\$repoStore\.currentPath\s*\)/.test(readFileSync(file, "utf8")),
    );
    expect(copies.map((file) => relative(SRC, file))).toEqual([]);
  });

  it("holds for every component", () => {
    const offenders = svelteFiles(SRC).flatMap((file) =>
      effectStoreReads(readFileSync(file, "utf8")).map(
        ({ line, read }) => `${relative(SRC, file)}:${line} ${read}`,
      ),
    );
    expect(offenders).toEqual([]);
  });
});
