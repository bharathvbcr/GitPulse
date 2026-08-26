import { describe, expect, it } from "vitest";
import { parseUnifiedDiff } from "./wordDiff";
import {
  buildFilePatchForHunk,
  buildFilePatchFromLines,
  parseHunkHeaderNumbers,
  serializeSelectivePatch,
} from "./patchBuilder";

const sample = [
  "diff --git a/src/a.rs b/src/a.rs",
  "--- a/src/a.rs",
  "+++ b/src/a.rs",
  "@@ -1,4 +1,5 @@",
  " fn main() {",
  "-    old();",
  "+    new();",
  "+    extra();",
  " }",
].join("\n");

describe("parseHunkHeaderNumbers", () => {
  it("reads both sides and defaults omitted counts to 1", () => {
    expect(parseHunkHeaderNumbers("@@ -3,4 +9,2 @@ fn main()")).toEqual({
      old_start: 3,
      old_lines: 4,
      new_start: 9,
      new_lines: 2,
    });
    expect(parseHunkHeaderNumbers("@@ -1 +1 @@")).toEqual({
      old_start: 1,
      old_lines: 1,
      new_start: 1,
      new_lines: 1,
    });
  });
});

describe("buildFilePatchFromLines", () => {
  it("serializes selected add/del lines into a backend FilePatch", () => {
    const lines = parseUnifiedDiff(sample);
    const add = lines.findIndex((l) => l.content.includes("new();"));
    const extra = lines.findIndex((l) => l.content.includes("extra();"));
    const patch = buildFilePatchFromLines(lines, "src/a.rs", new Set([add, extra]));
    expect(patch).not.toBeNull();
    expect(patch?.old_path).toBe("src/a.rs");
    expect(patch?.hunks).toHaveLength(1);
    const selected = patch!.hunks[0].lines.filter((l) => l.is_selected);
    expect(selected.map((l) => l.content)).toEqual(["    new();", "    extra();"]);
    expect(selected.every((l) => l.line_type === "Addition")).toBe(true);
  });

  it("returns null when no add/del lines are selected", () => {
    const lines = parseUnifiedDiff(sample);
    expect(buildFilePatchFromLines(lines, "src/a.rs", new Set())).toBeNull();
    const ctx = lines.findIndex((l) => l.type === "ctx");
    expect(buildFilePatchFromLines(lines, "src/a.rs", new Set([ctx]))).toBeNull();
  });

  it("carries noNewline flags through as no_newline on payload rows", () => {
    const lines = parseUnifiedDiff(
      ["@@ -1,2 +1,2 @@", "-old tail", "\\ No newline at end of file", "+new tail", "\\ No newline at end of file"].join("\n")
    );
    const patch = buildFilePatchFromLines(lines, "f.txt", new Set([1, 3]));
    expect(patch).not.toBeNull();
    const [del, add] = patch!.hunks[0].lines;
    expect(del.no_newline).toBe(true);
    expect(add.no_newline).toBe(true);
  });

  it("keeps a trailing CR in content verbatim (CRLF correctness)", () => {
    // git diff renders CRLF files as body lines whose content ends in "\r"
    // before the "\n" terminator; the \r is file content, not formatting.
    const lines = parseUnifiedDiff("@@ -1,3 +1,3 @@\n-a\r\n+beta\r\n ctx\r\n");
    const delIdx = lines.findIndex((l) => l.type === "del");
    const addIdx = lines.findIndex((l) => l.type === "add");
    const patch = buildFilePatchFromLines(lines, "crlf.txt", new Set([delIdx, addIdx]));
    const rows = patch!.hunks[0].lines;
    expect(rows.map((l) => l.content)).toEqual(["a\r", "beta\r", "ctx\r"]);
    expect(rows.filter((l) => l.is_selected).map((l) => l.content)).toEqual(["a\r", "beta\r"]);
  });
});

