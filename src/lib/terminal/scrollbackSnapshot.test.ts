import { describe, expect, it } from "vitest";
import {
  createScrollbackSnapshots,
  MAX_SCROLLBACK_SNAPSHOT_BYTES,
  MAX_SCROLLBACK_SNAPSHOT_ENTRIES,
} from "./scrollbackSnapshot";

describe("scrollbackSnapshot", () => {
  it("captures and takes back a visual ANSI snapshot once", () => {
    const store = createScrollbackSnapshots();
    expect(store.capture("tab-1", () => "\u001b[32mPASS\u001b[0m src/app.ts\r\n")).toBe(true);
    expect(store.peek("tab-1")).toBe("\u001b[32mPASS\u001b[0m src/app.ts\r\n");
    expect(store.take("tab-1")).toBe("\u001b[32mPASS\u001b[0m src/app.ts\r\n");
    expect(store.take("tab-1")).toBeNull();
  });

  it("ignores empty or whitespace-only snapshots", () => {
    const store = createScrollbackSnapshots();
    expect(store.capture("tab-1", () => "   \r\n  ")).toBe(false);
    expect(store.peek("tab-1")).toBeNull();
  });

  it("clips oversized serialized buffers on a clean line boundary within MAX_SCROLLBACK_SNAPSHOT_BYTES", () => {
    const store = createScrollbackSnapshots({ maxBytesPerEntry: 64 });
    const huge = [
      "line-1-0123456789",
      "line-2-0123456789",
      "line-3-0123456789",
      "line-4-0123456789",
      "line-5-0123456789",
    ].join("\r\n");
    expect(store.capture("tab-1", () => huge)).toBe(true);
    const saved = store.take("tab-1") ?? "";
    expect(new TextEncoder().encode(saved).byteLength).toBeLessThanOrEqual(64);
    expect(saved.startsWith("\u001b[0m")).toBe(true);
    expect(saved).toContain("line-5-0123456789");
    expect(saved).not.toContain("line-1-0123456789");
  });

  it("evicts oldest entries past MAX_SCROLLBACK_SNAPSHOT_ENTRIES and supports forget", () => {
    const store = createScrollbackSnapshots({ maxEntries: 2 });
    store.capture("a", () => "one");
    store.capture("b", () => "two");
    store.capture("c", () => "three");
    expect(store.peek("a")).toBeNull();
    expect(store.peek("b")).toBe("two");
    expect(store.peek("c")).toBe("three");
    store.forget("b");
    expect(store.peek("b")).toBeNull();
    expect(MAX_SCROLLBACK_SNAPSHOT_BYTES).toBe(256 * 1024);
    expect(MAX_SCROLLBACK_SNAPSHOT_ENTRIES).toBeGreaterThanOrEqual(16);
  });

  it("swallows serializer throws and returns false rather than crashing unmount", () => {
    const store = createScrollbackSnapshots();
    expect(
      store.capture("tab-1", () => {
        throw new Error("disposed buffer");
      }),
    ).toBe(false);
  });
});
