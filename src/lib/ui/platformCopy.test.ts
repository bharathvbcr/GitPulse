import { describe, expect, it } from "vitest";
import type { HostOS } from "../platform";
import {
  closedAppSchedulingReason,
  desktopNotificationsSupported,
  fileManagerName,
  launchAtLoginMechanism,
  notificationPermissionHint,
  notificationUnavailableReason,
  osDisplayName,
  shortcutKeyLabel,
  shortcutKeyLabels,
  shortcutTextLabel,
  statusIconLocationName,
  systemSettingsName,
} from "./platformCopy";

const EVERY_OS: readonly HostOS[] = ["macos", "windows", "linux", "unknown"];
const NON_MAC: readonly HostOS[] = ["windows", "linux", "unknown"];

/**
 * The whole point of this module: no macOS vocabulary may reach a host that
 * does not use it. Derived from one word list rather than asserted per string,
 * so a new function added to the module is covered without being hand-listed.
 */
const MAC_WORDS = [/\bmacOS\b/, /\bMac\b/, /\bFinder\b/, /\bDock\b/, /LaunchAgent/, /\bmenu bar\b/];

describe("platform vocabulary", () => {
  it.each(NON_MAC)("leaks no macOS words into copy for %s", (os) => {
    const strings = [
      fileManagerName(os),
      statusIconLocationName(os),
      systemSettingsName(os),
      notificationPermissionHint(os),
      launchAtLoginMechanism(os),
      osDisplayName(os),
      notificationUnavailableReason(os, false, null) ?? "",
    ];
    for (const text of strings) {
      for (const word of MAC_WORDS) {
        expect(text, `"${text}" mentions ${word} on ${os}`).not.toMatch(word);
      }
    }
  });

  it.each(EVERY_OS)("never returns an empty label for %s", (os) => {
    expect(fileManagerName(os).length).toBeGreaterThan(0);
    expect(statusIconLocationName(os).length).toBeGreaterThan(0);
    expect(systemSettingsName(os).length).toBeGreaterThan(0);
    expect(launchAtLoginMechanism(os).length).toBeGreaterThan(0);
    expect(osDisplayName(os).length).toBeGreaterThan(0);
  });

  it("names each host's file manager and status-icon home", () => {
    expect(fileManagerName("macos")).toBe("Finder");
    expect(fileManagerName("windows")).toBe("File Explorer");
    expect(statusIconLocationName("macos")).toBe("menu bar");
    expect(statusIconLocationName("windows")).toBe("notification area");
    expect(statusIconLocationName("linux")).toBe("system tray");
  });
});

describe("notification availability reasons", () => {
  it("says nothing when the feature is available", () => {
    for (const os of EVERY_OS) {
      expect(notificationUnavailableReason(os, true, null)).toBeNull();
      expect(notificationUnavailableReason(os, true, "ignored")).toBeNull();
    }
  });

  /**
   * The honesty invariant. "Unsupported platform", "supported but the probe
   * failed", and "supported, running, permission denied" are three different
   * facts; collapsing them tells a reader whose Mac is fine that their hardware
   * is the problem, or tells a Windows reader to change a macOS setting.
   */
  it("keeps an unsupported platform distinct from a macOS fault", () => {
    const windows = notificationUnavailableReason("windows", false, null);
    expect(windows).toMatch(/Windows/);
    expect(windows).not.toMatch(/macOS/);
    expect(windows).toMatch(/inbox/);

    const macFault = notificationUnavailableReason("macos", false, "center refused to install");
    expect(macFault).toMatch(/center refused to install/);
    expect(macFault).not.toMatch(/does not deliver/);
  });

  it("surfaces a macOS probe failure verbatim rather than restating it", () => {
    expect(notificationUnavailableReason("macos", false, "no bundle identifier")).toContain(
      "no bundle identifier",
    );
  });

  it("sends only macOS readers to a permission screen", () => {
    expect(notificationPermissionHint("macos")).toMatch(/System Settings/);
    for (const os of NON_MAC) {
      expect(notificationPermissionHint(os)).not.toMatch(/Settings, then try again/);
    }
  });
});

