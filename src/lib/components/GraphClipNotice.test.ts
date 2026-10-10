import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import GraphClipNotice from "./GraphClipNotice.svelte";

describe("GraphClipNotice", () => {
  it("renders a status line naming the lanes the canvas cannot draw", () => {
    const { body } = render(GraphClipNotice, { props: { clippedWidth: 600, laneWidth: 30 } });
    expect(body).toContain('data-testid="graph-clip-notice"');
    expect(body).toContain('role="status"');
    expect(body).toContain("20 branch lanes extend past the 4,096 px");
  });

  it("renders nothing for a graph that fits", () => {
    const { body } = render(GraphClipNotice, { props: { clippedWidth: 0, laneWidth: 30 } });
    expect(body).not.toContain("graph-clip-notice");
  });
});
