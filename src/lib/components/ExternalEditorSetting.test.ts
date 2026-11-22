import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import ExternalEditorSetting from "./ExternalEditorSetting.svelte";

describe("ExternalEditorSetting", () => {
  it("offers Auto-detect, VS Code, Cursor and Zed with Auto-detect selected by default", () => {
    const { body } = render(ExternalEditorSetting);
    for (const label of ["Auto-detect", "VS Code", "Cursor", "Zed"]) expect(body).toContain(label);
    expect(body).toMatch(/aria-pressed="true"[^>]*data-active="true"[^>]*>\s*Auto-detect/);
    expect(body).toMatch(/aria-pressed="false"[^>]*data-active="false"[^>]*>\s*Zed/);
    expect(body).toContain("your extensions keep working");
  });
});
