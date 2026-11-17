import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

/**
 * `bun audit --ignore=<GHSA>` accepts an advisory the dependency tree cannot
 * clear. An ignore with no reason and no end date is how a gate stops gating,
 * so each one has to be named by id, say why it is safe, and carry a
 * `revisit by YYYY-MM-DD` that has not passed. The same rule the coverage
 * exclusions follow (`scripts/coverage-exclusions-contract.test.ts`).
 *
 * The rule is a function over the workflow text so the self-test can show it
 * failing on the shapes it is for.
 */
const read = (path: string) => readFileSync(new URL(`../${path}`, import.meta.url), "utf8");

/** How far above an `--ignore` its explanation may sit, in lines. */
const COMMENT_WINDOW = 14;

export function ignoreProblems(workflow: string, today: Date): string[] {
  const problems: string[] = [];
  const lines = workflow.split("\n");
  lines.forEach((line, index) => {
    if (!/\bbun audit\b/.test(line) || line.trimStart().startsWith("#")) return;
    const flags = [...line.matchAll(/--(ignore|audit-level|prod|production)(?:=(\S+))?/g)];
    for (const [, flag, value] of flags) {
      if (flag === "audit-level") problems.push(`line ${index + 1}: --audit-level=${value} hides every advisory below that severity, not one named advisory`);
      if (flag === "prod" || flag === "production") problems.push(`line ${index + 1}: --${flag} stops auditing the dev toolchain altogether`);
      if (flag === "ignore" && !/^GHSA-[0-9a-z]{4}-[0-9a-z]{4}-[0-9a-z]{4}$/.test(value ?? "")) {
        problems.push(`line ${index + 1}: --ignore=${value} is not a single GHSA id`);
      }
    }
    if (!flags.some(([, flag]) => flag === "ignore")) return;
    const above = lines.slice(Math.max(0, index - COMMENT_WINDOW), index).filter((l) => l.trimStart().startsWith("#")).join("\n");
    const date = /revisit by (\d{4}-\d{2}-\d{2})/.exec(above)?.[1];
    if (!date) problems.push(`line ${index + 1}: an ignored advisory has no "revisit by YYYY-MM-DD" in the comment above it`);
    else if (new Date(`${date}T23:59:59Z`) < today) problems.push(`line ${index + 1}: the ignore's review date ${date} has passed`);
    for (const id of line.matchAll(/--ignore=(GHSA-\S+)/g)) {
      if (!above.includes(id[1])) problems.push(`line ${index + 1}: the comment above never names ${id[1]}`);
    }
  });
  return problems;
}

describe("bun audit ignores", () => {
  const workflow = read(".github/workflows/bun-audit.yml");

  it("every ignored advisory is a single id, explained, and inside its review date", () => {
    expect(ignoreProblems(workflow, new Date())).toEqual([]);
  });

  it("still audits: the job runs bun audit and the ignore is the only relaxation", () => {
    const audits = workflow.split("\n").filter((l) => /^\s+run: bun audit\b/.test(l));
    expect(audits).toHaveLength(1);
    expect(audits[0]).toMatch(/^\s+run: bun audit --ignore=GHSA-[0-9a-z-]+$/);
  });

  describe("the rule fails on the shapes it is for", () => {
    const withLine = (run: string, comment = "      # revisit by 2027-01-15; GHSA-gcqx-w48m-w2h4 is dev-only.") =>
      `    steps:\n${comment}\n      - name: bun audit\n        run: ${run}\n`;
    const today = new Date("2026-11-16T00:00:00Z");

    it("accepts a named, dated ignore", () => {
      expect(ignoreProblems(withLine("bun audit --ignore=GHSA-gcqx-w48m-w2h4"), today)).toEqual([]);
    });
    it("an ignore with no review date", () => {
      expect(ignoreProblems(withLine("bun audit --ignore=GHSA-gcqx-w48m-w2h4", "      # dev-only GHSA-gcqx-w48m-w2h4"), today).join("\n")).toContain("no \"revisit by");
    });
    it("an ignore whose review date has passed", () => {
      expect(ignoreProblems(withLine("bun audit --ignore=GHSA-gcqx-w48m-w2h4", "      # revisit by 2026-11-15 GHSA-gcqx-w48m-w2h4"), today).join("\n")).toContain("has passed");
    });
    it("an ignore the comment never names", () => {
      expect(ignoreProblems(withLine("bun audit --ignore=GHSA-aaaa-bbbb-cccc"), today).join("\n")).toContain("never names GHSA-aaaa-bbbb-cccc");
    });
    it("a blanket severity floor or a production-only audit", () => {
      expect(ignoreProblems(withLine("bun audit --audit-level=moderate"), today).join("\n")).toContain("hides every advisory below");
      expect(ignoreProblems(withLine("bun audit --prod"), today).join("\n")).toContain("dev toolchain");
    });
    it("something that is not one advisory id", () => {
      expect(ignoreProblems(withLine("bun audit --ignore=all"), today).join("\n")).toContain("not a single GHSA id");
    });
  });
});
