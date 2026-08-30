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
    expect(body).toContain('aria-label="Graph width"');
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
    expect(body).toContain("Balanced");
    expect(body).toContain("Wide");
    expect(body).toContain("Full");
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

describe("SettingsModal automatic coverage toggle", () => {
  it("offers automatic coverage generation as an explicit opt-in, off by default", () => {
    const { body } = render(SettingsModal, { props: { isOpen: true } });
    expect(body).toContain(
      'aria-label="Automatically generate coverage for repositories that have none"',
    );
    // Rendered from the stored preference, which defaults to off.
    expect(body).toContain('aria-checked="false"');
  });

  it("states the cost before the user turns it on", () => {
    // Running a repository's test suites and writing artifacts into its
    // working tree is not what a settings toggle is normally assumed to do.
    // Scoped to this section: the Updates section also opens "Off by default",
    // and an unscoped assertion passed with the coverage copy deleted.
    const { body } = render(SettingsModal, { props: { isOpen: true } });
    const section = body.slice(
      body.indexOf("Generate coverage automatically"),
      body.indexOf("Check for new releases"),
    );
    expect(section).not.toBe("");
    expect(section).toContain("Off by default");
    expect(section).toContain("writes coverage artifacts into the working tree");
    expect(section).toContain("never reported as a clean result");
  });
});
