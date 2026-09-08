import { describe, expect, it } from "vitest";
import { readBrowserVerdict } from "./browser-regressions.mjs";

describe("browser regression gate", () => {
  const html = (rows: unknown[]) => `<html data-gp-result="${encodeURIComponent(JSON.stringify({ results: rows }))}">`;
  it("requires an executed complete verdict", () => {
    expect(() => readBrowserVerdict("<html><pre>Running…</pre></html>")).toThrow();
    expect(() => readBrowserVerdict(html([]))).toThrow();
    expect(() => readBrowserVerdict(html([{ pass: true }]))).toThrow();
  });
  it("refuses a failed or malformed assertion among passing rows", () => {
    for (const row of [{ pass: false }, { pass: "true" }, null]) {
      expect(() => readBrowserVerdict(html([...Array.from({ length: 24 }, () => ({ pass: true })), row]))).toThrow();
    }
  });
  it("accepts the completed real-browser assertion set", () => {
    expect(readBrowserVerdict(html(Array.from({ length: 24 }, () => ({ pass: true }))))).toBe("24/24 browser regressions passed");
  });
});
