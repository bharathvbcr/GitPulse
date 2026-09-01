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

/** Concatenating a line's tokens must always reproduce the line exactly. */
function assertLossless(line: string, language: Parameters<typeof tokenizeLine>[1]): void {
  const tokens = tokenizeLine(line, language);
  expect(tokens.map((t) => t.text).join("")).toBe(line);
}

function typeOf(line: string, language: Parameters<typeof tokenizeLine>[1], text: string) {
  return tokenizeLine(line, language).find((t) => t.text === text)?.type;
}

describe("syntaxHighlight — diff lines", () => {
  it("colours added, removed and hunk lines distinctly", () => {
    expect(tokenizeLine("+added line", "diff")[0].type).toBe("attribute");
    expect(tokenizeLine("-removed line", "diff")[0].type).toBe("operator");
    expect(tokenizeLine("@@ -1,4 +1,6 @@", "diff")[0].type).toBe("type");
    expect(tokenizeLine(" context line", "diff")[0].type).toBe("text");
  });

  it("keeps the whole diff line as one token so nothing is dropped", () => {
    for (const line of ["+a", "-b", "@@ x @@", " c", ""]) {
      assertLossless(line, "diff");
    }
  });
});

describe("syntaxHighlight — comments", () => {
  it("treats a closed HTML comment as a comment and resumes after it", () => {
    const line = '<!-- note --><div class="x">';
    const tokens = tokenizeLine(line, "html");
    expect(tokens[0]).toEqual({ text: "<!-- note -->", type: "comment" });
    assertLossless(line, "html");
  });

  it("treats an unterminated HTML comment as comment to end of line", () => {
    const line = "<!-- unterminated";
    expect(tokenizeLine(line, "html")).toEqual([{ text: line, type: "comment" }]);
  });

  it("treats a closed block comment as a comment and resumes after it", () => {
    const line = "const a = /* why */ 1;";
    expect(typeOf(line, "typescript", "/* why */")).toBe("comment");
    assertLossless(line, "typescript");
  });

  it("treats an unterminated block comment as comment to end of line", () => {
    const line = "/* unterminated";
    expect(tokenizeLine(line, "typescript")).toEqual([{ text: line, type: "comment" }]);
  });
});

describe("syntaxHighlight — string literals", () => {
  it("handles each quote style", () => {
    for (const line of ['const a = "x";', "const a = 'x';", "const a = `x`;"]) {
      const string = tokenizeLine(line, "typescript").find((t) => t.type === "string");
      expect(string, line).toBeDefined();
      assertLossless(line, "typescript");
    }
  });

  it("does not end a string at an escaped quote", () => {
    const line = 'const a = "he said \\"hi\\" ok";';
    const string = tokenizeLine(line, "typescript").find((t) => t.type === "string");
    // The escaped quotes are inside the literal, so it does not stop at the first one.
    expect(string?.text.startsWith('"he said')).toBe(true);
    assertLossless(line, "typescript");
  });

  it("treats an unterminated string as running to end of line", () => {
    const line = 'const a = "never closed';
    const string = tokenizeLine(line, "typescript").find((t) => t.type === "string");
    expect(string?.text).toBe('"never closed');
    assertLossless(line, "typescript");
  });
});

describe("syntaxHighlight — numbers", () => {
  it("recognises integers, floats and leading-dot floats", () => {
    expect(typeOf("let a = 42;", "typescript", "42")).toBe("number");
    expect(typeOf("let a = 3.14;", "typescript", "3.14")).toBe("number");
    const leading = tokenizeLine("let a = .5;", "typescript").find((t) => t.type === "number");
    expect(leading?.text).toBe(".5");
  });
});