describe("authoritative header paths", () => {
  it("prefers the parsed ---/+++ paths over the caller-supplied path", () => {
    const lines = parseUnifiedDiff(sample);
    const add = lines.findIndex((l) => l.type === "add");
    // Caller passes something stale/wrong; headers win.
    const patch = buildFilePatchFromLines(lines, "WRONG/path.rs", new Set([add]));
    expect(patch?.old_path).toBe("src/a.rs");
    expect(patch?.new_path).toBe("src/a.rs");
  });

  it("strips exactly one git side prefix so real top-level a/ dirs survive", () => {
    // A repo that genuinely contains a/ as a top-level directory: the
    // header carries a/a/real.rs and one strip must yield a/real.rs — not
    // the old caller-side ^[ab]/ rewrite that collapsed to real.rs.
    const lines = parseUnifiedDiff(
      [
        "diff --git a/a/real.rs b/a/real.rs",
        "--- a/a/real.rs",
        "+++ b/a/real.rs",
        "@@ -1 +1 @@",
        "-old",
        "+new",
      ].join("\n")
    );
    const add = lines.findIndex((l) => l.type === "add");
    const patch = buildFilePatchFromLines(lines, "a/real.rs", new Set([add]));
    expect(patch?.old_path).toBe("a/real.rs");
    expect(patch?.new_path).toBe("a/real.rs");
  });

  it("decodes quoted C-escaped unicode paths byte-wise as UTF-8", () => {
    // git quotes non-ASCII paths and escapes each UTF-8 BYTE as octal:
    // unié.txt → "uni\303\251.txt". Decoding per-escape would produce
    // "uniÃ©"; bytes must reassemble into one é.
    const raw = [
      'diff --git "a/uni\\303\\251\\360\\237\\216\\211.txt" "b/uni\\303\\251\\360\\237\\216\\211.txt"',
      '--- "a/uni\\303\\251\\360\\237\\216\\211.txt"',
      '+++ "b/uni\\303\\251\\360\\237\\216\\211.txt"',
      "@@ -1 +1 @@",
      "-alt",
      "+neu",
    ].join("\n");
    const lines = parseUnifiedDiff(raw);
    const add = lines.findIndex((l) => l.type === "add");
    const patch = buildFilePatchFromLines(lines, "fallback.txt", new Set([add]));
    expect(patch?.old_path).toBe("unié🎉.txt");
    expect(patch?.new_path).toBe("unié🎉.txt");
  });

  it("falls back to the caller-supplied repo-relative path without headers", () => {
    // Hand-built hunk-only input (no ---/+++ block): the caller's path is
    // already repo-relative, so it must pass through UNSTRIPPED — the old
    // ^[ab]/ rewrite corrupted paths in repos with real a/ or b/ dirs.
    const lines = parseUnifiedDiff("@@ -1 +1 @@\n-old\n+new\n");
    const add = lines.findIndex((l) => l.type === "add");
    const patch = buildFilePatchFromLines(lines, "a/plain.txt", new Set([add]));
    expect(patch?.old_path).toBe("a/plain.txt");
    expect(patch?.new_path).toBe("a/plain.txt");
  });

  it("drops timestamps riding after a tab on header lines", () => {
    const lines = parseUnifiedDiff(
      "--- a/t.txt\t2026-01-01 00:00:00.000000000 +0000\n+++ b/t.txt\t2026-08-24 00:00:00.000000000 +0000\n@@ -1 +1 @@\n-x\n+y\n"
    );
    const add = lines.findIndex((l) => l.type === "add");
    const patch = buildFilePatchFromLines(lines, "t.txt", new Set([add]));
    expect(patch?.old_path).toBe("t.txt");
    expect(patch?.new_path).toBe("t.txt");
  });
});

describe("whole-file creation and deletion via /dev/null", () => {
  it("emits new_path /dev/null for a deleted file (header route)", () => {
    const lines = parseUnifiedDiff(
      [
        "diff --git a/gone.txt b/gone.txt",
        "deleted file mode 100644",
        "index 1111111..0000000",
        "--- a/gone.txt",
        "+++ /dev/null",
        "@@ -1,2 +0,0 @@",
        "-bye",
        "-cruel world",
      ].join("\n")
    );
    const header = lines.findIndex((l) => l.type === "hdr" && l.content.startsWith("@@"));
    const patch = buildFilePatchForHunk(lines, "gone.txt", header);
    expect(patch).not.toBeNull();
    expect(patch?.old_path).toBe("gone.txt");
    // Without this, the backend stages a 0-byte blob instead of removing.
    expect(patch?.new_path).toBe("/dev/null");
  });

  it("emits new_path /dev/null for a deleted file (headerless +0,0 numbers route)", () => {
    const lines = parseUnifiedDiff("@@ -1,2 +0,0 @@\n-bye\n-cruel world\n");
    const header = lines.findIndex((l) => l.type === "hdr" && l.content.startsWith("@@"));
    const patch = buildFilePatchForHunk(lines, "gone.txt", header);
    expect(patch?.new_path).toBe("/dev/null");
    expect(patch?.old_path).toBe("gone.txt");
  });

  it("does NOT flag /dev/null when only some deletions are selected of a modified file", () => {
    // A partial-stage of an ordinary modification selecting only the del
    // line must keep both paths pointing at the file.
    const lines = parseUnifiedDiff(
      "--- a/live.txt\n+++ b/live.txt\n@@ -1,2 +1,2 @@\n keep\n-drop me\n+keep me too\n"
    );
    const delIdx = lines.findIndex((l) => l.type === "del");
    const patch = buildFilePatchFromLines(lines, "live.txt", new Set([delIdx]));
    expect(patch?.old_path).toBe("live.txt");
    expect(patch?.new_path).toBe("live.txt");
  });

  it("passes old-side /dev/null through for created files", () => {
    const lines = parseUnifiedDiff(
      [
        "diff --git a/fresh.txt b/fresh.txt",
        "new file mode 100644",
        "index 0000000..1111111",
        "--- /dev/null",
        "+++ b/fresh.txt",
        "@@ -0,0 +1,2 @@",
        "+hello",
        "+world",
      ].join("\n")
    );
    const header = lines.findIndex((l) => l.type === "hdr" && l.content.startsWith("@@"));
    const patch = buildFilePatchForHunk(lines, "fresh.txt", header);
    expect(patch).not.toBeNull();
    // The backend's unified_path_header already special-cases /dev/null;
    // the contract test against real git proves the round-trip.
    expect(patch?.old_path).toBe("/dev/null");
    expect(patch?.new_path).toBe("fresh.txt");
  });
});

