/**
 * Adversarial checks on lane L5's Cursor hooks record (ft-878db72b), written by
 * a reviewer who did not write it. A failing test here is a claim that is false.
 *
 * Source, fetched 2026-10-26 with curl and tag-stripped (not a summariser):
 *
 * - https://cursor.com/docs/hooks, per-script options table:
 *   `timeout | number | platform default | Execution timeout in seconds`.
 * - https://cursor.com/docs/reference/third-party-hooks, "Tool Name Mapping":
 *   `Bash -> Shell`, `Read -> Read`, `Write -> Write`, `Edit -> Write`,
 *   `Grep`, `Task`, `WebFetch`, `WebSearch`, `Glob -> -` (not supported); and
 *   its event table has `Notification | - | No`, `PermissionRequest | - | No`.
 * - https://cursor.com/docs/hooks, "Matcher Configuration": for
 *   `preToolUse`, "Values include Shell, Read, Write, Grep, Delete, Task".
 *
 * `docs/QUALIFICATION.md` ("Plugin hooks per agent host") lists as "Not in any
 * page read" both the unit of `timeout` and whether a `preToolUse` matcher
 * matches Claude's tool names or Cursor's, and quotes "not supported" for two
 * events whose table cell says `No`.
 */
import { spawnSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const QUALIFICATION_PATH = "docs/QUALIFICATION.md";
const qualificationUrl = new URL(`../${QUALIFICATION_PATH}`, import.meta.url);
const hasQualification = existsSync(fileURLToPath(qualificationUrl));

const qualification = hasQualification ? readFileSync(qualificationUrl, "utf8") : "";
const start = qualification.indexOf("### Plugin hooks per agent host");
const end = qualification.indexOf("### Native PTY", start);
const section = qualification.slice(start, end);
// A missing section must fail each test below, not pass them by being empty.
const found = start > 0 && end > start;

it(`${QUALIFICATION_PATH} may be absent only while git ignores it`, () => {
  if (hasQualification) return;
  const ignored = spawnSync("git", ["check-ignore", "-q", "--", QUALIFICATION_PATH], {
    cwd: fileURLToPath(new URL("..", import.meta.url)),
    timeout: 5_000,
  });
  expect(ignored.error, "git check-ignore could not run").toBeUndefined();
  expect(ignored.status, `${QUALIFICATION_PATH} is missing and not ignored`).toBe(0);
});

describe.skipIf(!hasQualification)("the Cursor rows of docs/QUALIFICATION.md", () => {
  it("adv_l5_does_not_list_the_timeout_unit_as_unverified", () => {
    expect(found).toBe(true);
    expect(section).not.toMatch(/the unit of `timeout`/);
  });

  it("adv_l5_does_not_list_the_pretooluse_tool_names_as_unverified", () => {
    // The mapping is documented (Claude's `Bash` is Cursor's `Shell`, `Edit`
    // is `Write`; the hooks page lists the `preToolUse` matcher values), so
    // "not in any page read" is false of this question.
    expect(found).toBe(true);
    expect(section).not.toMatch(
      /whether a plugin `matcher` on `preToolUse` matches/,
    );
  });

  it("adv_l5_does_not_quote_not_supported_where_the_page_says_No", () => {
    expect(found).toBe(true);
    expect(section).not.toMatch(/"not supported"/);
  });
});
