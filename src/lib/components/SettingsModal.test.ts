import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import SettingsModal from "./SettingsModal.svelte";

describe("SettingsModal", () => {
  it("renders nothing while closed", () => {
    const { body } = render(SettingsModal, { props: { isOpen: false } });
    expect(body).not.toContain('role="dialog"');
  });

  it("exposes theme and density as labelled stateful controls", () => {
    const { body } = render(SettingsModal, { props: { isOpen: true } });

    expect(body).toContain('role="dialog"');
    expect(body).toContain('aria-label="Theme appearance"');
    expect(body).toContain('aria-label="Branch spacing"');
    // Every option is a pressed/unpressed toggle.
    expect(body).toContain('aria-pressed="true"');
    expect(body).toContain('aria-pressed="false"');
    // Theme options: system is the default preference in a fresh store.
    expect(body).toContain("System");
    expect(body).toContain("Light");
    expect(body).toContain("Dark");
    // Density options (sole owner since the FilterBar control moved here).
    expect(body).toContain("Spacious");
    expect(body).toContain("Compact");
  });

  it("owns the interface visibility switches", () => {
    const { body } = render(SettingsModal, { props: { isOpen: true } });

    expect(body).toContain('role="switch"');
    expect(body).toContain('aria-label="Show language statistics bar"');
    expect(body).toContain('aria-label="Show MANVI status badges"');
    // Defaults are on, so both switches render checked.
    expect(body).toContain('aria-checked="true"');
  });
});
