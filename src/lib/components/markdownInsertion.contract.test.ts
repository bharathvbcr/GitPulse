import { readFileSync, readdirSync } from "node:fs";
import { join, relative } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

/**
 * Rendered Markdown is repository content placed in the app's own document.
 * `MarkdownContent.svelte` is the one place that does it, because that is
 * where GitPulse's half of the boundary lives: ids namespaced so a README
 * heading cannot shadow an app element, pictures that would resolve against
 * the app's origin replaced, and every link judged on click so the webview
 * never follows one. A second `{@html}` of a render skips all three.
 *
 * Derived, not listed: every Svelte file that asks for a render is swept, so
 * a new caller is held to it without anyone remembering to add it here.
 */
const SRC = fileURLToPath(new URL("../../", import.meta.url));

function svelteFiles(dir: string): string[] {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const full = join(dir, entry.name);
    if (entry.isDirectory()) return svelteFiles(full);
    return entry.name.endsWith(".svelte") ? [full] : [];
  });
}

const OWNER = "lib/components/MarkdownContent.svelte";

describe("rendered Markdown reaches the document through MarkdownContent only", () => {
  const renderers = svelteFiles(SRC)
    .map((path) => ({ path: relative(SRC, path), text: readFileSync(path, "utf8") }))
    .filter((file) => file.text.includes("renderMarkDevMarkdown"));

  it("finds the renderers it guards", () => {
    // A sweep that matched nothing would pass vacuously.
    expect(renderers.map((file) => file.path).sort()).toEqual(
      expect.arrayContaining(["lib/components/MarkdownBody.svelte", "lib/components/files/MarkDevViewer.svelte"]),
    );
  });

  it("no renderer inserts HTML itself", () => {
    for (const file of renderers) {
      expect(file.text, file.path).not.toContain("{@html");
      expect(file.text, file.path).toContain("MarkdownContent");
    }
  });

  it("the owner prepares before it inserts", () => {
    const owner = readFileSync(join(SRC, OWNER), "utf8");
    const inserts = [...owner.matchAll(/\{@html\s+([^}]+)\}/g)].map((match) => match[1].trim());
    expect(inserts).toEqual(["prepared"]);
    expect(owner).toContain("const prepared = $derived(prepareRenderedMarkdown(html, note));");
  });
});
