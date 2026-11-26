import { afterEach, describe, expect, it, vi } from "vitest";
import { copyImageBlob, copyText, createCopyFlash, readClipboardText, type WebClipboard } from "./clipboard";

function fakeDocument(execResult: boolean) {
  const removed: string[] = [];
  const textarea = {
    value: "",
    setAttribute: () => {},
    style: {} as Record<string, string>,
    select: () => {},
  };
  return {
    textarea,
    removed,
    execCommand: (command: string) => command === "copy" && execResult,
    createElement: () => textarea,
    body: { appendChild: () => {}, removeChild: (node: unknown) => void removed.push(String(node)) },
  };
}

describe("copyText", () => {
  it("uses the async clipboard when it works", async () => {
    const written: string[] = [];
    const clipboard: WebClipboard = { writeText: async (t) => void written.push(t) };
    expect(await copyText("hello", { clipboard })).toBe(true);
    expect(written).toEqual(["hello"]);
  });

  it("falls back to execCommand when the async clipboard rejects", async () => {
    const doc = fakeDocument(true);
    const ok = await copyText(
      "fallback",
      { clipboard: { writeText: async () => Promise.reject(new Error("denied")) }, document: doc },
    );
    expect(ok).toBe(true);
    expect(doc.textarea.value).toBe("fallback");
    expect(doc.removed).toHaveLength(1);
  });

  it("reports failure when both paths fail", async () => {
    const doc = fakeDocument(false);
    const ok = await copyText(
      "lost",
      { clipboard: { writeText: async () => Promise.reject(new Error("denied")) }, document: doc },
    );
    expect(ok).toBe(false);
  });

  it("refuses an empty payload instead of pretending it copied", async () => {
    expect(await copyText("", { clipboard: { writeText: async () => {} } })).toBe(false);
    expect(await copyText("")).toBe(false);
  });

  it("fails closed with no clipboard and no document", async () => {
    expect(await copyText("text", { clipboard: null, document: null })).toBe(false);
  });
});

describe("readClipboardText", () => {
  it("returns the clipboard's text", async () => {
    expect(await readClipboardText({ clipboard: { readText: async () => "pasted" } })).toEqual({ ok: true, text: "pasted" });
  });

  it("says unavailable when there is no clipboard or no read API, and failed when the read throws", async () => {
    expect(await readClipboardText({ clipboard: null })).toEqual({ ok: false, reason: "unavailable" });
    expect(await readClipboardText({ clipboard: {} })).toEqual({ ok: false, reason: "unavailable" });
    const refused = { readText: () => Promise.reject(new Error("NotAllowedError")) };
    expect(await readClipboardText({ clipboard: refused })).toEqual({ ok: false, reason: "failed" });
  });
});

describe("createCopyFlash", () => {
  afterEach(() => vi.useRealTimers());
  const ok: WebClipboard = { writeText: async () => {} };
  const bad: WebClipboard = { writeText: async () => { throw new Error("denied"); } };

  it("lights on success, clears after the delay, and restarts on a second copy", async () => {
    vi.useFakeTimers();
    const seen: boolean[] = [];
    const flash = createCopyFlash((v) => seen.push(v), 1000);
    expect(await flash.copy("a", { clipboard: ok })).toBe(true);
    vi.advanceTimersByTime(600);
    await flash.copy("a", { clipboard: ok });
    vi.advanceTimersByTime(600);
    expect(seen).toEqual([true, true]);
    vi.advanceTimersByTime(400);
    expect(seen).toEqual([true, true, false]);
  });

  it("flash lights without copying, and reset puts it out and cancels the countdown", () => {
    vi.useFakeTimers();
    const seen: boolean[] = [];
    const flash = createCopyFlash((v) => seen.push(v), 1000);
    flash.flash();
    flash.reset();
    vi.advanceTimersByTime(5000);
    expect(seen).toEqual([true, false]);
  });

  it("leaves the indicator alone on failure and cancels on dispose", async () => {
    vi.useFakeTimers();
    const seen: boolean[] = [];
    const flash = createCopyFlash((v) => seen.push(v), 1000);
    expect(await flash.copy("a", { clipboard: bad, document: null })).toBe(false);
    expect(seen).toEqual([]);
    await flash.copy("a", { clipboard: ok });
    flash.dispose();
    vi.advanceTimersByTime(5000);
    expect(seen).toEqual([true]);
  });
});

describe("copyImageBlob", () => {
  it("writes a ClipboardItem for both immediate blobs and gesture-scoped promises", async () => {
    const written: unknown[] = [];
    class FakeItem {
      readonly items: Record<string, Blob | PromiseLike<Blob>>;
      constructor(items: Record<string, Blob | PromiseLike<Blob>>) {
        this.items = items;
      }
    }
    const clipboard = { write: async (items: unknown[]) => void written.push(...items) };
    const blob = new Blob(["png-bytes"], { type: "image/png" });

    expect(await copyImageBlob(blob, "image/png", { clipboard, ClipboardItemCtor: FakeItem })).toBe(true);
    expect(await copyImageBlob(Promise.resolve(blob), "image/png", { clipboard, ClipboardItemCtor: FakeItem })).toBe(true);
    expect(written).toHaveLength(2);
  });

  it("returns false cleanly when ClipboardItem or clipboard.write is missing or rejects", async () => {
    const blob = new Blob(["png"], { type: "image/png" });
    expect(await copyImageBlob(blob, "image/png", { clipboard: null, ClipboardItemCtor: null })).toBe(false);
    expect(await copyImageBlob(Promise.reject(new Error("raster fail")), "image/png", { clipboard: null, ClipboardItemCtor: null })).toBe(false);
    class FakeItem {
      constructor(public items: Record<string, Blob | PromiseLike<Blob>>) {}
    }
    expect(
      await copyImageBlob(blob, "image/png", {
        clipboard: { write: async () => Promise.reject(new Error("denied")) },
        ClipboardItemCtor: FakeItem,
      }),
    ).toBe(false);
  });

  it("falls back to resolving the blob when ClipboardItem rejects Promise entries", async () => {
    const written: Array<Record<string, Blob | PromiseLike<Blob>>> = [];
    class ConcreteOnlyItem {
      readonly items: Record<string, Blob | PromiseLike<Blob>>;
      constructor(items: Record<string, Blob | PromiseLike<Blob>>) {
        for (const val of Object.values(items)) {
          if (val instanceof Promise) throw new TypeError("Promise values unsupported");
        }
        this.items = items;
      }
    }
    const clipboard = {
      write: async (items: unknown[]) => {
        for (const item of items) written.push((item as ConcreteOnlyItem).items);
      },
    };
    const blob = new Blob(["png-bytes"], { type: "image/png" });
    expect(
      await copyImageBlob(Promise.resolve(blob), "image/png", {
        clipboard,
        ClipboardItemCtor: ConcreteOnlyItem,
      }),
    ).toBe(true);
    expect(written).toHaveLength(1);
    expect(written[0]["image/png"]).toBe(blob);
  });
});
