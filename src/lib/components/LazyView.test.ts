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

  it("hands a freshly loaded view in on the base token, transform and opacity only", () => {
    expect(source).toContain('motionMs("base")');
    expect(source).toContain("in:viewEnter");
    const enter = source.slice(source.indexOf("function viewEnter("), source.indexOf("let { load, name"));
    expect(enter).toContain("opacity: ${t}");
    expect(enter).toContain("translate3d(0,");
    expect(enter).not.toMatch(/\b(width|height|margin|top|left)\s*:/);
    // A cached view mounts plainly: a tab revisit never replays the hand-off.
    expect(source).toContain("{#if view && handedOff}");
    expect(source).toContain("{:else if view}");
  });

  it("does not hand off, and so remount, an instance whose sibling already resolved the view", () => {
    // Pinned from the agent-capacity harness: the dock mounts one panel per
    // repository tab, all awaiting one loader. A late sibling flipping to the
    // wrapper branch remounted its terminal panel and opened an extra shell.
    const landed = source.slice(source.indexOf("(module) => {"), source.indexOf("(error: unknown)"));
    expect(landed.indexOf("if (resolved.has(loader)) return;")).toBeGreaterThan(-1);
    expect(landed.indexOf("if (resolved.has(loader)) return;")).toBeLessThan(landed.indexOf("handedOff = true;"));
  });

  it("swaps the skeleton for the view without an outro that would split the box", () => {
    // Skeleton leaves at once by default; opting into `hold` would keep it in
    // flow beside the view that replaces it.
    expect(source.match(/<Skeleton\b/g)?.length).toBe(3);
    expect(source).not.toMatch(/<Skeleton[^>]*\bhold\b/);
  });

  it("spreads optional props onto the resolved view, the same seam LazyMount uses", () => {
    // TerminalDock binds each host to a repository path this way. Callers
    // that have no extra inputs pass nothing; an empty spread is <View />.
    expect(source).toContain("<View {...props} />");
    expect(source).toContain("props?: Record<string, unknown>");
  });
});
