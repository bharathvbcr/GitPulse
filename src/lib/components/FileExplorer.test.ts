import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const here = dirname(fileURLToPath(import.meta.url));

/**
 * Source-level contract for the blame page's File Explorer (node environment:
 * nothing mounts here — the pure tree logic it leans on is unit- and
 * stress-tested in src/lib/files, this pins the component's wiring).
 */
const source = readFileSync(join(here, "FileExplorer.svelte"), "utf8");

describe("FileExplorer", () => {
  it("lists files through the registered IPC command", () => {
    expect(source).toContain('invoke<string[]>("cmd_list_repo_files"');
  });

  it("builds its rows exclusively from the pure fileTree pipeline", () => {
    expect(source).toContain('from "../files/fileTree"');
    for (const fn of ["buildFileTree", "flattenFileTree", "filterPathsByQuery", "ancestorsOf"]) {
      expect(source).toContain(fn);
    }
  });

  it("selects through the shared session store, mirroring outward", () => {
    expect(source).toContain("repoStore.selectFilePath(path)");
    // No local-only selection path may bypass the store.
    expect(source).not.toMatch(/let selectedPath/);
  });

  it("guards loads against races and cancels in-flight work on repo switch", () => {
    expect(source).toContain("createAsyncGuard");
    expect(source).toContain("guard.isLive()");
    expect(source.match(/inflight\?\.cancel\(\)/g)?.length).toBeGreaterThanOrEqual(2);
  });

  it("routes rejections through formatError", () => {
    expect(source).toContain("formatError(err)");
  });

  it("supports keyboard navigation and activation like BranchList", () => {
    for (const key of ['"ArrowDown"', '"ArrowUp"', '"ArrowRight"', '"ArrowLeft"', '"Enter"', '"Escape"']) {
      expect(source, key).toContain(key);
    }
    expect(source).toContain("ensureVisible");
  });

  it("expands filtered results so matches are never hidden behind collapsed folders", () => {
    expect(source).toContain("isFiltering ? false : collapsed[dirPath] === true");
  });

  it("reveals the store's selected file by opening its ancestor chain", () => {
    expect(source).toContain("ancestorsOf(selected)");
    // Conditional write: unconditional collapse writes inside an effect that
    // reads collapsed would loop forever under Svelte 5.
    expect(source).toContain("if (changed) collapsed = next;");
  });
});
