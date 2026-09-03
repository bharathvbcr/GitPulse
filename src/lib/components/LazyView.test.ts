import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import LazyView from "./LazyView.svelte";

/**
 * Tests run in vitest's `node` environment, so only the server-rendered
 * pending branch is exercised here — `$effect` (the load, cache, cancel and
 * failure paths) needs a DOM and is not covered by this file.
 */
describe("LazyView (server render)", () => {
  it("renders an accessible pending state before the chunk resolves", () => {
    const { body } = render(LazyView, {
      props: { load: () => Promise.resolve({ default: (() => {}) as never }), label: "Coverage" },
    });
    expect(body).toContain('aria-label="Loading Coverage"');
    expect(body).toContain('aria-busy="true"');
  });

  it("labels the pending state per view", () => {
    const { body } = render(LazyView, {
      props: { load: () => Promise.resolve({ default: (() => {}) as never }), label: "MANVI" },
    });
    expect(body).toContain('aria-label="Loading MANVI"');
  });
});
