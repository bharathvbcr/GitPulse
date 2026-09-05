import { describe, expect, it } from "vitest";
import {
  buildOutline,
  churnSummary,
  hunkAt,
  languagePathForLine,
  outlineLanguagePath,
  outlineTitle,
  sectionAt,
  sectionStatus,
} from "./outline";
import { parseUnifiedDiff } from "./wordDiff";

const outlineOf = (raw: string) => buildOutline(parseUnifiedDiff(raw));

const twoFiles = [
  "diff --git a/src/a.ts b/src/a.ts",
  "index 111..222 100644",
  "--- a/src/a.ts",
  "+++ b/src/a.ts",
  "@@ -1,3 +1,4 @@ export function a()",
  " keep",
  "-old",
  "+new",
  "+extra",
  "diff --git a/docs/b.md b/docs/b.md",
  "--- a/docs/b.md",
  "+++ b/docs/b.md",
  "@@ -10,2 +10,1 @@",
  "-gone",
  "",
].join("\n");

describe("buildOutline sections", () => {
  it("finds one section per file with its own churn", () => {
    const outline = outlineOf(twoFiles);
    expect(outline.files.map((file) => [file.path, file.additions, file.deletions])).toEqual([
      ["src/a.ts", 2, 1],
      ["docs/b.md", 0, 1],
    ]);
    expect(outline.additions).toBe(2);
    expect(outline.deletions).toBe(2);
    expect(outline.headerless).toBe(false);
  });

  it("splits a hunk header from the section heading git appends", () => {
    const [file] = outlineOf(twoFiles).files;
    expect(file.hunks).toHaveLength(1);
    expect(file.hunks[0].header).toBe("@@ -1,3 +1,4 @@");
    expect(file.hunks[0].heading).toBe("export function a()");
    expect(file.hunks[0].additions).toBe(2);
    expect(file.hunks[0].deletions).toBe(1);
  });

  it("bounds each section so a caller can slice it without a lookahead", () => {
    const lines = parseUnifiedDiff(twoFiles);
    const outline = buildOutline(lines);
    expect(outline.files[0].end).toBe(outline.files[1].index);
    expect(outline.files[1].end).toBe(lines.length);
  });

  it("names a created file from the new side and marks it created", () => {
    const outline = outlineOf(
      [
        "diff --git a/new.ts b/new.ts",
        "new file mode 100644",
        "--- /dev/null",
        "+++ b/new.ts",
        "@@ -0,0 +1,2 @@",
        "+one",
        "+two",
        "",
      ].join("\n"),
    );
    expect(outline.files[0].path).toBe("new.ts");
    expect(outline.files[0].created).toBe(true);
    expect(sectionStatus(outline.files[0])).toBe("A");
  });

  it("names a deleted file from the old side and marks it deleted", () => {
    const outline = outlineOf(
      [
        "diff --git a/gone.ts b/gone.ts",
        "deleted file mode 100644",
        "--- a/gone.ts",
        "+++ /dev/null",
        "@@ -1,2 +0,0 @@",
        "-one",
        "",
      ].join("\n"),
    );
    expect(outline.files[0].path).toBe("gone.ts");
    expect(outline.files[0].deleted).toBe(true);
    expect(sectionStatus(outline.files[0])).toBe("D");
  });

  it("records both sides of a rename", () => {
    const outline = outlineOf(
      [
        "diff --git a/old/name.ts b/new/name.ts",
        "similarity index 96%",
        "rename from old/name.ts",
        "rename to new/name.ts",
        "--- a/old/name.ts",
        "+++ b/new/name.ts",
        "@@ -1 +1 @@",
        "-a",
        "+b",
        "",
      ].join("\n"),
    );
    expect(outline.files[0].path).toBe("new/name.ts");
    expect(outline.files[0].oldPath).toBe("old/name.ts");
    expect(sectionStatus(outline.files[0])).toBe("R");
  });

  it("decodes git's quoted, octal-escaped path form", () => {
    // Git escapes every non-ASCII BYTE: "é" is \303\251, and decoding the two
    // escapes separately yields "Ã©". A binary file has no ---/+++ pair, so
    // the `diff --git` line is the only path there is.
    const outline = outlineOf(
      [
        'diff --git "a/sp ace/\\303\\251.png" "b/sp ace/\\303\\251.png"',
        "Binary files a/x and b/x differ",
        "",
      ].join("\n"),
    );
    expect(outline.files[0].path).toBe("sp ace/é.png");
  });

  it("prefers the ---/+++ pair over the ambiguous diff --git line", () => {
    // `a/foo b/bar.txt b/foo b/bar.txt` has three ` b/`-shaped splits.
    const outline = outlineOf(
      [
        "diff --git a/foo b/bar.txt b/foo b/bar.txt",
        "--- a/foo b/bar.txt",
        "+++ b/foo b/bar.txt",
        "@@ -1 +1 @@",
        "+x",
        "",
      ].join("\n"),
    );
    expect(outline.files[0].path).toBe("foo b/bar.txt");
  });

  it("splits a space-bearing bare header at the point where both sides agree", () => {
    const outline = outlineOf(
      ["diff --git a/foo b/bar.txt b/foo b/bar.txt", "Binary files a/x and b/x differ", ""].join("\n"),
    );
    expect(outline.files[0].path).toBe("foo b/bar.txt");
  });

  it("drops the timestamp git appends to a ---/+++ path after a tab", () => {
    const outline = outlineOf(
      ["--- a/src/x.ts\t2024-01-01 10:00:00", "+++ b/src/x.ts\t2024-01-02 10:00:00", "@@ -1 +1 @@", "+a", ""].join("\n"),
    );
    expect(outline.files[0].path).toBe("src/x.ts");
  });

  it("marks a binary section without claiming line counts", () => {
    const outline = outlineOf(
      [
        "diff --git a/logo.png b/logo.png",
        "Binary files a/logo.png and b/logo.png differ",
        "",
      ].join("\n"),
    );
    expect(outline.files[0].binary).toBe(true);
    expect(outline.files[0].additions).toBe(0);
  });

  it("says a header-less stream is header-less rather than inventing a file", () => {
    const outline = outlineOf(["@@ -1,2 +1,2 @@", "-a", "+b", ""].join("\n"));
    expect(outline.headerless).toBe(true);
    expect(outline.files).toHaveLength(1);
    expect(outline.files[0].path).toBe("");
  });

  it("returns the empty outline for no lines", () => {
    expect(buildOutline([])).toEqual({
      files: [],
      additions: 0,
      deletions: 0,
      headerless: false,
    });
  });

  it("keeps `git show` prose out of the file it precedes", () => {
    // `git show` writes the commit message above the first `diff --git`.
    const outline = outlineOf(
      [
        "commit 1234",
        "Author: Ada",
        "",
        "    the message",
        "",
        "diff --git a/x.ts b/x.ts",
        "--- a/x.ts",
        "+++ b/x.ts",
        "@@ -1 +1 @@",
        "+a",
        "",
      ].join("\n"),
    );
    expect(outline.files).toHaveLength(2);
    expect(outline.files[0].path).toBe("");
    expect(outline.files[1].path).toBe("x.ts");
    expect(outline.headerless).toBe(true);
  });
});

