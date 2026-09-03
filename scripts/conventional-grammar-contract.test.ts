import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { headerLine, isConventionalCommit } from "../src/lib/pulse/metrics";

/**
 * `analyzer/conventional.rs` is the canonical owner of the Conventional
 * Commits header grammar: the commit badges, the `type:` search filter and the
 * Pulse hygiene metric must all agree about the same subject line.
 *
 * Pulse used to carry a hand-written frontend regex with a fixed 11-type
 * vocabulary, a narrow scope charset and a mandatory description. It silently
 * disagreed with the backend on `wip:`, `fix(build system): x` and `chore:`.
 *
 * Rather than restate the grammar here — a third copy, free to drift in turn —
 * this extracts the pattern from the Rust source and asserts the frontend
 * agrees with it across a corpus. Change either side and this fails.
 */
const RUST_SOURCE = "src-tauri/src/analyzer/conventional.rs";

function rustHeaderPattern(): RegExp {
  const source = readFileSync(new URL(`../${RUST_SOURCE}`, import.meta.url), "utf8");
  // The header regex is the one anchored at `^(` capturing the commit type.
  const match = source.match(/Regex::new\(r"(\^\([a-zA-Z\-\]\[+]+\)[^"]*)"\)/);
  expect(match, `no header regex literal found in ${RUST_SOURCE}`).not.toBeNull();
  const pattern = match![1];
  expect(pattern).toContain("[a-zA-Z]+");
  // Rust `regex` and JS RegExp share this subset verbatim.
  return new RegExp(pattern);
}

/** Subjects that separate the real grammar from a guessed one. */
const CORPUS = [
  "feat: add pulse view",
  "fix(auth): stop dropping the session",
  "feat!: breaking change",
  "fix(api)!: breaking with scope",
  "FEAT: uppercase type",
  "Fix: capitalised type",
  "wip: not in the classic vocabulary",
  "hotfix: also not in it",
  "chore(deps): bump serde",
  "fix(build system): scope containing a space",
  "fix(a/b.c-d): punctuated scope",
  "docs:",
  "docs:   ",
  "refactor:no space after colon",
  // Non-conventional shapes.
  "just a normal commit message",
  "Merge branch 'main' into feature",
  "Revert \"feat: add pulse view\"",
  "fix",
  "fix -- not a colon",
  "123: numeric type",
  "fix(): empty scope",
  ": no type at all",
  "",
  "   ",
  // A full message: the Rust parser matches its header line, not the body.
  "feat: add pulse view\n\nCo-authored-by: Bob <bob@example.com>",
  "not conventional\n\nfeat: but the body is",
];

describe("conventional-commit grammar matches the canonical Rust parser", () => {
  it("agrees with the Rust header regex on every corpus subject", () => {
    const rust = rustHeaderPattern();
    // `ConventionalCommitParser::parse` takes `lines().next()?.trim()` before
    // applying the pattern, so the comparison has to do the same.
    const disagreements = CORPUS.filter(
      (subject) => isConventionalCommit(subject) !== rust.test(headerLine(subject)),
    );
    expect(
      disagreements,
      `frontend and ${RUST_SOURCE} disagree on these subjects`,
    ).toEqual([]);
  });

  it("accepts types outside the classic vocabulary, as the backend does", () => {
    // The regression that motivated this contract.
    expect(isConventionalCommit("wip: still going")).toBe(true);
    expect(isConventionalCommit("hotfix: page down")).toBe(true);
  });

  it("accepts an empty description and a spaced scope", () => {
    expect(isConventionalCommit("docs:")).toBe(true);
    expect(isConventionalCommit("fix(build system): x")).toBe(true);
  });

  it("parses the header line of a message that carries a body", () => {
    expect(isConventionalCommit("feat: x\n\nCo-authored-by: Bob <b@e.com>")).toBe(true);
    expect(isConventionalCommit("just a subject\n\nfeat: in the body only")).toBe(false);
  });

  it("still rejects subjects with no conventional header", () => {
    expect(isConventionalCommit("just a normal commit message")).toBe(false);
    expect(isConventionalCommit("Merge branch 'main'")).toBe(false);
    expect(isConventionalCommit("123: numeric type")).toBe(false);
  });
});
