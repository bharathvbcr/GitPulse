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

  it("supports card, tree-row, and rect variants", () => {
    const card = render(Skeleton, { props: { variant: "card", count: 2 } });
    expect(card.body).toContain("rounded-2xl");

    const tree = render(Skeleton, { props: { variant: "tree-row", count: 4 } });
    expect(tree.body).toContain("flex items-center");

    const circle = render(Skeleton, { props: { variant: "circle" } });
    expect(circle.body).toContain("rounded-full");
  });
});
