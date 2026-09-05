import { describe, expect, it } from "vitest";
import {
  decodeQuotedGitPath,
  gitHeaderSides,
  parseHeaderPath,
  stripSidePrefix,
  unquoteGitPath,
} from "./gitPaths";

/**
 * This parser decides which file a hunk belongs to, and three modules used to
 * answer that question differently. Its escape handling and its four
 * quoted/bare header shapes were only ever exercised indirectly, through the
 * patch builder — so the branches that decide where a patch APPLIES were the
 * least-tested code in the diff page.
 */

describe("decodeQuotedGitPath", () => {
  it("reassembles octal escapes as bytes before decoding UTF-8", () => {
    // Decoding each escape on its own is what turned `é` into `Ã©`, and a
    // mangled path is a file the UI cannot line up with its patch.
    expect(decodeQuotedGitPath("sp ace/\\303\\251.ts")).toBe("sp ace/é.ts");
    expect(decodeQuotedGitPath("\\346\\227\\245\\346\\234\\254.md")).toBe("日本.md");
    expect(decodeQuotedGitPath("\\360\\237\\232\\200.txt")).toBe("🚀.txt");
  });

  it("decodes the C escapes git emits for control characters", () => {
    expect(decodeQuotedGitPath("a\\nb")).toBe("a\nb");
    expect(decodeQuotedGitPath("a\\tb")).toBe("a\tb");
    expect(decodeQuotedGitPath("a\\rb")).toBe("a\rb");
  });

  it("decodes an escaped quote and an escaped backslash", () => {
    expect(decodeQuotedGitPath('say \\"hi\\".txt')).toBe('say "hi".txt');
    expect(decodeQuotedGitPath("back\\\\slash.txt")).toBe("back\\slash.txt");
  });

  it("passes an unknown escape through as the character itself", () => {
    expect(decodeQuotedGitPath("a\\zb")).toBe("azb");
  });

  it("stops at a trailing backslash rather than reading past the end", () => {
    expect(decodeQuotedGitPath("trailing\\")).toBe("trailing");
  });

  it("stops an octal run at three digits", () => {
    // `\\1011` is byte 0o101 ("A") followed by a literal "1", not a 4-digit
    // value silently truncated to something else.
    expect(decodeQuotedGitPath("\\1011")).toBe("A1");
  });

  it("keeps bytes that are not valid UTF-8 rather than losing the path", () => {
    // A lone continuation byte cannot be decoded; replacing the whole path
    // with U+FFFD would lose the only handle the caller has on the file.
    expect(decodeQuotedGitPath("bad\\377end")).toBe("badÿend");
  });

  it("returns an empty string for empty input", () => {
    expect(decodeQuotedGitPath("")).toBe("");
  });
});

describe("unquoteGitPath", () => {
  it("unwraps and decodes a quoted path", () => {
    expect(unquoteGitPath('"sp ace/\\303\\251.ts"')).toBe("sp ace/é.ts");
  });

  it("leaves a bare path exactly as it is", () => {
    expect(unquoteGitPath("src/a.ts")).toBe("src/a.ts");
    // Not quoted, so the backslash is a real character, not an escape.
    expect(unquoteGitPath("weird\\303name")).toBe("weird\\303name");
  });

  it("leaves a lone quote alone rather than reading it as a wrapper", () => {
    expect(unquoteGitPath('"')).toBe('"');
  });

  it("treats a path that only starts or only ends with a quote as bare", () => {
    expect(unquoteGitPath('"open')).toBe('"open');
    expect(unquoteGitPath('close"')).toBe('close"');
  });
});

describe("parseHeaderPath", () => {
  it("reads the path from a --- / +++ line", () => {
    expect(parseHeaderPath("--- a/src/x.ts")).toBe("a/src/x.ts");
    expect(parseHeaderPath("+++ b/src/x.ts")).toBe("b/src/x.ts");
  });

  it("drops the tab-separated timestamp git appends in some modes", () => {
    expect(parseHeaderPath("--- a/x.ts\t2026-09-04 12:00:00.000000000 +0530")).toBe("a/x.ts");
  });

  it("drops the stray carriage return a CRLF diff leaves behind", () => {
    expect(parseHeaderPath("+++ b/x.ts\r")).toBe("b/x.ts");
  });

  it("decodes a quoted header path", () => {
    expect(parseHeaderPath('+++ "b/sp ace/\\303\\251.ts"')).toBe("b/sp ace/é.ts");
  });

  it("keeps the side prefix, because /dev/null must survive unprefixed", () => {
    // Stripping here would leave the caller unable to tell a created file
    // from one named `dev/null`.
    expect(parseHeaderPath("--- /dev/null")).toBe("/dev/null");
  });
});

