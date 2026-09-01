import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

/**
 * Backlog C2 requires the Rust `PullRequestInfo` struct and the TypeScript
 * interface to agree field for field. The interface is declared inline in the
 * panel rather than in github/types.ts, so check:types does not cover it;
 * this does.
 */
const rust = readFileSync(new URL("../src-tauri/src/github/mod.rs", import.meta.url), "utf8");
const panel = readFileSync(
  new URL("../src/lib/components/GitHubPanel.svelte", import.meta.url),
  "utf8",
);

/** Slice from a declaration to its closing brace at the given indent. */
function block(source: string, header: string, closeIndent = ""): string {
  const start = source.indexOf(header);
  expect(start, `${header} must exist`).toBeGreaterThanOrEqual(0);
  const from = source.indexOf("{", start);
  const to = source.indexOf(`\n${closeIndent}}`, from);
  expect(to, `${header} must be closed`).toBeGreaterThan(from);
  return source.slice(from, to);
}

function rustFields(source: string): string[] {
  return [...source.matchAll(/^\s*pub ([a-z_]+):/gm)].map((match) => match[1]).sort();
}

function tsFields(source: string): string[] {
  return [...source.matchAll(/^\s{4}([a-z_]+)\??:/gm)].map((match) => match[1]).sort();
}

describe("pull-request timing contract", () => {
  it("keeps the Rust struct and the TypeScript interface field-for-field identical", () => {
    const fromRust = rustFields(block(rust, "pub struct PullRequestInfo"));
    // The interface is indented two spaces inside <script module>, so the
    // closing brace must be matched at that indent or the slice runs on into
    // the next interface.
    const fromTs = tsFields(block(panel, "interface PullRequestInfo", "  "));
    expect(fromRust.length).toBeGreaterThan(8);
    expect(fromTs).toEqual(fromRust);
  });

  it("requests only gh fields that were verified against `gh pr list --json`", () => {
    // A field gh does not know fails the WHOLE listing, which once degraded
    // the entire panel into an error.
    // Scope to the pull-request listing: other gh calls have their own field
    // sets, and matching the first one found tested the issue listing instead.
    const listing = block(rust, "fn list_pull_requests");
    const requested = /"number,title,[^"]*"/.exec(listing)?.[0] ?? "";
    expect(requested).toContain("createdAt");
    expect(requested).toContain("updatedAt");
    expect(requested).toContain("reviewDecision");
    expect(requested).toContain("reviews");
    // camelCase is gh's vocabulary; a snake_case name here would be rejected.
    expect(requested).not.toMatch(/created_at|review_decision/);
  });

  it("keeps 'not reviewed' distinct from 'reviewed instantly' on both sides", () => {
    expect(rust).toContain("fn earliest_review");
    const velocity = readFileSync(
      new URL("../src/lib/github/prVelocity.ts", import.meta.url),
      "utf8",
    );
    expect(velocity).toContain("return null");
    expect(velocity).toContain("isAwaitingFirstReview");
  });
});
