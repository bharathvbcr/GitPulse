import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import StatusBar from "./StatusBar.svelte";

describe("StatusBar", () => {
  it("renders status bar role and shortcut indicators", () => {
    const { body } = render(StatusBar);
    expect(body).toContain('role="status"');
    expect(body).toContain('aria-label="Repository Status Bar"');
    expect(body).toContain("Palette");
    expect(body).toContain("Shortcuts");
  });
});
