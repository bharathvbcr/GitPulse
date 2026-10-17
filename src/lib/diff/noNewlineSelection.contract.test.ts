/**
 * Selection x missing-trailing-newline contract (real `git apply --cached`).
 *
 * `\ No newline at end of file` describes the end of one file side, and only
 * that side's last line can lack its newline. A flagged row therefore may not
 * keep the marker when the selection leaves more result lines after it (a
 * skipped deletion kept as context, a restored deletion ahead of unselected
 * additions): git joins the two lines. This pins every selection subset of a
 * set of blob pairs, in both directions, against the index git actually
 * writes.
 */
import { spawnSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import { parseUnifiedDiff, type AnnotatedDiffLine } from "./wordDiff";
import { buildFilePatchFromLines, serializeSelectivePatch } from "./patchBuilder";

const IDENT = ["-c", "user.name=GitPulse Contract", "-c", "user.email=contract@gitpulse.test"];

function git(cwd: string, args: string[], input?: string) {
  return spawnSync("git", args, { cwd, input, encoding: "utf8" });
}

const dirs: string[] = [];
afterEach(() => {
  for (const dir of dirs.splice(0)) rmSync(dir, { recursive: true, force: true });
});

function initRepo(): string {
  const dir = mkdtempSync(join(tmpdir(), "gitpulse-nonl-sel-"));
  dirs.push(dir);
  git(dir, ["init", "-q", "-b", "main"]);
  git(dir, ["config", "core.autocrlf", "false"]);
  return dir;
}

type Mode = "stage" | "unstage";

/**
 * The index a selection should produce: rows join with "\n", and the file
 * ends without a newline only when its last row came from a side that did.
 */
function expectedIndex(lines: AnnotatedDiffLine[], selected: Set<number>, mode: Mode): string {
  const out: { text: string; newline: boolean }[] = [];
  lines.forEach((l, i) => {
    if (l.type !== "ctx" && l.type !== "add" && l.type !== "del") return;
    const row = { text: l.content.slice(1), newline: !l.noNewline };
    const picked = selected.has(i);
    if (l.type === "ctx") out.push(row);
    else if (mode === "stage" ? (l.type === "add") === picked : (l.type === "add") !== picked)
      out.push(row);
  });
  return out.map((r, i) => r.text + (i < out.length - 1 || r.newline ? "\n" : "")).join("");
}

function checkPair(oldBlob: string, newBlob: string, mode: Mode): void {
  const dir = initRepo();
  const f = join(dir, "f");
  writeFileSync(f, oldBlob);
  git(dir, ["add", "f"]);
  let before = oldBlob;
  let raw: string;
  if (mode === "stage") {
    writeFileSync(f, newBlob);
    raw = git(dir, ["diff"]).stdout;
  } else {
    git(dir, [...IDENT, "commit", "-q", "-m", "base"]);
    writeFileSync(f, newBlob);
    git(dir, ["add", "f"]);
    raw = git(dir, ["diff", "--cached"]).stdout;
    before = newBlob;
  }
  const lines = parseUnifiedDiff(raw);
  const changed = lines.flatMap((l, i) => (l.type === "add" || l.type === "del" ? [i] : []));
  for (let mask = 1; mask < 1 << changed.length; mask++) {
    const selected = new Set(changed.filter((_, bit) => mask & (1 << bit)));
    const fp = buildFilePatchFromLines(lines, "f", selected);
    expect(fp).not.toBeNull();
    const patch = serializeSelectivePatch(fp!, mode === "stage");
    writeFileSync(f, before);
    git(dir, ["add", "f"]);
    const applied = git(dir, ["apply", "--cached", "--unidiff-zero", "--recount", "-"], patch);
    const context = JSON.stringify({ mode, oldBlob, newBlob, selected: [...selected], patch });
    expect(applied.status, `${context}: ${applied.stderr}`).toBe(0);
    expect(git(dir, ["show", ":f"]).stdout, context).toBe(expectedIndex(lines, selected, mode));
  }
}

const PAIRS: Array<[string, string]> = [
  // Skipped deletion that lacked the newline, kept as context before additions.
  ["b\nc", "b\nb\nb"],
  ["a\nb", "c\nc\nc\n"],
  ["c", "c\nc\na\n"],
  ["b", "b\na"],
  ["a\nb", "b\nc\na"],
  // Only one side lacks the newline.
  ["a\nx\n", "a\nx\ny"],
  ["a\n", "a"],
  ["a", "a\n"],
  // Both sides lack it and the last row is context.
  ["one\ntwo\nthree", "ONE\ntwo\nthree"],
  // CRLF bytes ride along with the marker.
  ["a\r\nb", "a\r\nB\r\nc"],
];

describe.skipIf(spawnSync("git", ["--version"]).status !== 0)(
  "no-newline marker x line selection (real git)",
  () => {
    for (const [oldBlob, newBlob] of PAIRS) {
      for (const mode of ["stage", "unstage"] as const) {
        // Every selection subset is a real `git apply`: dozens of git processes
        // a case, so vitest's 5 s default fails it on a loaded host (it did,
        // 4 cases, in a full ci:local at load ~90) though it passes alone.
        it(`${mode} ${JSON.stringify(oldBlob)} -> ${JSON.stringify(newBlob)}, every selection`, () => {
          checkPair(oldBlob, newBlob, mode);
        }, 60_000);
      }
    }
  }
);
