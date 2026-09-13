import type { HostOS } from "../platform";

/**
 * The one place platform-specific vocabulary is decided.
 *
 * Every string here was previously hard-coded to macOS inside a component and
 * shown on every host, so a Windows reader was told about the Dock, Finder, and
 * "this Mac". Centralising them means a new platform is one arm of one switch
 * rather than a grep across components — and it makes the wording testable
 * without rendering anything, which matters because these strings live behind
 * `$effect`-gated markup that a server render never reaches.
 */

/** The host's file manager, as its users call it. */
export function fileManagerName(os: HostOS): string {
  switch (os) {
    case "macos":
      return "Finder";
    case "windows":
      return "File Explorer";
    case "linux":
      return "your file manager";
    default:
      return "your file manager";
  }
}

/** Where a background status icon lives on this host. */
export function statusIconLocationName(os: HostOS): string {
  switch (os) {
    case "macos":
      return "menu bar";
    case "windows":
      return "notification area";
    case "linux":
      return "system tray";
    default:
      return "system tray";
  }
}

/**
 * Sentence-case title for the background status-icon setting.
 *
 * Kept beside the location name so the label and the sentences that mention it
 * can never drift into naming different things on the same screen.
 */
export function statusIconSettingLabel(os: HostOS): string {
  const location = statusIconLocationName(os);
  return `${location.charAt(0).toUpperCase()}${location.slice(1)} status icon`;
}

/** The host's own settings app, for "grant this permission over there" copy. */
export function systemSettingsName(os: HostOS): string {
  switch (os) {
    case "macos":
      return "System Settings";
    case "windows":
      return "Windows Settings";
    case "linux":
      return "your desktop settings";
    default:
      return "your system settings";
  }
}

/**
 * Why native desktop notifications are unavailable — keeping the reasons
 * distinct.
 *
 * Three different negatives were previously one message. They are not
 * interchangeable: a reader on a supported OS who has merely denied permission
 * must not be told their platform is unsupported, and a Windows reader must not
 * be sent to macOS Settings to fix something no amount of clicking will fix.
 *
 * `available` is the backend's runtime probe (did the notification centre
 * actually install), never a guess from the OS name.
 */
export function notificationUnavailableReason(
  os: HostOS,
  available: boolean,
  error: string | null,
): string | null {
  if (available) return null;
  if (os !== "macos") {
    return `GitPulse does not deliver desktop notifications on ${osDisplayName(os)} yet. Activity still appears in the in-app inbox.`;
  }
  // On macOS the feature exists, so the probe failing is a real fault worth
  // surfacing verbatim rather than restating as "unsupported".
  return error
    ? `Desktop notifications are unavailable: ${error}`
    : "Desktop notifications could not start. Open the activity inbox, or recheck below.";
}

/**
 * Whether the desktop-notification controls may be shown at all.
 *
 * Split out of the component because everything it decides sits behind an
 * `$effect`, which a server render never runs — so left inline it was the one
 * part of the largest platform leak with no test at all.
 *
 * `native` is the backend's runtime probe and `null` until it answers. Outside
 * Tauri the browser preview keeps its controls, because there the panel is
 * editing saved settings rather than promising an OS banner.
 *
 * Every control in that panel shapes native delivery only — the eligibility
 * query behind `enabled`, `sound`, quiet hours and the mute lists feeds the
 * native queue, and the local-minute lookup quiet hours need errors off macOS.
 * So where delivery is impossible the controls are not merely mislabelled, they
 * are inert, and showing them promises an effect that cannot happen.
 */
export function desktopNotificationsSupported(
  isTauriHost: boolean,
  os: HostOS,
  native: { available: boolean } | null,
): boolean {
  if (!isTauriHost) return true;
  // A host with no implementation cannot deliver whatever a probe says, and this
  // is known before any probe runs — so the panel closes immediately rather than
  // showing controls for one frame and then taking them away.
  if (os === "windows" || os === "linux") return false;
  // macOS, or an OS that could not be named: only the probe can say, and a probe
  // that has not answered is not a finding. Reading `null` as "unsupported" was
  // wrong in both directions — it blinked the controls away on every open, and
  // when the probe failed outright it removed the reader's saved settings with
  // no explanation, because the unavailable reason is also blank while the
  // status is null. Hide only on a probe that ran and said no.
  return native === null ? true : native.available;
}

/** How to phrase a notification-permission prompt for this host. */
export function notificationPermissionHint(os: HostOS): string {
  return os === "macos"
    ? `Allow GitPulse notifications in ${systemSettingsName(os)}, then try again.`
    : `GitPulse does not deliver desktop notifications on ${osDisplayName(os)} yet.`;
}

/**
 * Why closed-app cleanup scheduling is unavailable.
 *
 * The backend reports one boolean for two causes — wrong platform, or a macOS
 * build that is not an installed app bundle. Telling a Windows reader to
 * install a macOS application bundle is the collapse this separates.
 */
