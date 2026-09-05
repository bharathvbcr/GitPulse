import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { extractNotes, main, normalizeTag } from "./release-notes.mjs";

const CHANGELOG = `# Changelog

## [Unreleased]

- pending work

## [1.2.0] - 2026-01-05

### Added

- a thing

## [1.1.0] - 2025-12-01

### Fixed

- another thing

[1.2.0]: https://github.com/o/r/compare/v1.1.0...v1.2.0
`;

describe("release notes extraction", () => {
  it("accepts a tag with or without the leading v", () => {
    expect(normalizeTag("v1.2.0")).toBe("1.2.0");
    expect(normalizeTag("1.2.0")).toBe("1.2.0");
    expect(() => normalizeTag("  ")).toThrow();
  });

  it("returns only the requested version's section", () => {
    const result = extractNotes(CHANGELOG, "v1.2.0");
    expect(result.found).toBe(true);
    if (!result.found) return;
    expect(result.date).toBe("2026-01-05");
    expect(result.body).toContain("- a thing");
    // the next release's content must not bleed in
    expect(result.body).not.toContain("another thing");
    expect(result.body).not.toContain("pending work");
  });

  it("strips the trailing link-reference footer from the last section", () => {
    const result = extractNotes(CHANGELOG, "1.1.0");
    expect(result.found).toBe(true);
    if (!result.found) return;
    expect(result.body).not.toContain("https://github.com");
    expect(result.body).toContain("another thing");
  });

  it("reports a tag with no section rather than returning empty notes", () => {
    const result = extractNotes(CHANGELOG, "v9.9.9");
    expect(result.found).toBe(false);
    if (result.found) return;
    expect(result.versions).toContain("1.2.0");
  });

  it("treats a heading with an empty body as missing", () => {
    const result = extractNotes("# C\n\n## [2.0.0] - 2026-02-02\n\n## [1.0.0] - 2026-01-01\n\n- real\n", "2.0.0");
    expect(result.found).toBe(false);
  });

  it("exits 1 for an absent tag and 2 for unusable input", () => {
    // A release must fail rather than publish notes that silently vanished.
    expect(main(["--tag", "v9.9.9"])).toBe(1);
    expect(main([])).toBe(2);
    expect(main(["--tag", "v1.0.0", "--changelog", "/nonexistent/CHANGELOG.md"])).toBe(2);
    expect(main(["--bogus", "x"])).toBe(2);
  });

  it("matches the repository's own changelog for a released tag", () => {
    expect(main(["--tag", "v0.0.3"])).toBe(0);
  });

  it("extracts the current v0.0.5 section, including the language-mix fix", () => {
    const changelog = readFileSync(fileURLToPath(new URL("../CHANGELOG.md", import.meta.url)), "utf8");
    const result = extractNotes(changelog, "v0.0.5");
    expect(result.found).toBe(true);
    if (!result.found) return;
    expect(result.body).toContain("Language mix was ordered by category, not by share");
    expect(result.body).toContain("Release notes for this tag would have failed to publish");
    expect(result.body.length).toBeGreaterThan(48_000);
  });
});
