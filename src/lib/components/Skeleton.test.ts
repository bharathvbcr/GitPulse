import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
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

  it("supports card, tree-row, and rect variants", () => {
    const card = render(Skeleton, { props: { variant: "card", count: 2 } });
    expect(card.body).toContain("rounded-2xl");

    const tree = render(Skeleton, { props: { variant: "tree-row", count: 4 } });
    expect(tree.body).toContain("flex items-center");

    const circle = render(Skeleton, { props: { variant: "circle" } });
    expect(circle.body).toContain("rounded-full");
  });
});
