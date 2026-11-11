import { readdirSync, readFileSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";
import { afterEach, describe, expect, it } from "vitest";
import { setReduceMotionOverride } from "../motion/easing";
import { EXIT_MS } from "../motion/tokens";
import { popoverExit } from "./popover";

const SRC = join(dirname(fileURLToPath(import.meta.url)), "..", "..");

function svelteFiles(dir: string): string[] {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) return svelteFiles(path);
    return path.endsWith(".svelte") ? [path] : [];
  });
}

/** The opening tag around every `use:popover` in a component, braces and quotes balanced. */
function popoverTags(markup: string): string[] {
  // A comment that names the action is not an element that uses it.
  const source = markup.replace(/<!--[\s\S]*?-->/g, (comment) => " ".repeat(comment.length));
  const tags: string[] = [];
  for (let at = source.indexOf("use:popover"); at >= 0; at = source.indexOf("use:popover", at + 1)) {
    const start = source.lastIndexOf("<", at);
    let depth = 0;
    let quote = "";
    let end = at;
    for (; end < source.length; end += 1) {
      const ch = source[end];
      if (quote) { if (ch === quote) quote = ""; continue; }
      if (depth === 0 && (ch === '"' || ch === "'")) { quote = ch; continue; }
      if (ch === "{") depth += 1;
      else if (ch === "}") depth -= 1;
      else if (ch === ">" && depth === 0) break;
    }
    tags.push(source.slice(start, end + 1));
  }
  return tags;
}

describe("popoverExit", () => {
  afterEach(() => setReduceMotionOverride(false));

  it("is the exit cap, 70 ms or less, on transform and opacity only", () => {
    const exit = popoverExit();
    expect(exit.duration).toBe(EXIT_MS.fast);
    expect(exit.duration).toBeLessThanOrEqual(70);
    for (const t of [0, 0.3, 0.6, 1]) {
      const css = exit.css?.(t, 1 - t) ?? "";
      const properties = [...css.matchAll(/([a-z-]+)\s*:/g)].map((m) => m[1]);
      expect(properties.length).toBeGreaterThan(0);
      for (const property of properties) expect(["opacity", "transform"]).toContain(property);
    }
  });

  it("takes no time under reduced motion, so Svelte skips the outro and the popover just goes", () => {
    setReduceMotionOverride(true);
    expect(popoverExit().duration).toBe(0);
  });
});

describe("every popover the action places leaves with the shared exit", () => {
  /** Elements that carry `use:popover` for dismissal only and own a different exit. Each names the evidence. */
  const OWN_EXIT: Record<string, { tag: RegExp; why: string }> = {
    "lib/components/Sidebar.svelte": { tag: /out:panelOut/, why: "the floating sidebar slides out" },
    "lib/components/TerminalDock.svelte": { tag: /gp-dock-motion/, why: "the dock exits through gp-dock-slide-out (data-phase)" },
    "lib/components/TaskBoard.svelte": { tag: /out:fly/, why: "the scope rail overlay flies out" },
  };

  const found = svelteFiles(SRC).flatMap((file) => {
    const rel = relative(SRC, file);
    return popoverTags(readFileSync(file, "utf8")).map((tag) => ({ rel, tag }));
  });

  it("finds the popovers it is meant to police", () => {
    expect(found.length).toBeGreaterThanOrEqual(25);
  });

  it("has out:popoverExit on each one, or names the exit it has instead", () => {
    const missing = found
      .filter(({ rel, tag }) => !/\bout:popoverExit\b/.test(tag) && !OWN_EXIT[rel]?.tag.test(tag))
      .map(({ rel, tag }) => `${rel}: ${tag.slice(0, 120).replace(/\s+/g, " ")}`);
    expect(missing).toEqual([]);
  });

  it("lets only the listed elements keep their own exit, and the shared one covers the rest", () => {
    for (const { rel, tag } of found) {
      if (OWN_EXIT[rel]?.tag.test(tag)) expect(tag, `${rel}: ${OWN_EXIT[rel].why}`).not.toMatch(/\bout:popoverExit\b/);
    }
    const shared = found.filter(({ tag }) => /\bout:popoverExit\b/.test(tag));
    expect(shared.length).toBeGreaterThanOrEqual(22);
  });

  it("is the repository strip's exit too: RepoTabBar no longer keeps its own popupExit", () => {
    const bar = readFileSync(join(SRC, "lib/components/RepoTabBar.svelte"), "utf8");
    expect(bar).not.toContain("popupExit");
    expect(bar.match(/in:popupEnter\s+out:popoverExit/g)).toHaveLength(2);
    expect(readFileSync(join(SRC, "lib/ui/headerMotion.ts"), "utf8")).not.toMatch(/export const popupExit\b/);
  });
});
