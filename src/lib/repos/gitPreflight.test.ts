import { describe, expect, it } from "vitest";
import {
  IDENTITY_MISSING,
  identityProblem,
  isHookCancelled,
  isIdentityMissing,
  parseGitPreflight,
  preflightProblem,
} from "./gitPreflight";

describe("git preflight", () => {
  it("reports a missing or outdated git and stays quiet when it is fine", () => {
    expect(preflightProblem(parseGitPreflight({ status: "ok", version: "git version 2.45.0", message: null }))).toBeNull();
    expect(preflightProblem(parseGitPreflight({ status: "missing", version: null, message: "Git was not found." })))
      .toBe("Git was not found.");
    expect(preflightProblem(parseGitPreflight({ status: "outdated", version: "git version 2.20.1", message: "too old" })))
      .toBe("too old");
  });

  it("does not present a probe that never ran as a problem with git", () => {
    expect(preflightProblem(parseGitPreflight({ status: "unchecked", version: null, message: "busy" }))).toBeNull();
  });

  it("refuses a malformed answer instead of reading it as fine", () => {
    expect(() => parseGitPreflight({ status: "great" })).toThrow();
    expect(() => parseGitPreflight(null)).toThrow();
  });
});

describe("commit preflight errors", () => {
  it("recognises the identity refusal by its opening phrase only", () => {
    expect(isIdentityMissing(`${IDENTITY_MISSING}: user.name is not set.`)).toBe(true);
    expect(isIdentityMissing("Author identity unknown")).toBe(false);
    expect(isIdentityMissing(undefined)).toBe(false);
  });

  it("recognises a cancelled hook run", () => {
    expect(isHookCancelled("git commit was cancelled while running its pre-commit hook (installed) in /r/.git/hooks.")).toBe(true);
    expect(isHookCancelled("git commit timed out after 1200s")).toBe(false);
  });

  it("validates an identity the way the backend does", () => {
    expect(identityProblem("Ada", "ada@gitpulse.dev")).toBeNull();
    expect(identityProblem("  ", "ada@gitpulse.dev")).toMatch(/name/);
    expect(identityProblem("Ada <x>", "ada@gitpulse.dev")).toMatch(/< or >/);
    expect(identityProblem("Ada", "ada")).toMatch(/email/);
    expect(identityProblem("Ada", "ada @x.dev")).toMatch(/email/);
    expect(identityProblem("Ada", "@x.dev")).toMatch(/email/);
  });
});
