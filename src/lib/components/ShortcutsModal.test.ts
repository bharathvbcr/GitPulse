import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import ShortcutsModal from "./ShortcutsModal.svelte";

describe("ShortcutsModal", () => {
  it("renders nothing when closed", () => {
    const { body } = render(ShortcutsModal, { props: { isOpen: false } });
    expect(body).not.toContain('role="dialog"');
  });

  it("renders modal dialog with shortcut categories and keycaps when open", () => {
    const { body } = render(ShortcutsModal, { props: { isOpen: true } });
    expect(body).toContain('role="dialog"');
    expect(body).toContain('aria-label="Keyboard Shortcuts"');
    expect(body).toContain("Workspace");
    expect(body).toContain("Navigation");
    expect(body).toContain("Open Command Palette");
  });
});
