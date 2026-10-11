import { afterEach, describe, expect, it, vi } from "vitest";
import { copyText, createCopyFlash, type WebClipboard } from "./clipboard";

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
