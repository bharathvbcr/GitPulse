import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

/**
 * SECURITY.md promises of the opt-in release check: "No user tokens,
 * repository paths, or hardware telemetry are ever sent."
 *
 * That is a claim about one function, and it is checkable from the source:
 * the request must be a repo-less git invocation against the project's own
 * upstream URL, carrying nothing derived from the user's machine.
 */
const updates = readFileSync(new URL("../src-tauri/src/updates/mod.rs", import.meta.url), "utf8");

function bodyOf(name: string): string {
  const start = updates.indexOf(`pub fn ${name}`);
  expect(start, `${name} must exist`).toBeGreaterThanOrEqual(0);
  const end = updates.indexOf("\n}\n", start);
  expect(end, `${name} must be closed`).toBeGreaterThan(start);
  return updates.slice(start, end);
}

describe("update check privacy contract", () => {
  const body = bodyOf("check_for_update");

  it("runs the check outside any repository", () => {
    // A repo-scoped invocation would put the user's repository path on the
    // command line. `git_global_with_timeout` takes no repo.
    expect(body).toContain("git_global_with_timeout");
    expect(body).not.toContain("git_with_timeout(");
    expect(body).not.toMatch(/repo_path|repo:/);
  });

  it("contacts only the project's own upstream, from a build-time constant", () => {
    expect(body).toContain("REPOSITORY_URL");
    // A URL assembled at runtime could carry something derived from the user.
    expect(updates).toContain('pub const REPOSITORY_URL: &str = env!("CARGO_PKG_REPOSITORY")');
    expect(body).not.toContain("format!");
  });

  it("sends only tag plumbing, with no credential or identity flags", () => {
    expect(body).toContain('"ls-remote"');
    expect(body).toContain('"--tags"');
    for (const leak of ["--upload-pack", "-c ", "http.extraHeader", "Authorization", "token"]) {
      expect(body, `argv must not carry ${leak}`).not.toContain(leak);
    }
  });

  it("is bounded, so a hung remote cannot stall the app", () => {
    expect(body).toContain("CHECK_TIMEOUT");
  });
});
