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
import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const read = (relative: string): string =>
  readFileSync(new URL(`../${relative}`, import.meta.url), "utf8");

const qualification = read("docs/QUALIFICATION.md");
const start = qualification.indexOf("### Plugin hooks per agent host");
const end = qualification.indexOf("### Native PTY", start);
const section = qualification.slice(start, end);
// A missing section must fail each test below, not pass them by being empty.
const found = start > 0 && end > start;

describe("the Cursor rows of docs/QUALIFICATION.md", () => {
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