describe("closed-app scheduling reason", () => {
  it("distinguishes a missing app bundle from an unsupported platform", () => {
    expect(closedAppSchedulingReason("macos", false)).toMatch(/application bundle/);
    const windows = closedAppSchedulingReason("windows", false);
    expect(windows).toMatch(/only available on macOS/);
    expect(windows).toMatch(/Windows/);
    expect(windows).not.toMatch(/application bundle/);
  });

  it("explains the mechanism without naming an OS when it works", () => {
    for (const os of EVERY_OS) {
      const text = closedAppSchedulingReason(os, true);
      expect(text).toMatch(/background job/);
      expect(text).not.toMatch(/macOS/);
    }
  });
});

describe("shortcut key labels", () => {
  it("leaves macOS glyphs untouched on macOS", () => {
    expect(shortcutKeyLabels(["⌘", "⇧", "O"], "macos")).toEqual(["⌘", "⇧", "O"]);
  });

  it.each(NON_MAC)("translates every macOS glyph on %s", (os) => {
    expect(shortcutKeyLabels(["⌘", "⇧", "O"], os)).toEqual(["Ctrl", "Shift", "O"]);
    expect(shortcutKeyLabel("⌥", os)).toBe("Alt");
    expect(shortcutKeyLabel("⌃", os)).toBe("Ctrl");
  });

  it("translates a glyph embedded in a composite label", () => {
    expect(shortcutKeyLabel("⇧ ← / →", "windows")).toBe("Shift ← / →");
  });

  /**
   * `Ctrl+Tab` is literally Control on every platform — the native menu binds
   * `Ctrl`, not `CmdOrCtrl`. Rewriting it would introduce an error.
   */
  it("leaves platform-neutral key names alone", () => {
    for (const os of EVERY_OS) {
      expect(shortcutKeyLabels(["Ctrl", "Tab"], os)).toEqual(["Ctrl", "Tab"]);
      expect(shortcutKeyLabel("Enter", os)).toBe("Enter");
      expect(shortcutKeyLabel("1–9", os)).toBe("1–9");
    }
  });

  it("never leaves a macOS glyph in a non-macOS label", () => {
    const glyphs = /[⌘⌥⌃⇧]/;
    for (const os of NON_MAC) {
      for (const key of ["⌘", "⌥", "⌃", "⇧", "⌘ ⇧", "⇧ ← / →"]) {
        expect(shortcutKeyLabel(key, os)).not.toMatch(glyphs);
      }
    }
  });
});

describe("compact shortcut and prose labels", () => {
  it("leaves macOS notation untouched on macOS", () => {
    for (const text of ["⌘T", "⌃`", "⌘⇧W", "Next match (⇧F3 for previous)"]) {
      expect(shortcutTextLabel(text, "macos")).toBe(text);
    }
  });

  it.each([
    ["⌘T", "Ctrl+T"],
    ["⌘Enter", "Ctrl+Enter"],
    ["⌘,", "Ctrl+,"],
    ["⌘⇧W", "Ctrl+Shift+W"],
    ["⌘0", "Ctrl+0"],
  ])("maps %s to %s", (input, expected) => {
    expect(shortcutTextLabel(input, "windows")).toBe(expected);
  });

  /**
   * The palette's old substitution covered only ⌘ and ⇧, so the terminal dock's
   * ⌃` reached Windows and Linux with a macOS glyph intact.
   */
  it("maps the control glyph the palette used to miss", () => {
    expect(shortcutTextLabel("⌃`", "windows")).toBe("Ctrl+`");
    expect(shortcutTextLabel("⌃`", "linux")).toBe("Ctrl+`");
  });

  /**
   * A "+" between two modifiers is a separator and must be absorbed, or the
   * substitution doubles it ("Ctrl+Shift++Tab").
   */
  it("absorbs a separator already present between modifiers", () => {
    expect(shortcutTextLabel("Ctrl+⇧+Tab", "windows")).toBe("Ctrl+Shift+Tab");
    expect(shortcutTextLabel("Ctrl+⇧+←", "windows")).toBe("Ctrl+Shift+←");
    expect(shortcutTextLabel("Ctrl+⇧+→", "windows")).toBe("Ctrl+Shift+→");
  });

  /** A trailing "+" is the key itself — Zoom In is Cmd and the "+" key. */
  it("keeps a trailing plus that is the key", () => {
    expect(shortcutTextLabel("⌘+", "windows")).toBe("Ctrl++");
    expect(shortcutTextLabel("⌘−", "windows")).toBe("Ctrl+−");
  });

  it("maps a glyph embedded in a sentence", () => {
    expect(shortcutTextLabel("Next match (⇧F3 for previous)", "windows")).toBe(
      "Next match (Shift+F3 for previous)",
    );
  });

  it("leaves text with no glyph unchanged", () => {
    for (const os of EVERY_OS) {
      expect(shortcutTextLabel("Ctrl+Tab", os)).toBe("Ctrl+Tab");
      expect(shortcutTextLabel("Stage all changes and commit", os)).toBe(
        "Stage all changes and commit",
      );
    }
  });

  it("never leaves a macOS glyph behind off macOS", () => {
    for (const os of NON_MAC) {
      for (const text of ["⌘T", "⌃`", "⌘⇧W", "⌥↑", "Ctrl+⇧+Tab", "(⇧F3)"]) {
        expect(shortcutTextLabel(text, os)).not.toMatch(/[⌘⌥⌃⇧]/);
      }
    }
  });
});