describe("serializeSelectivePatch (wire-format twin)", () => {
  function stagedPatch(raw: string, selectedTypes: string[]): string {
    const lines = parseUnifiedDiff(raw);
    const indices = new Set(
      lines.map((l, i) => (selectedTypes.includes(l.type) ? i : -1)).filter((i) => i >= 0)
    );
    const patch = buildFilePatchFromLines(lines, "f.txt", indices)!;
    return serializeSelectivePatch(patch, true);
  }

  it("appends the marker after each flagged side in a both-sides EOF hunk", () => {
    const raw = "--- a/eof.txt\n+++ b/eof.txt\n@@ -1,2 +1,2 @@\n shared\n-old tail\n\\ No newline at end of file\n+new tail\n\\ No newline at end of file\n";
    const text = stagedPatch(raw, ["del", "add"]);
    expect(text).toBe(
      [
        "--- a/eof.txt",
        "+++ b/eof.txt",
        "@@ -1,2 +1,2 @@",
        " shared",
        "-old tail",
        "\\ No newline at end of file",
        "+new tail",
        "\\ No newline at end of file",
      ].join("\n") + "\n"
    );
  });

  it("marks only the old side when staging a deletion-only EOF hunk", () => {
    const raw = "--- a/eof.txt\n+++ b/eof.txt\n@@ -1,2 +1,2 @@\n keep\n-gone tail\n\\ No newline at end of file\n+kept tail\n";
    const text = stagedPatch(raw, ["del"]);
    expect(text).toBe(
      [
        "--- a/eof.txt",
        "+++ b/eof.txt",
        "@@ -1,2 +1,1 @@",
        " keep",
        "-gone tail",
        "\\ No newline at end of file",
      ].join("\n") + "\n"
    );
    // Skipped addition stays out entirely.
    expect(text).not.toContain("+kept tail");
  });

  it("counts marker lines toward neither side's totals", () => {
    const raw = "--- a/x.txt\n+++ b/x.txt\n@@ -1,1 +1,1 @@\n-solo\n\\ No newline at end of file\n+solo too\n\\ No newline at end of file\n";
    const text = stagedPatch(raw, ["del", "add"]);
    expect(text).toContain("@@ -1,1 +1,1 @@");
    expect(text.endsWith("-solo\n\\ No newline at end of file\n+solo too\n\\ No newline at end of file\n")).toBe(true);
  });

  it("writes +++ /dev/null for deleted-file patches and prefixes nothing else", () => {
    const raw = "--- a/gone.txt\n+++ /dev/null\n@@ -1,2 +0,0 @@\n-bye\n-cruel world\n";
    const lines = parseUnifiedDiff(raw);
    const header = lines.findIndex((l) => l.type === "hdr" && l.content.startsWith("@@"));
    const patch = buildFilePatchForHunk(lines, "gone.txt", header)!;
    expect(serializeSelectivePatch(patch, true)).toBe(
      ["--- a/gone.txt", "+++ /dev/null", "@@ -1,2 +0,0 @@", "-bye", "-cruel world"].join("\n") + "\n"
    );
  });

  it("preserves CRLF content bytes through serialization", () => {
    const raw = "--- a/crlf.txt\n+++ b/crlf.txt\n@@ -1,2 +1,2 @@\n-alpha\r\n+omega\r\n keep\r\n";
    const text = stagedPatch(raw, ["del", "add"]);
    expect(text).toBe(
      ["--- a/crlf.txt", "+++ b/crlf.txt", "@@ -1,2 +1,2 @@", "-alpha\r", "+omega\r", " keep\r"].join("\n") + "\n"
    );
  });

  it("mirrors unstaging role-swaps: selected additions become deletions", () => {
    const lines = parseUnifiedDiff("--- a/u.txt\n+++ b/u.txt\n@@ -1,1 +1,2 @@\n base\n+added by me\n");
    const addIdx = lines.findIndex((l) => l.type === "add");
    const patch = buildFilePatchFromLines(lines, "u.txt", new Set([addIdx]))!;
    const reverse = serializeSelectivePatch(patch, false);
    expect(reverse).toBe(
      ["--- a/u.txt", "+++ b/u.txt", "@@ -1,2 +1,1 @@", " base", "-added by me"].join("\n") + "\n"
    );
  });
});
