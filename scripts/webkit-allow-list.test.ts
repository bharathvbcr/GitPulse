import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { allowWebkitPage } from "./webkit-allow-list.mjs";

const swift = readFileSync(new URL("./webkit-regressions.swift", import.meta.url), "utf8");

function allowList(source: string): string[] {
  const list = /\[([^\]]*)\]\.contains\(url\.path\)/.exec(source)?.[1] ?? "";
  return [...list.matchAll(/"([^"]+)"/g)].map((m) => m[1]);
}

describe("allowWebkitPage", () => {
  it("appends an unlisted page to the shipped runner's allow-list, whatever its last entry is", () => {
    const before = allowList(swift);
    const patched = allowWebkitPage(swift, "/harness/terminal-scroll.html");
    expect(allowList(patched)).toEqual([...before, "/harness/terminal-scroll.html"]);
    expect(patched.replace(', "/harness/terminal-scroll.html"', "")).toBe(swift);
  });

  it("still finds the list after another harness is registered at its end", () => {
    const grown = allowWebkitPage(swift, "/harness/added-later.html");
    expect(allowList(allowWebkitPage(grown, "/harness/terminal-switch.html")).slice(-2)).toEqual([
      "/harness/added-later.html",
      "/harness/terminal-switch.html",
    ]);
  });

  it("refuses a source with no allow-list rather than running an unpatched copy", () => {
    expect(() => allowWebkitPage("let x = 1\n", "/harness/x.html")).toThrow(/0 harness allow-lists/);
  });
});