describe("stripSidePrefix", () => {
  it("strips one leading a/ or b/ when no side is named", () => {
    expect(stripSidePrefix("a/src/x.ts")).toBe("src/x.ts");
    expect(stripSidePrefix("b/src/x.ts")).toBe("src/x.ts");
  });

  it("strips only the side it was told to strip", () => {
    expect(stripSidePrefix("a/x.ts", "a")).toBe("x.ts");
    expect(stripSidePrefix("b/x.ts", "a")).toBe("b/x.ts");
    expect(stripSidePrefix("a/x.ts", "b")).toBe("a/x.ts");
  });

  it("strips only the LEADING occurrence", () => {
    // A repository with a real top-level `a/` directory has to keep working.
    expect(stripSidePrefix("a/a/nested.ts")).toBe("a/nested.ts");
    expect(stripSidePrefix("src/a/x.ts")).toBe("src/a/x.ts");
  });

  it("passes /dev/null through verbatim, with or without a side", () => {
    expect(stripSidePrefix("/dev/null")).toBe("/dev/null");
    expect(stripSidePrefix("/dev/null", "a")).toBe("/dev/null");
  });

  it("leaves a path with no prefix alone", () => {
    expect(stripSidePrefix("src/x.ts")).toBe("src/x.ts");
    expect(stripSidePrefix("")).toBe("");
  });
});

describe("gitHeaderSides", () => {
  it("splits the plain bare/bare case", () => {
    expect(gitHeaderSides("diff --git a/src/x.ts b/src/x.ts")).toEqual(["src/x.ts", "src/x.ts"]);
  });

  it("prefers the split whose two sides name the same file", () => {
    // Git does not quote a path merely for containing spaces, so this line
    // has three ` b/`-shaped splits and one right answer.
    expect(gitHeaderSides("diff --git a/foo b/bar.txt b/foo b/bar.txt")).toEqual([
      "foo b/bar.txt",
      "foo b/bar.txt",
    ]);
  });

  it("falls back to the first split for a rename, which git repeats below", () => {
    expect(gitHeaderSides("diff --git a/old.ts b/new.ts")).toEqual(["old.ts", "new.ts"]);
  });

  it("handles a quoted left side with a bare right side", () => {
    expect(gitHeaderSides('diff --git "a/sp ace/\\303\\251.ts" b/plain.ts')).toEqual([
      "sp ace/é.ts",
      "plain.ts",
    ]);
  });

  it("handles a bare left side with a quoted right side", () => {
    expect(gitHeaderSides('diff --git a/plain.ts "b/sp ace/\\303\\251.ts"')).toEqual([
      "plain.ts",
      "sp ace/é.ts",
    ]);
  });

  it("handles both sides quoted", () => {
    expect(gitHeaderSides('diff --git "a/\\303\\251.ts" "b/\\303\\251.ts"')).toEqual([
      "é.ts",
      "é.ts",
    ]);
  });

  it("survives a quoted side whose closing quote never arrives", () => {
    // Truncated input must not throw. It also must not silently produce a
    // clean-looking left path: the unclosed quote stays visible, so a caller
    // comparing this against `---`/`+++` sees the disagreement instead of
    // targeting a file that was never named.
    const sides = gitHeaderSides('diff --git "a/unterminated b/x.ts');
    expect(sides).toEqual(['"a/unterminated', "x.ts"]);
    // Nothing to split on at all: null, not a half-parsed pair.
    expect(gitHeaderSides('diff --git "a/unterminated')).toBeNull();
  });

  it("returns null when there is nothing to split", () => {
    expect(gitHeaderSides("diff --git ")).toBeNull();
    expect(gitHeaderSides("diff --git a/only-one-side.ts")).toBeNull();
  });

  it("returns null rather than an empty left side", () => {
    // A cut at index 0 would name the empty string as a file.
    expect(gitHeaderSides("diff --git  b/x.ts")).toBeNull();
  });
});
