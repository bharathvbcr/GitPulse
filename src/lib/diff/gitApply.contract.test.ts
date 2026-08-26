/**
 * Contract tests: parse → build → serialize → REAL `git apply --cached`.
 *
 * These pin the whole selective-staging wire format against actual git, in
 * throwaway temp repos. Every case asserts the exact corrected patch text
 * (each of which the pre-fix builder could NOT have produced):
 *   - deleted files must target +++ /dev/null or git stages an empty blob
 *     instead of removing the entry;
 *   - EOF hunks must carry `\ No newline at end of file` or git rejects the
 *     patch because the no-newline preimage cannot match;
 *   - CRLF files must keep their \r bytes (the Rust validator historically
 *     rejected them outright);
 *   - the phantom trailing "" context row from split("\n") corrupted every
 *     last-hunk patch with a fabricated empty context line.
 */
import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, unlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import { parseUnifiedDiff, type AnnotatedDiffLine } from "./wordDiff";
import {
  buildFilePatchForHunk,
  buildFilePatchFromLines,
  serializeSelectivePatch,
} from "./patchBuilder";

const gitAvailable = (() => {
  try {
    execFileSync("git", ["--version"], { stdio: "ignore" });
    return true;
  } catch {
    return false;
  }
})();

function git(cwd: string, ...args: string[]): string {
  return execFileSync("git", args, { cwd, encoding: "utf8" });
}

function gitApplyToIndex(cwd: string, patchText: string): void {
  execFileSync("git", ["apply", "--cached", "--unidiff-zero", "--recount", "-"], {
    cwd,
    input: patchText,
    encoding: "utf8",
  });
}

/** Like git(), but swallows stderr and reports success — for expected failures. */
function gitQuiet(cwd: string, ...args: string[]): boolean {
  try {
    execFileSync("git", args, { cwd, encoding: "utf8", stdio: ["pipe", "pipe", "ignore"] });
    return true;
  } catch {
    return false;
  }
}

function initRepo(): string {
  const dir = mkdtempSync(join(tmpdir(), "gitpulse-apply-contract-"));
  dirs.push(dir);
  git(dir, "init", "-q", "-b", "main");
  git(dir, "config", "user.email", "contract@gitpulse.test");
  git(dir, "config", "user.name", "GitPulse Contract");
  git(dir, "config", "core.autocrlf", "false");
  return dir;
}

function commitFile(dir: string, path: string, content: string): void {
  writeFileSync(join(dir, path), content);
  git(dir, "add", "--", path);
  git(dir, "commit", "-q", "-m", `add ${path}`);
}

function indexOfHunkHeader(lines: AnnotatedDiffLine[]): number {
  return lines.findIndex((l) => l.type === "hdr" && l.content.startsWith("@@"));
}

const dirs: string[] = [];

afterEach(() => {
  for (const dir of dirs.splice(0)) {
    rmSync(dir, { recursive: true, force: true });
  }
});

