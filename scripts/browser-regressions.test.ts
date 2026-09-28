import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { BROWSER_HARNESSES, readBrowserVerdict } from "./browser-regressions.mjs";

describe("browser regression gate", () => {
  it("keeps the native WebKit entrypoint aligned with every supported harness", () => {
    const native = readFileSync(new URL("./webkit-regressions.swift", import.meta.url), "utf8");
    const paths = [...native.matchAll(/"\/harness\/([^"/]+)\.html"/g)].map(match => match[1]);
    expect(paths.sort()).toEqual([...BROWSER_HARNESSES].sort());
  });
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

describe("failure reporting a CI reader can see", () => {
  it("names the failing checks, crashes and unexpected commands", async () => {
    const { failedCheckNames } = await import("./browser-regressions.mjs");
    const rows = Array.from({ length: 24 }, (_, i) => ({ name: `check ${i}`, pass: i !== 3 }));
    const result = { results: rows, crashes: [{ message: "boom" }], unexpected: ["cmd_x", "cmd_x"] };
    expect(failedCheckNames(result, rows)).toEqual([
      "check 3",
      'crash: {"message":"boom"}',
      "unexpected commands: cmd_x",
    ]);
  });

  it("escapes an annotation so its text cannot end the command early", async () => {
    const { annotation } = await import("./browser-regressions.mjs");
    expect(annotation("Chrome a:b,c", "one\ntwo 50%")).toBe("::error title=Chrome a%3Ab%2Cc::one%0Atwo 50%25");
  });
});
