import { describe, expect, it } from "vitest";
import { detectLanguageFromPath, tokenizeLine, tokenClass } from "./syntaxHighlight";

describe("syntaxHighlight", () => {
  it("detects languages accurately from file extensions", () => {
    expect(detectLanguageFromPath("src/App.svelte")).toBe("svelte");
    expect(detectLanguageFromPath("src/main.rs")).toBe("rust");
    expect(detectLanguageFromPath("src/lib/test.ts")).toBe("typescript");
    expect(detectLanguageFromPath("index.js")).toBe("javascript");
    expect(detectLanguageFromPath("package.json")).toBe("json");
    expect(detectLanguageFromPath("README.md")).toBe("markdown");
    expect(detectLanguageFromPath("styles.css")).toBe("css");
    expect(detectLanguageFromPath("script.py")).toBe("python");
    expect(detectLanguageFromPath("server.go")).toBe("go");
    expect(detectLanguageFromPath("query.sql")).toBe("sql");
    expect(detectLanguageFromPath("Dockerfile")).toBe("shell");
    expect(detectLanguageFromPath("unknown.xyz")).toBe("plaintext");
  });

  it("tokenizes typescript / javascript keywords and types", () => {
    const tokens = tokenizeLine("const count: number = 42;", "typescript");
    expect(tokens.some((t) => t.text === "const" && t.type === "keyword")).toBe(true);
    expect(tokens.some((t) => t.text === "number" && t.type === "type")).toBe(true);
    expect(tokens.some((t) => t.text === "42" && t.type === "number")).toBe(true);
  });

  it("tokenizes rust fn, types, and comments", () => {
    const tokens = tokenizeLine("pub fn calculate(val: u32) -> Result<String, Error> { // compute", "rust");
    expect(tokens.some((t) => t.text === "pub" && t.type === "keyword")).toBe(true);
    expect(tokens.some((t) => t.text === "fn" && t.type === "keyword")).toBe(true);
    expect(tokens.some((t) => t.text === "u32" && t.type === "type")).toBe(true);
    expect(tokens.some((t) => t.text === "Result" && t.type === "type")).toBe(true);
    expect(tokens.some((t) => t.text.includes("// compute") && t.type === "comment")).toBe(true);
  });

  it("tokenizes json keys and values", () => {
    const tokens = tokenizeLine('  "version": "1.0.0",', "json");
    expect(tokens.some((t) => t.text === '"version"' && t.type === "property")).toBe(true);
    expect(tokens.some((t) => t.text === '"1.0.0"' && t.type === "string")).toBe(true);
  });

  it("handles empty line and plain text cleanly", () => {
    expect(tokenizeLine("", "typescript")).toEqual([]);
    expect(tokenizeLine("hello world", "plaintext")).toEqual([{ text: "hello world", type: "text" }]);
  });

  it("returns appropriate classes for token types", () => {
    expect(tokenClass("keyword")).toContain("text-purple-400");
    expect(tokenClass("type")).toContain("text-cyan-400");
    expect(tokenClass("string")).toContain("text-emerald-400");
    expect(tokenClass("comment")).toContain("italic");
  });
});