export function closedAppSchedulingReason(os: HostOS, supported: boolean): string {
  if (supported) {
    return "Closed-app mode uses a per-user background job and the same saved limits.";
  }
  return os === "macos"
    ? "Closed-app mode is available from an installed macOS application bundle."
    : `Closed-app mode is only available on macOS, not on ${osDisplayName(os)}.`;
}

/** How this host starts an app at sign-in, for the launch-at-login note. */
export function launchAtLoginMechanism(os: HostOS): string {
  switch (os) {
    case "macos":
      return "a per-user LaunchAgent";
    case "windows":
      return "a per-user startup registry entry";
    case "linux":
      return "a per-user autostart entry";
    default:
      return "the host's own startup mechanism";
  }
}

/** The OS as a reader would name it. */
export function osDisplayName(os: HostOS): string {
  switch (os) {
    case "macos":
      return "macOS";
    case "windows":
      return "Windows";
    case "linux":
      return "Linux";
    default:
      return "this platform";
  }
}

/**
 * Rewrites a macOS modifier glyph for hosts that do not use it.
 *
 * The native menu binds `CmdOrCtrl`, which really is Command on macOS and
 * Control elsewhere, so the keys work on every host — only the printed
 * reference was wrong. An entry that already reads "Ctrl" is left alone: those
 * accelerators are literally `Ctrl` on every platform, so substituting would
 * introduce the error this removes.
 */
const GLYPH_WORDS: Readonly<Record<string, string>> = {
  "⌘": "Ctrl",
  "⇧": "Shift",
  "⌥": "Alt",
  "⌃": "Ctrl",
};

export function shortcutKeyLabel(key: string, os: HostOS): string {
  if (os === "macos") return key;
  const direct = GLYPH_WORDS[key];
  if (direct !== undefined) return direct;
  // Composite labels such as "⇧ ← / →" carry a glyph inside other text.
  let mapped = key;
  for (const [glyph, word] of Object.entries(GLYPH_WORDS)) {
    mapped = mapped.replaceAll(glyph, word);
  }
  return mapped;
}

/**
 * Rewrites macOS glyphs inside a compact shortcut string or a sentence.
 *
 * Distinct from `shortcutKeyLabel`, which labels one key for one `<kbd>` chip
 * and so wants a bare word. Here the glyph is a modifier bound to whatever
 * follows it — "⌘T", "⌃`", "⇧F3 for previous" — so the word needs a joining
 * "+": "Ctrl+T", "Ctrl+`", "Shift+F3 for previous".
 *
 * Two cases the previous ad-hoc substitution in the command palette got wrong,
 * and the reason this is one function rather than a `replaceAll` per caller:
 *
 *   - It covered only ⌘ and ⇧, so "⌃`" (toggle the terminal dock) reached
 *     Windows and Linux readers with a macOS glyph intact.
 *   - A glyph already followed by a separator produced a doubled one:
 *     "Ctrl+⇧+Tab" became "Ctrl+Shift++Tab". A "+" between two modifiers is a
 *     separator and is absorbed; a trailing "+" is the key itself ("⌘+" is Zoom
 *     In) and is kept.
 */
export function shortcutTextLabel(text: string, os: HostOS): string {
  if (os === "macos") return text;
  let mapped = text;
  for (const glyph of Object.keys(GLYPH_WORDS)) {
    // A separator "+" directly after a glyph, with something following it, is
    // absorbed so the substitution below does not double it.
    mapped = mapped.replaceAll(new RegExp(`${glyph}\\+(?=.)`, "g"), glyph);
  }
  for (const [glyph, word] of Object.entries(GLYPH_WORDS)) {
    mapped = mapped.replaceAll(glyph, `${word}+`);
  }
  return mapped;
}

/**
 * Picks between two chords that are genuinely different per platform.
 *
 * Distinct from `shortcutTextLabel`, which rewrites ONE chord's notation: the
 * terminal really is bound to ⌘F on macOS and Ctrl+Shift+F elsewhere, because
 * the plain Ctrl+F belongs to the shell. Printing both to everyone — which is
 * what these hints used to do — makes the reader work out which half is theirs,
 * and still shows a Mac glyph to someone who has no Command key.
 */
export function platformChord(macChord: string, otherChord: string, os: HostOS): string {
  return os === "macos" ? macChord : otherChord;
}

export function shortcutKeyLabels(keys: readonly string[], os: HostOS): string[] {
  return keys.map((key) => shortcutKeyLabel(key, os));
}

/** Folder access guidance is descriptive; it never claims an OS permission probe. */
export function repositoryAccessGuidance(os: HostOS): string {
  switch (os) {
    case "macos":
      return "macOS may ask for access when you open repositories in Desktop, Documents, or Downloads. If denied, review GitPulse under System Settings → Privacy & Security → Files and Folders, then open the repository again.";
    case "windows":
      return "On Windows, choose a folder your account can read and write. If access is blocked, review its folder permissions and Windows Security protection, then try opening it again.";
    case "linux":
      return "On Linux, your account needs permission to read the repository and write Git data. Check folder ownership, permissions, and any application confinement if access is denied.";
    default:
      return "This host’s permission controls are unknown. If opening a folder fails, review your operating system’s folder-access controls. This tour does not verify access.";
  }
}
