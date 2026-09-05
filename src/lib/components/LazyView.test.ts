import { readFileSync } from "node:fs";
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
      props: { load: () => Promise.resolve({ default: (() => {}) as never }), name: "Coverage" },
    });
    expect(body).toContain('aria-label="Loading Coverage"');
    expect(body).toContain('aria-busy="true"');
  });

  it("labels the pending state per view", () => {
    const { body } = render(LazyView, {
      props: { load: () => Promise.resolve({ default: (() => {}) as never }), name: "MANVI" },
    });
    expect(body).toContain('aria-label="Loading MANVI"');
  });
});

describe("LazyView pending and failed states", () => {
  const source = readFileSync(new URL("./LazyView.svelte", import.meta.url), "utf8");

  it("holds the pane with a skeleton rather than a line of centred text", () => {
    // The text placeholder was replaced by content of a different size on
    // arrival, so every deferred view opened with a layout jump.
    expect(source).toContain("<Skeleton");
    expect(source).not.toContain("Loading {name}…");
    // The accessible name still says what is arriving.
    expect(source).toContain('aria-label="Loading {name}"');
    expect(source).toContain('aria-busy="true"');
  });

  it("offers a retry instead of telling the user to reopen the window", () => {
    expect(source).toContain(">\n      Retry\n    </button>");
    expect(source).toContain("attempt += 1");
    expect(source).not.toContain("Reopening the window\n      retries it.");
  });

  it("re-runs the load effect on retry even though the loader is unchanged", () => {
    // LazyView keys its cache on loader identity, so without reading a
    // counter the effect would not re-run and Retry would do nothing.
    const effect = source.slice(source.indexOf("$effect(() => {"));
    expect(effect.slice(0, effect.indexOf("loader().then"))).toContain("void attempt;");
  });
});
