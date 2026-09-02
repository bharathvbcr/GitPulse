import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import LanguageBar from "./LanguageBar.svelte";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "LanguageBar.svelte"),
  "utf8",
);

describe("LanguageBar", () => {
  it("renders without crashing when stats are empty", () => {
    const { body } = render(LanguageBar);
    expect(body).toBeDefined();
  });

  it("integrates LanguageLogo for visual language identification", () => {
    expect(source).toContain("LanguageLogo");
    expect(source).toContain("language={lang.language}");
  });

  it("supports interactive language click to jump to files", () => {
    expect(source).toContain("handleLanguageClick");
    expect(source).toContain('repoStore.setActiveTab("files")');
    expect(source).toContain("gitpulse:filter-lang");
  });
});
