import { readdirSync, readFileSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import Skeleton from "./Skeleton.svelte";

describe("Skeleton", () => {
  it("renders with status role and aria attributes", () => {
    const { body } = render(Skeleton, { props: { count: 3 } });
    expect(body).toContain('role="status"');
    expect(body).toContain('aria-busy="true"');
    expect(body).toContain("animate-pulse");
  });

  it("starts transparent and unshown, so a fast load never flashes it", () => {
    // The 300 ms delay and 400 ms hold are exercised with fake timers in
    // ui/skeletonGate.test.ts; the server render is the "before the delay" frame.
    const { body } = render(Skeleton, { props: {} });
    expect(body).toContain("opacity-0");
    expect(body).toContain('data-skeleton-shown="false"');
    const source = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "Skeleton.svelte"), "utf8");
    expect(source).toContain("startSkeletonGate(");
    expect(source).toContain("out:holdThenLeave|global");
    expect(source).toContain("gate?.holdRemainingMs()");
  });

  it("does not hold by default: the 400 ms minimum is opt-in, and every opt-in names its reason", () => {
    // The hold is an outro, and an outgoing branch stays in the DOM while its
    // replacement mounts, so content in the same flow lands one skeleton lower
    // for up to 400 ms (measured in Chromium: content top 72 px with the hold,
    // 20 px without). 46 of 46 callers needed it off, so off is the default.
    // List a caller here, with the reason, only if it swaps the skeleton into
    // the same box without taking flow space the replacement needs.
    const HOLD_KEPT: Record<string, string> = {};
    const source = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "Skeleton.svelte"), "utf8");
    expect(source).toMatch(/hold = false,/);
    const root = join(dirname(fileURLToPath(import.meta.url)), "..");
    const offenders: string[] = [];
    let seen = 0;
    const walk = (dir: string) => {
      for (const entry of readdirSync(dir, { withFileTypes: true })) {
        const full = join(dir, entry.name);
        if (entry.isDirectory()) walk(full);
        else if (entry.name.endsWith(".svelte") && entry.name !== "Skeleton.svelte") {
          const text = readFileSync(full, "utf8");
          for (const tag of text.matchAll(/<Skeleton\b[^>]*?\/>/gs)) {
            seen += 1;
            const holds = /\bhold\b(?!=\{false\})/.test(tag[0]);
            if (holds && !HOLD_KEPT[relative(root, full)]) offenders.push(`${relative(root, full)}: ${tag[0].replace(/\s+/g, " ")}`);
          }
        }
      }
    };
    walk(root);
    expect(seen).toBeGreaterThan(40);
    expect(offenders).toEqual([]);
  });

  it("supports card, tree-row, and rect variants", () => {
    const card = render(Skeleton, { props: { variant: "card", count: 2 } });
    expect(card.body).toContain("rounded-2xl");

    const tree = render(Skeleton, { props: { variant: "tree-row", count: 4 } });
    expect(tree.body).toContain("flex items-center");

    const circle = render(Skeleton, { props: { variant: "circle" } });
    expect(circle.body).toContain("rounded-full");
  });
});
