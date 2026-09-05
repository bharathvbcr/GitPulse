import { describe, expect, it } from "vitest";
import {
  carryPrefix,
  createCarryIndex,
  tokenizeLine,
  tokenizeLineWithCarry,
  type SupportedLanguage,
  type SyntaxToken,
} from "./syntaxHighlight";

/** Types of the tokens covering `text`, in order. */
function typesOf(tokens: SyntaxToken[]): string[] {
  return tokens.map((token) => token.type);
}

/** Tokenizes a whole snippet the way a file viewer must: threading carry. */
function tokenizeFile(source: string, language: SupportedLanguage): SyntaxToken[][] {
  const lines = source.split("\n");
  const carries = carryPrefix(lines, language);
  return lines.map((line, i) => tokenizeLineWithCarry(line, language, carries[i]).tokens);
}

describe("a block comment stays a comment on every line it covers", () => {
  it("colours the body of a C-style block, not just its opening line", () => {
    const rows = tokenizeFile(
      ["/*", " * const answer = 42;", " */", "const real = 1;"].join("\n"),
      "typescript",
    );
    expect(typesOf(rows[0])).toEqual(["comment"]);
    // The regression: this line was re-read as live code, so `const` came back
    // as a keyword inside a comment.
    expect(typesOf(rows[1])).toEqual(["comment"]);
    expect(typesOf(rows[2])).toEqual(["comment"]);
    expect(rows[3].some((t) => t.type === "keyword")).toBe(true);
  });

  it("keeps the comment open across a blank line", () => {
    // A blank line neither opens nor closes anything; returning a clean carry
    // for it ended the comment at the first empty line inside it.
    const rows = tokenizeFile("/*\n\nstill inside\n*/\nlet x = 1;", "typescript");
    expect(typesOf(rows[2])).toEqual(["comment"]);
    expect(rows[4].some((t) => t.type === "keyword")).toBe(true);
  });

  it("treats a stray terminator as ordinary source, not as a comment", () => {
    // `*/` with nothing open is an operator pair, which is what it was before
    // the carry existed. What must NOT happen is the rest of the line being
    // swallowed as a comment because a terminator was seen.
    const [row] = tokenizeFile("*/ const after = 1;", "typescript").slice(0, 1);
    expect(row.some((t) => t.type === "comment")).toBe(false);
    expect(row.some((t) => t.type === "keyword")).toBe(true);
  });

  it("resumes code on the same line a block closes", () => {
    const rows = tokenizeFile("/* open\nclosed */ const after = 1;", "typescript");
    expect(rows[1][0]).toEqual({ text: "closed */", type: "comment" });
    expect(rows[1].some((t) => t.type === "keyword")).toBe(true);
  });

  it("carries an unterminated HTML comment", () => {
    const rows = tokenizeFile("<!--\n<div>hidden</div>\n-->", "html");
    expect(typesOf(rows[1])).toEqual(["comment"]);
  });
});

describe("multi-line strings hide the code and markers inside them", () => {
  it("carries a Python docstring", () => {
    const rows = tokenizeFile('"""\ndef not_a_function():\n"""\ndef real(): pass', "python");
    expect(typesOf(rows[1])).toEqual(["string"]);
    expect(rows[3].some((t) => t.type === "keyword")).toBe(true);
  });

  it("carries a single-quoted Python docstring too", () => {
    const rows = tokenizeFile("'''\nnot code\n'''", "python");
    expect(typesOf(rows[1])).toEqual(["string"]);
  });

  it("carries a Go raw string, including comment markers inside it", () => {
    const rows = tokenizeFile("var s = `\n// not a comment\n`", "go");
    expect(typesOf(rows[1])).toEqual(["string"]);
  });

  it("carries a template literal", () => {
    const rows = tokenizeFile("const t = `\nplain text // not a comment\n`;", "typescript");
    expect(typesOf(rows[1])).toEqual(["string"]);
  });

  it("does not treat a block comment marker as one inside a raw string", () => {
    const rows = tokenizeFile("s := `\n/* not a comment */\n`", "go");
    expect(typesOf(rows[1])).toEqual(["string"]);
  });
});

describe("languages without block comments are unaffected", () => {
  it("does not open a block comment on a shell division-looking token", () => {
    const rows = tokenizeFile("echo a/*\necho b", "shell");
    expect(rows[1].some((t) => t.type === "comment")).toBe(false);
  });
});

describe("createCarryIndex", () => {
  const lines = ["/*", "a", "b", "*/", "const x = 1;"];

  it("agrees with a full prefix pass at every index", () => {
    const eager = carryPrefix(lines, "typescript");
    const lazy = createCarryIndex(lines, "typescript");
    for (let i = 0; i < lines.length; i += 1) {
      expect(lazy(i)).toEqual(eager[i]);
    }
  });

  it("answers a deep index without being walked to first", () => {
    // A virtualized window starts wherever the user scrolled; asking for line
    // 4 must give the same answer as arriving there one line at a time.
    const jumped = createCarryIndex(lines, "typescript")(4);
    expect(jumped).toBeNull();
    const inside = createCarryIndex(lines, "typescript")(2);
    expect(inside).toEqual({ kind: "block", close: "*/", type: "comment" });
  });

  it("clamps out-of-range and negative indices instead of looping", () => {
    const index = createCarryIndex(lines, "typescript");
    expect(index(-5)).toBeNull();
    expect(index(9_999)).toBeNull();
  });

  it("is stable when the same index is asked for twice", () => {
    const index = createCarryIndex(lines, "typescript");
    expect(index(2)).toEqual(index(2));
  });
});

describe("the carry-free entry point still exists for single lines", () => {
  it("behaves exactly as before for a line with no context", () => {
    expect(tokenizeLine("const x = 1;", "typescript")).toEqual(
      tokenizeLineWithCarry("const x = 1;", "typescript", null).tokens,
    );
  });
});