describe("sectionAt / hunkAt", () => {
  const lines = parseUnifiedDiff(twoFiles);
  const outline = buildOutline(lines);

  it("finds the file a line belongs to", () => {
    expect(sectionAt(outline, 0)?.path).toBe("src/a.ts");
    expect(sectionAt(outline, outline.files[1].index)?.path).toBe("docs/b.md");
    expect(sectionAt(outline, lines.length - 1)?.path).toBe("docs/b.md");
  });

  it("answers with the first section for a line above every header", () => {
    expect(sectionAt(outline, -5)?.path).toBe("src/a.ts");
  });

  it("returns null when the outline has no sections", () => {
    expect(sectionAt(buildOutline([]), 0)).toBeNull();
    expect(hunkAt(null, 0)).toBeNull();
  });

  it("finds the hunk a line sits in, and none above the first", () => {
    const section = outline.files[0];
    expect(hunkAt(section, section.hunks[0].index)?.header).toBe("@@ -1,3 +1,4 @@");
    expect(hunkAt(section, section.index)).toBeNull();
  });

  it("agrees with a linear scan over a many-section diff", () => {
    const many = parseUnifiedDiff(
      Array.from({ length: 60 }, (_, i) =>
        [
          `diff --git a/f${i}.ts b/f${i}.ts`,
          `--- a/f${i}.ts`,
          `+++ b/f${i}.ts`,
          "@@ -1 +1 @@",
          `+line ${i}`,
        ].join("\n"),
      ).join("\n") + "\n",
    );
    const wide = buildOutline(many);
    for (let i = 0; i < many.length; i += 1) {
      const linear = [...wide.files].reverse().find((file) => file.index <= i) ?? wide.files[0];
      expect(sectionAt(wide, i)).toBe(linear);
    }
  });
});

