/**
 * Absence claims in the Cursor rows of docs/QUALIFICATION.md.
 *
 * The row "Cursor plugin, still undocumented" said what the Cursor pages do
 * not document. Four such claims were wrong in turn (the `timeout` unit, the
 * tool names, a quoted "not supported", and `cursor_version`, which the hooks
 * page lists as a common input field). A claim that a field is absent from a
 * page is only as good as the search behind it, so this file pins two things:
 *
 * - the sentences already shown false stay out of the text, and
 * - the row describes how its absence claims were checked (which pages, saved
 *   when, one name per search, untruncated counts), so a reader can rerun the
 *   search against the live pages.
 *
 * Source for the corrected claims, fetched 2026-10-26 and 2026-10-27:
 * https://cursor.com/docs/hooks, "Common input fields":
 *   `cursor_version | string | Cursor application version (e.g. "1.7.2")`
 * https://cursor.com/docs/reference/third-party-hooks, "Response Format
 * Compatibility": "Cursor supports both Claude Code's nested
 * `hookSpecificOutput` response format and the older flat response format."
 */
import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const qualification = readFileSync(
  new URL("../docs/QUALIFICATION.md", import.meta.url),
  "utf8",
);
const start = qualification.indexOf("### Plugin hooks per agent host");
const end = qualification.indexOf("### Native PTY", start);
const section = qualification.slice(start, end);
const found = start > 0 && end > start;

describe("absence claims in the Cursor rows of docs/QUALIFICATION.md", () => {
  it("finds its section (a missing one must fail every test, not pass them)", () => {
    expect(found).toBe(true);
  });

  it("does not say cursor_version is absent from the pages", () => {
    expect(section).not.toMatch(/cursor_version[^|]*appears on none of the pages/);
    expect(section).not.toMatch(/appears on none of the pages/);
  });

  it("says cursor_version is documented", () => {
    expect(section).toMatch(/`cursor_version` is a common input field/);
  });

  it("says what is missing about async: an option, not the word", () => {
    // The word appears in the pages' TypeScript examples (`async function`).
    expect(section).not.toMatch(/is not mentioned on any of the three pages/);
    expect(section).toMatch(/no `async` hook option/);
  });

  it("describes how its absence claims were checked, with page URLs", () => {
    for (const page of [
      "https://cursor.com/docs/hooks",
      "https://cursor.com/docs/reference/plugins",
      "https://cursor.com/docs/reference/third-party-hooks",
      "https://cursor.com/docs/plugins",
    ]) {
      expect(section, page).toContain(page);
    }
    expect(section).toMatch(/one name per command/);
    expect(section).toMatch(/never truncated/);
  });

  it("points at nothing outside the repository", () => {
    // No scratch directory or lane report id: none of them is tracked, so a
    // reader of the repository cannot open them.
    expect(section).not.toMatch(/\btmp\b|scratch|\bl5-/i);
  });

  it("does not claim the nested hookSpecificOutput format is unsupported", () => {
    expect(section).toMatch(/nested `hookSpecificOutput` response format/);
    expect(section).not.toMatch(/nested `permissionDecision`[^.]*blocks a Cursor tool/);
  });
});
