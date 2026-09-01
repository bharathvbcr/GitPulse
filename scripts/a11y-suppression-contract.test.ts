import { readdirSync, readFileSync, statSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const SRC = fileURLToPath(new URL("../src", import.meta.url));

function svelteFiles(dir: string): string[] {
  return readdirSync(dir).flatMap((entry) => {
    const full = path.join(dir, entry);
    if (statSync(full).isDirectory()) return svelteFiles(full);
    return full.endsWith(".svelte") ? [full] : [];
  });
}

/**
 * `npm run check` reporting "0 warnings" means zero *unsuppressed* warnings.
 * A bare `svelte-ignore` makes a rule that never ran look the same as a rule
 * that ran and passed, so every suppression must say why it is correct.
 */
describe("a11y suppression contract", () => {
  const files = svelteFiles(SRC);

  it("covers the component tree", () => {
    expect(files.length).toBeGreaterThan(40);
  });

  it("requires every svelte-ignore to carry a written justification", () => {
    const unjustified: string[] = [];
    for (const file of files) {
      const lines = readFileSync(file, "utf8").split("\n");
      lines.forEach((line, index) => {
        if (!line.includes("svelte-ignore")) return;
        // The justification may sit on either side of a multi-line ignore block.
        const window = lines.slice(Math.max(0, index - 3), index + 6).join("\n");
        if (!/Justified:|Accepted, not fixed:/.test(window)) {
          unjustified.push(`${path.relative(SRC, file)}:${index + 1}`);
        }
      });
    }
    expect(unjustified).toEqual([]);
  });
});