describe.skipIf(!gitAvailable)("git apply --cached contract (real git)", () => {
  it("stages a plain modification via both hunk and line-selection variants", () => {
    const dir = initRepo();
    commitFile(dir, "plain.txt", "alpha\nbeta\ngamma\n");
    writeFileSync(join(dir, "plain.txt"), "alpha\nBETA\ngamma\n");

    const raw = git(dir, "diff");
    const lines = parseUnifiedDiff(raw);

    // Hunk variant: everything in the hunk.
    const hunkPatch = buildFilePatchForHunk(lines, "plain.txt", indexOfHunkHeader(lines))!;
    expect(hunkPatch.old_path).toBe("plain.txt");
    expect(hunkPatch.new_path).toBe("plain.txt");
    const hunkText = serializeSelectivePatch(hunkPatch, true);
    expect(hunkText).toBe(
      "--- a/plain.txt\n+++ b/plain.txt\n@@ -1,3 +1,3 @@\n alpha\n-beta\n+BETA\n gamma\n"
    );
    gitApplyToIndex(dir, hunkText);
    expect(git(dir, "show", ":plain.txt")).toBe("alpha\nBETA\ngamma\n");

    // Line-selection variant: stage only the replacement line. Reset the
    // index back to HEAD first (checkout would restore from the staged
    // index and leave nothing to diff).
    git(dir, "reset", "-q", "HEAD", "--", "plain.txt");
    const lines2 = parseUnifiedDiff(git(dir, "diff"));
    const addIdx = lines2.findIndex((l) => l.type === "add");
    const linePatch = buildFilePatchFromLines(lines2, "plain.txt", new Set([addIdx]))!;
    const lineText = serializeSelectivePatch(linePatch, true);
    // Unselected deletions ride along as context so git has a full preimage.
    expect(lineText).toBe(
      "--- a/plain.txt\n+++ b/plain.txt\n@@ -1,3 +1,4 @@\n alpha\n beta\n+BETA\n gamma\n"
    );
    gitApplyToIndex(dir, lineText);
    expect(git(dir, "show", ":plain.txt")).toBe("alpha\nbeta\nBETA\ngamma\n");
  });

  it("reproduces missing-newline markers so EOF hunks actually apply", () => {
    const dir = initRepo();
    commitFile(dir, "eof.txt", "first\nsecond");
    writeFileSync(join(dir, "eof.txt"), "first\nSECOND");

    const lines = parseUnifiedDiff(git(dir, "diff"));
    const del = lines.find((l) => l.type === "del")!;
    const add = lines.find((l) => l.type === "add")!;
    expect(del.noNewline).toBe(true);
    expect(add.noNewline).toBe(true);

    const patch = buildFilePatchForHunk(lines, "eof.txt", indexOfHunkHeader(lines))!;
    const text = serializeSelectivePatch(patch, true);
    expect(text).toBe(
      [
        "--- a/eof.txt",
        "+++ b/eof.txt",
        "@@ -1,2 +1,2 @@",
        " first",
        "-second",
        "\\ No newline at end of file",
        "+SECOND",
        "\\ No newline at end of file",
      ].join("\n") + "\n"
    );
    gitApplyToIndex(dir, text);
    const staged = git(dir, "show", ":eof.txt");
    expect(staged).toBe("first\nSECOND");
    expect(staged.endsWith("\n")).toBe(false);
  });

  it("keeps CRLF bytes intact through the whole pipeline", () => {
    const dir = initRepo();
    commitFile(dir, "crlf.txt", "one\r\ntwo\r\nthree\r\n");
    writeFileSync(join(dir, "crlf.txt"), "one\r\nTWO\r\nthree\r\n");

    const lines = parseUnifiedDiff(git(dir, "diff"));
    const patch = buildFilePatchForHunk(lines, "crlf.txt", indexOfHunkHeader(lines))!;
    const text = serializeSelectivePatch(patch, true);
    expect(text).toBe(
      "--- a/crlf.txt\n+++ b/crlf.txt\n@@ -1,3 +1,3 @@\n one\r\n-two\r\n+TWO\r\n three\r\n"
    );
    gitApplyToIndex(dir, text);
    expect(git(dir, "show", ":crlf.txt")).toBe("one\r\nTWO\r\nthree\r\n");
  });

  it("deletes the index entry via +++ /dev/null instead of staging an empty blob", () => {
    const dir = initRepo();
    commitFile(dir, "gone.txt", "keep me\ngone soon\n");
    unlinkSync(join(dir, "gone.txt"));

    const lines = parseUnifiedDiff(git(dir, "diff"));
    expect(lines.find((l) => l.content.startsWith("+++"))?.content).toBe("+++ /dev/null");

    const patch = buildFilePatchForHunk(lines, "gone.txt", indexOfHunkHeader(lines))!;
    expect(patch.new_path).toBe("/dev/null");
    const text = serializeSelectivePatch(patch, true);
    expect(text).toBe(
      "--- a/gone.txt\n+++ /dev/null\n@@ -1,2 +0,0 @@\n-keep me\n-gone soon\n"
    );

    gitApplyToIndex(dir, text);
    expect(git(dir, "ls-files").split("\n")).not.toContain("gone.txt");
    expect(gitQuiet(dir, "show", ":gone.txt")).toBe(false);
    // Worktree untouched by --cached.
    expect(git(dir, "status", "--porcelain")).toContain("D  gone.txt");
  });

  it("creates index entries for intent-to-add files via --- /dev/null", () => {
    const dir = initRepo();
    commitFile(dir, "base.txt", "anchor\n");
    writeFileSync(join(dir, "fresh.txt"), "hola\nhey\n");
    git(dir, "add", "--intent-to-add", "fresh.txt");

    const raw = git(dir, "diff");
    const lines = parseUnifiedDiff(raw);
    expect(lines.some((l) => l.content === "--- /dev/null")).toBe(true);

    // The fresh-file hunk is the second one in the diff (after base.txt's
    // no-op section); find ITS header, not the first.
    const freshHeader = lines.findIndex(
      (l) => l.type === "hdr" && l.content.startsWith("@@ -0,0")
    );
    expect(freshHeader).toBeGreaterThan(-1);
    const patch = buildFilePatchForHunk(lines, "fresh.txt", freshHeader)!;
    expect(patch.old_path).toBe("/dev/null");
    expect(patch.new_path).toBe("fresh.txt");
    const text = serializeSelectivePatch(patch, true);
    expect(text).toBe("--- /dev/null\n+++ b/fresh.txt\n@@ -0,0 +1,2 @@\n+hola\n+hey\n");

    gitApplyToIndex(dir, text);
    expect(git(dir, "show", ":fresh.txt")).toBe("hola\nhey\n");
    // The anchor file is unaffected.
    expect(git(dir, "ls-files").split("\n")).toEqual(expect.arrayContaining(["base.txt"]));
    expect(readFileSync(join(dir, "fresh.txt"), "utf8")).toBe("hola\nhey\n");
  });
});