describe("whether the desktop-notification controls may be shown", () => {
  /**
   * The gate is the backend's runtime probe, never the OS name. On macOS the
   * feature exists but the notification centre can still fail to install, and
   * that is a fault to report rather than a platform limit.
   */
  it("follows the probe on a host that has the feature", () => {
    expect(desktopNotificationsSupported(true, "macos", { available: true })).toBe(true);
    expect(desktopNotificationsSupported(true, "macos", { available: false })).toBe(false);
  });

  /**
   * A host with no implementation is settled before any probe runs, so the panel
   * closes at once instead of offering controls for a frame and retracting them.
   */
  it("denies on a host with no implementation, probe or not", () => {
    for (const os of ["windows", "linux"] as const) {
      expect(desktopNotificationsSupported(true, os, null)).toBe(false);
      expect(desktopNotificationsSupported(true, os, { available: true })).toBe(false);
    }
  });

  /**
   * "Not probed yet" is not a finding, and must not read like a probe that ran
   * and said no. Reading `null` as unsupported was wrong twice over: it blinked
   * the controls away on every open, and on a failed probe it took the reader's
   * saved settings away with nothing said -- the unavailable reason is empty
   * while the status is null, so the panel went silent rather than explaining.
   */
  it("waits for the probe rather than treating silence as a refusal", () => {
    expect(desktopNotificationsSupported(true, "macos", null)).toBe(true);
    expect(desktopNotificationsSupported(true, "unknown", null)).toBe(true);
  });

  /**
   * The browser preview is editing saved settings rather than promising an OS
   * banner, and says so in its own line, so it keeps its controls.
   */
  it("keeps the preview's controls outside the desktop app", () => {
    expect(desktopNotificationsSupported(false, "windows", null)).toBe(true);
    expect(desktopNotificationsSupported(false, "linux", { available: false })).toBe(true);
  });

  /**
   * The invariant behind both helpers: the panel never withdraws its controls
   * without saying why. Every combination that hides must yield a reason, and
   * every combination that shows must yield none -- which is why the component
   * derives the reason from `supported` instead of from the probe.
   */
  it("never hides the controls silently", () => {
    const cases: Array<[HostOS, { available: boolean; error: string | null } | null]> = [
      ["macos", null],
      ["macos", { available: true, error: null }],
      ["macos", { available: false, error: "notification centre failed to install" }],
      ["macos", { available: false, error: null }],
      ["windows", null],
      ["windows", { available: false, error: null }],
      ["linux", null],
      ["unknown", null],
      ["unknown", { available: false, error: null }],
    ];
    for (const [os, native] of cases) {
      const supported = desktopNotificationsSupported(true, os, native);
      const reason = supported
        ? null
        : notificationUnavailableReason(os, native?.available ?? false, native?.error ?? null);
      expect(supported ? reason === null : Boolean(reason?.trim()), `${os} / ${JSON.stringify(native)}`).toBe(true);
    }
  });
});