describe("syntaxHighlight — language-specific keywords", () => {
  it("recognises keywords per language and not across languages", () => {
    expect(typeOf("fn main() {}", "rust", "fn")).toBe("keyword");
    expect(typeOf("def main():", "python", "def")).toBe("keyword");
    expect(typeOf("func main() {}", "go", "func")).toBe("keyword");
    // SQL keywords are matched case-insensitively.
    expect(typeOf("select * from t", "sql", "select")).toBe("keyword");
    expect(typeOf("SELECT * FROM t", "sql", "SELECT")).toBe("keyword");
    // A Rust keyword is not a TypeScript keyword.
    expect(typeOf("fn main() {}", "typescript", "fn")).not.toBe("keyword");
  });

  it("keeps a Svelte rune as one token, typed by how it is used", () => {
    // A called rune is reported as a function, because the call check runs
    // before the rune check. A bare rune reference takes the keyword branch.
    expect(typeOf("let a = $state(0);", "svelte", "$state")).toBe("function");
    expect(typeOf("let a = $props;", "svelte", "$props")).toBe("keyword");
  });

  it("marks at-rules as attributes where '@' starts a word", () => {
    // '@' only begins a word in svelte and css; elsewhere it is an operator
    // and the following identifier is tokenized separately.
    expect(typeOf("@tailwind base;", "css", "@tailwind")).toBe("attribute");
    expect(typeOf("@apply px-3;", "css", "@apply")).toBe("attribute");
    // Followed by '(' the call check wins, same ordering as a called rune.
    expect(typeOf("@media (min-width: 40rem) {", "css", "@media")).toBe("function");
    expect(typeOf("@Component class X {}", "typescript", "@")).toBe("operator");
    expect(typeOf("@Component class X {}", "typescript", "Component")).toBe("type");
  });

  it("terminates on an at-rule instead of looping forever", () => {
    // Regression: '@' could start a word but was not a word character, so the
    // scanner never advanced — an empty word, `i` unchanged, and a token array
    // that grew until the process ran out of memory. Every CSS file with
    // `@media`, `@import` or `@tailwind` hung the viewer, including this
    // repository's own app.css.
    for (const line of [
      "@tailwind base;",
      "@media (min-width: 40rem) {",
      "@import url(x);",
      "@",
      "@ ",
      "@@",
      "@-",
    ]) {
      const started = Date.now();
      const tokens = tokenizeLine(line, "css");
      expect(Date.now() - started, `${line} took too long`).toBeLessThan(1000);
      expect(tokens.length, `${line} produced no tokens`).toBeGreaterThan(0);
      expect(tokens.map((t) => t.text).join("")).toBe(line);
    }
    // Svelte allows '@' as a word start too, so it had the same defect.
    expect(tokenizeLine("@const x", "svelte").map((t) => t.text).join("")).toBe("@const x");
  });

  it("marks yaml and css keys as properties", () => {
    // The scanner treats ':' as a word character, so the key token carries the
    // colon. Before this was accounted for, the property branch tested the
    // character *after* the word and could never match — every YAML key and
    // CSS property rendered as plain text.
    expect(typeOf("name: value", "yaml", "name:")).toBe("property");
    expect(typeOf("color: red;", "css", "color:")).toBe("property");
    // A word without a colon is not a property.
    expect(typeOf("name value", "yaml", "name")).not.toBe("property");
  });
});

describe("syntaxHighlight — json", () => {
  it("distinguishes a key from a string value", () => {
    const line = '{"key": "value"}';
    expect(typeOf(line, "json", '"key"')).toBe("property");
    expect(typeOf(line, "json", '"value"')).toBe("string");
    assertLossless(line, "json");
  });

  it("tolerates whitespace between a key and its colon", () => {
    expect(typeOf('{"key"  : 1}', "json", '"key"')).toBe("property");
  });

  it("recognises literals and negative numbers", () => {
    expect(typeOf('{"a": true}', "json", "true")).toBe("keyword");
    expect(typeOf('{"a": null}', "json", "null")).toBe("keyword");
    expect(typeOf('{"a": -12.5}', "json", "-12.5")).toBe("number");
  });

  it("does not end a json string at an escaped quote", () => {
    const line = '{"a": "x\\"y"}';
    assertLossless(line, "json");
  });
});

describe("syntaxHighlight — token classes", () => {
  it("gives every token type a distinct, non-empty class", () => {
    const types = [
      "keyword", "type", "string", "comment", "number", "function", "operator",
      "tag", "attribute", "property", "punctuation", "regex", "variable", "text",
    ] as const;
    const classes = types.map((t) => tokenClass(t));
    for (const [index, cls] of classes.entries()) {
      expect(cls, types[index]).toBeTruthy();
    }
    // "text" is the fallback; every other type is visually distinguishable.
    const distinct = new Set(classes.slice(0, -1));
    expect(distinct.size).toBe(types.length - 1);
  });
});

describe("syntaxHighlight — losslessness", () => {
  it("never drops or invents characters, in any language", () => {
    const samples: Array<[string, Parameters<typeof tokenizeLine>[1]]> = [
      ["", "typescript"],
      ["   ", "typescript"],
      ["const x = { a: [1, 2], b: `t${x}` }; // done", "typescript"],
      ["fn main() -> Result<(), String> { Ok(()) }", "rust"],
      ["SELECT a, b FROM t WHERE c = 'x';", "sql"],
      ["<div class=\'a\' data-x=\'1\'>text</div>", "html"],
      ["key: [1, 2] # comment", "yaml"],
      ["héllo = '日本語' # 👩‍👩‍👧‍👦", "toml"],
      ["a".repeat(5000), "typescript"],
    ];
    for (const [line, language] of samples) {
      assertLossless(line, language);
    }
  });
});
