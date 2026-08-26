import { describe, expect, it } from "vitest";
import { extractPlanSteps, tokenizeCommand } from "./tokenize";

describe("tokenizeCommand", () => {
  it("splits plain words", () => {
    expect(tokenizeCommand("npm audit fix")).toEqual({
      ok: true,
      argv: ["npm", "audit", "fix"],
    });
  });

  it("collapses runs of whitespace", () => {
    expect(tokenizeCommand("  cargo   update\t-p serde ")).toEqual({
      ok: true,
      argv: ["cargo", "update", "-p", "serde"],
    });
  });

  it("keeps quoted arguments intact, including empty ones", () => {
    expect(tokenizeCommand('git commit -m "feat: two words"')).toEqual({
      ok: true,
      argv: ["git", "commit", "-m", "feat: two words"],
    });
    expect(tokenizeCommand("git commit -m ''")).toEqual({
      ok: true,
      argv: ["git", "commit", "-m", ""],
    });
  });

  it("joins adjacent quoted segments into one argument", () => {
    expect(tokenizeCommand("echo 'a''b'\"c\"")).toEqual({
      ok: true,
      argv: ["echo", "abc"],
    });
  });

  it("honors backslash escapes outside quotes", () => {
    expect(tokenizeCommand("echo a\\ b")).toEqual({ ok: true, argv: ["echo", "a b"] });
    // Escaped metacharacters are literal data, not shell syntax.
    expect(tokenizeCommand("echo a\\&b")).toEqual({ ok: true, argv: ["echo", "a&b"] });
  });

  it("inside double quotes escapes only the sh-legal set", () => {
    expect(tokenizeCommand('echo "a\\"b"')).toEqual({ ok: true, argv: ["echo", 'a"b'] });
    expect(tokenizeCommand("echo \"a\\nb\"")).toEqual({ ok: true, argv: ["echo", "a\\nb"] });
  });

  it("refuses pipes, chaining, and redirection instead of mis-running them", () => {
    for (const line of [
      "npm ci && npm audit fix",
      "cargo audit | tee log.txt",
      "echo done; rm -rf /",
      "ls > out.txt",
      "sort < in.txt",
      "echo $(whoami)",
      "echo `id`",
    ]) {
      const result = tokenizeCommand(line);
      expect(result.ok, line).toBe(false);
      if (!result.ok) {
        expect(result.error).toMatch(/shell syntax/);
      }
    }
  });

  it("refuses unterminated quotes", () => {
    const result = tokenizeCommand('git commit -m "unterminated');
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.error).toMatch(/Unterminated quote/);
  });

  it("rejects empty input", () => {
    for (const line of ["", "   ", '""', "''"]) {
      expect(tokenizeCommand(line).ok).toBe(false);
    }
  });

  it("an all-quoted empty token still counts as one empty argument", () => {
    expect(tokenizeCommand('""')).toEqual({ ok: false, error: expect.any(String) });
    // A lone empty string is not a command; but `cmd ''` is cmd with "" arg.
    expect(tokenizeCommand("echo ''")).toEqual({ ok: true, argv: ["echo", ""] });
  });
});

describe("extractPlanSteps", () => {
  it("extracts numbered steps with their inline commands", () => {
    const plan = [
      "# Remediation plan",
      "",
      "1. First patch the vulnerable package by running `npm audit fix`.",
      "2. Then refresh the lockfile with `npm install`.",
      "3. Finally verify nothing broke: `npm test`.",
    ].join("\n");

    const steps = extractPlanSteps(plan);
    expect(steps).toHaveLength(3);
    expect(steps[0]).toEqual({
      number: 1,
      text: "First patch the vulnerable package by running `npm audit fix`.",
      commands: ["npm audit fix"],
    });
    expect(steps[2].commands).toEqual(["npm test"]);
  });

  it("reads fenced blocks as step lines too", () => {
    const plan = [
      "1. Apply the fix:",
      "",
      "```bash",
      "npm audit fix --package lodash",
      "```",
    ].join("\n");
    const steps = extractPlanSteps(plan);
    expect(steps).toHaveLength(1);
    expect(steps[0].commands).toEqual([]);
  });

  it("keeps prose-only steps visible instead of dropping them", () => {
    const plan = [
      "1. Review the diff carefully before committing — this is a major bump.",
      "2. Run `npm test` afterwards.",
    ].join("\n");
    const steps = extractPlanSteps(plan);
    expect(steps).toHaveLength(2);
    expect(steps[0].commands).toEqual([]);
    expect(steps[0].text).toContain("major bump");
  });

  it("extracts multiple command spans from one step", () => {
    const plan = "1. Run `npm audit fix`, then `npm outdated` to confirm.";
    expect(extractPlanSteps(plan)[0].commands).toEqual(["npm audit fix", "npm outdated"]);
  });

  it("returns no steps for prose without list structure", () => {
    expect(extractPlanSteps("No vulnerabilities reported. Nothing to do.")).toEqual([]);
  });
});
