import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const source = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "App.svelte"), "utf8");

function scriptAndTemplate(svelte: string): { script: string; template: string } {
  const match = svelte.match(/<script\b[^>]*>([\s\S]*?)<\/script>/);
  if (!match || match.index === undefined) {
    throw new Error("App.svelte is missing a script block");
  }
  return { script: match[1], template: svelte.slice(match.index + match[0].length) };
}

function importedBindings(script: string): Set<string> {
  const names = new Set<string>();
  for (const match of script.matchAll(/import\s+(\w+)\s+from\s+/g)) {
    names.add(match[1]);
  }
  for (const match of script.matchAll(/import\s+(?:type\s+)?\{([^}]+)\}/g)) {
    const isTypeOnly = /import\s+type\s+\{/.test(script.slice(match.index, match.index + 20));
    if (isTypeOnly) continue;
    for (const spec of match[1].split(",")) {
      const trimmed = spec.trim();
      if (!trimmed || trimmed.startsWith("type ")) continue;
      const local = trimmed.split(/\s+as\s+/).at(-1)?.trim();
      if (local) names.add(local);
    }
  }
  return names;
}

function usedComponents(template: string): string[] {
  return [...new Set([...template.matchAll(/<([A-Z][A-Za-z0-9]*)\b/g)].map((match) => match[1]))];
}

describe("App overlay wiring", () => {
  it("imports every PascalCase component the template instantiates", () => {
    const { script, template } = scriptAndTemplate(source);
    const imported = importedBindings(script);
    const missing = usedComponents(template).filter((name) => !imported.has(name));
    expect(missing).toEqual([]);
    expect(imported.has("PromptModal")).toBe(true);
  });

  it("does not present the commit-search bar as if it filters Work", () => {
    const filterIdx = source.indexOf("<FilterBar");
    expect(filterIdx).toBeGreaterThan(-1);
    expect(source.slice(Math.max(0, filterIdx - 120), filterIdx)).toContain(
      "showsCommitFilter($repoStore.activeTab)",
    );
  });

  it("switches to Graph before focusing commit search when the bar is unmounted", () => {
    const fn = source.slice(
      source.indexOf("async function focusCommitSearch"),
      source.indexOf("onMount(() => {"),
    );
    expect(fn).toContain("tabForCommitSearch");
    expect(fn).toContain("repoStore.setActiveTab(target)");
    expect(fn).toContain("FOCUS_COMMIT_SEARCH_EVENT");
    expect(source).toContain("ownsCommitSearchChord($repoStore.activeTab)");
    expect(source).toContain("focusFilter: () => void focusCommitSearch()");
  });

  it("keeps PromptModal and DiagnosticsModal in separate crash boundaries", () => {
    const promptIdx = source.indexOf("<PromptModal");
    const diagIdx = source.indexOf("<DiagnosticsModal");
    expect(promptIdx).toBeGreaterThan(-1);
    expect(diagIdx).toBeGreaterThan(-1);
    const between =
      promptIdx < diagIdx ? source.slice(promptIdx, diagIdx) : source.slice(diagIdx, promptIdx);
    expect(between).toContain("</svelte:boundary>");
  });
});