describe("outline titles", () => {
  it("names the file when a diff covers exactly one", () => {
    const outline = outlineOf(
      ["diff --git a/only.ts b/only.ts", "--- a/only.ts", "+++ b/only.ts", "@@ -1 +1 @@", "+a", ""].join("\n"),
    );
    expect(outlineTitle(outline, "fallback")).toBe("only.ts");
    expect(outlineLanguagePath(outline)).toBe("only.ts");
  });

  it("counts files instead of naming one of many", () => {
    // The regression this exists for: the header printed the last-clicked
    // path over a body showing a different file entirely.
    const outline = outlineOf(twoFiles);
    expect(outlineTitle(outline, "fallback")).toBe("2 files");
    expect(outlineLanguagePath(outline)).toBeNull();
  });

  it("falls back rather than naming a file it only inferred", () => {
    const outline = outlineOf(["@@ -1 +1 @@", "+a", ""].join("\n"));
    expect(outlineTitle(outline, "Working tree")).toBe("Working tree");
    expect(outlineLanguagePath(outline)).toBeNull();
  });

  it("falls back for an empty diff", () => {
    expect(outlineTitle(buildOutline([]), "Nothing")).toBe("Nothing");
  });

  it("uses the caller's fallback only when the diff names no file at all", () => {
    // A headerless fragment still belongs to whatever the reader opened.
    const headerless = outlineOf(["@@ -1 +1 @@", "+a", ""].join("\n"));
    expect(outlineLanguagePath(headerless, "opened.ts")).toBe("opened.ts");
    expect(outlineLanguagePath(buildOutline([]), "opened.ts")).toBe("opened.ts");
  });

  it("refuses the fallback for a diff that names several files", () => {
    // Handing it back here is how a 200-file commit wore a JSON badge,
    // because the reader's last click happened to land on a `.json`.
    expect(outlineLanguagePath(outlineOf(twoFiles), "last-clicked.json")).toBeNull();
  });
});

describe("languagePathForLine", () => {
  it("answers with the file each line actually belongs to", () => {
    const lines = parseUnifiedDiff(twoFiles);
    const outline = buildOutline(lines);
    const [first, second] = outline.files;
    expect(languagePathForLine(outline, first.index)).toBe(first.path);
    expect(languagePathForLine(outline, second.index)).toBe(second.path);
    expect(languagePathForLine(outline, lines.length - 1)).toBe(second.path);
  });

  it("never lets one file's language leak onto another's rows", () => {
    // One global language tokenized every file in a commit with whichever
    // language the header named — Rust coloured by the JSON tokenizer.
    const lines = parseUnifiedDiff(twoFiles);
    const outline = buildOutline(lines);
    const answers = new Set(
      Array.from({ length: lines.length }, (_, i) => languagePathForLine(outline, i, "wrong.json")),
    );
    expect(answers).toEqual(new Set(outline.files.map((file) => file.path)));
    expect(answers.has("wrong.json")).toBe(false);
  });

  it("falls back for a line that belongs to no named section", () => {
    const outline = outlineOf(["@@ -1 +1 @@", "+a", ""].join("\n"));
    expect(languagePathForLine(outline, 0, "opened.ts")).toBe("opened.ts");
    expect(languagePathForLine(outline, 0)).toBeNull();
  });

  it("clamps an out-of-range line rather than throwing", () => {
    const outline = outlineOf(twoFiles);
    expect(languagePathForLine(outline, -20)).toBe(outline.files[0].path);
    expect(languagePathForLine(outline, 99_999)).toBe(outline.files[1].path);
    expect(languagePathForLine(buildOutline([]), 3, "only.ts")).toBe("only.ts");
  });
});

describe("churnSummary", () => {
  it("formats both sides with thousands separators", () => {
    expect(churnSummary(1234, 7)).toBe("+1,234 −7");
  });

  it("is empty when nothing changed, so an unchanged file adds no chrome", () => {
    expect(churnSummary(0, 0)).toBe("");
  });

  it("shows a zero side rather than hiding it", () => {
    expect(churnSummary(3, 0)).toBe("+3 −0");
  });
});
