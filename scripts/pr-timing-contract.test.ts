import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { CONTRACTS } from "./check-coverage-types.mjs";

/**
 * Backlog C2 requires the Rust `PullRequestInfo` struct and the TypeScript
 * interface to agree field for field. That comparison used to live here
 * because the interface was declared inline in GitHubPanel, out of reach of
 * check:types. It has since moved to github/types.ts and is covered by the
 * `github` contract, which compares wire types and presence as well as names —
 * strictly more than this file did. What remains here is everything that
 * contract cannot see: gh's field vocabulary, and the distinction between
 * "not reviewed" and "reviewed instantly".
 */
const rust = readFileSync(new URL("../src-tauri/src/github/mod.rs", import.meta.url), "utf8");
/** Slice from a declaration to its closing brace at the given indent. */
function block(source: string, header: string, closeIndent = ""): string {
  const start = source.indexOf(header);
  expect(start, `${header} must exist`).toBeGreaterThanOrEqual(0);
  const from = source.indexOf("{", start);
  const to = source.indexOf(`\n${closeIndent}}`, from);
  expect(to, `${header} must be closed`).toBeGreaterThan(from);
  return source.slice(from, to);
}

describe("pull-request timing contract", () => {
  it("keeps PullRequestInfo inside the check:types contract table", () => {
    // The field-for-field comparison this file used to do by hand now belongs
    // to check:types. Asserting the type is still listed there means moving it
    // back into a component cannot silently drop the check on the way.
    const covered = CONTRACTS.some((contract) => contract.structs.includes("PullRequestInfo"));
    expect(covered, "PullRequestInfo must stay covered by a check:types contract").toBe(true);
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
