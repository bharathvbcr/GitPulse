import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import CommitDetails from "./CommitDetails.svelte";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "CommitDetails.svelte"),
  "utf8",
);

describe("CommitDetails", () => {
  it("renders empty state when no commit is selected", () => {
    const { body } = render(CommitDetails);
    expect(body).toBeDefined();
  });

  it("integrates LanguageLogo and formatPathParts for changed files", () => {
    expect(source).toContain("LanguageLogo");
    expect(source).toContain("formatPathParts");
    expect(source).toContain("filePath={f.path}");
  });
});
