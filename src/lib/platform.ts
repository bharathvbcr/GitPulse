/**
 * Pure macOS classification, so callers that already hold a platform/UA pair
 * (the OS classifier below, and tests) get the same answer as the live check
 * instead of a second implementation that can disagree.
 */
export function looksLikeMacOS(platform: string, ua: string, maxTouchPoints: number): boolean {
  // iOS says "like Mac OS X", and desktop-mode iPads report MacIntel.
  // Neither should receive desktop chrome (including traffic-light spacing).
  if (/iPad|iPhone|iPod/.test(ua) || /iPad|iPhone|iPod/.test(platform)) return false;
  if (maxTouchPoints > 1) return false;
  return /Mac/.test(platform) || /Macintosh|Mac OS X/.test(ua);
}

export function isMacOS(): boolean {
  if (typeof navigator === "undefined") return false;
  return looksLikeMacOS(
    navigator.platform || "",
    navigator.userAgent || "",
    navigator.maxTouchPoints,
  );
}

export function isTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

export function applyPlatformClass(): void {
  if (typeof document === "undefined") return;
  document.documentElement.classList.toggle("macos", isMacOS());
}

/**
 * Which operating system the app is running on.
 *
 * `isMacOS()` answers a question about *appearance* — it reads the webview, so
 * it is the right input for fonts, vibrancy and traffic-light spacing. It is
 * the wrong input for deciding whether a feature exists, for two reasons: it
 * collapses Windows and Linux into one "not Mac" bucket, and a user agent says
 * nothing about whether the code backing a feature was compiled in.
 *
 * "unknown" is a real answer, not a failure to be papered over: a host we
 * cannot name must not be told a platform-exclusive feature is available.
 */
export type HostOS = "macos" | "windows" | "linux" | "unknown";

/**
 * The `cmd_host_platform` envelope, mirroring the Rust struct exactly.
 *
 * `os` is `string` and not [`HostOS`] on purpose: Rust sends
 * `std::env::consts::OS`, which can name a host this app does not model. Typing
 * the wire as the narrow union would be a claim the backend does not make, and
 * check:types compares these field types against the Rust struct to keep the
 * two honest — a rename on either side would otherwise read as `undefined`,
 * which is falsy, hiding a capability on a host that has it.
 */
export interface HostPlatform {
  readonly os: string;
  readonly dock_hiding: boolean;
}

/** The same facts with `os` narrowed to a host this app knows how to address. */
export interface HostProfile {
  readonly os: HostOS;
  /** Whether the Dock/taskbar icon can be hidden for menu-bar-only mode. */
  readonly dock_hiding: boolean;
}

export function isHostOS(value: unknown): value is HostOS {
  return value === "macos" || value === "windows" || value === "linux" || value === "unknown";
}

/**
 * Best-effort OS from the webview, for the browser harness and for the first
 * paint before the backend answers.
 *
 * Deliberately conservative about capabilities: it names the OS but never
 * claims a native capability, because the webview cannot know what compiled in.
 */
export function hostOSFromUserAgent(
  platform = typeof navigator === "undefined" ? "" : navigator.platform || "",
  ua = typeof navigator === "undefined" ? "" : navigator.userAgent || "",
  maxTouchPoints = typeof navigator === "undefined" ? 0 : navigator.maxTouchPoints,
): HostOS {
  // Classified from the arguments, never from the live `navigator`: a caller
  // that passes a platform/UA pair must get an answer about that pair.
  if (looksLikeMacOS(platform, ua, maxTouchPoints)) return "macos";
  // Order matters: "Windows NT" contains neither "Linux" nor "X11", but an
  // Android UA contains "Linux" while being neither a Windows nor a desktop
  // Linux host — and it is not a host GitPulse ships to, so it stays unknown.
  if (/Win/.test(platform) || /Windows/.test(ua)) return "windows";
  if (/Android/.test(ua)) return "unknown";
  if (/Linux|X11/.test(platform) || /Linux|X11/.test(ua)) return "linux";
  return "unknown";
}

/**
 * The pre-backend default. macOS is detectable from the webview, so the first
 * paint is already correct there; every other host starts with no native
 * capability claimed and is refined once `cmd_host_platform` answers.
 */
export function fallbackHostPlatform(): HostProfile {
  return { os: hostOSFromUserAgent(), dock_hiding: false };
}

/** Parses the `cmd_host_platform` envelope, falling back on anything unexpected. */
export function parseHostPlatform(value: unknown): HostProfile {
  if (typeof value !== "object" || value === null) return fallbackHostPlatform();
  const raw = value as Record<string, unknown>;
  // An unrecognised OS name (a host Rust knows and this list does not) must
  // read as "unknown" rather than silently inheriting the webview's guess,
  // which would claim a platform the backend just contradicted.
  const os = isHostOS(raw.os) ? raw.os : "unknown";
  return { os, dock_hiding: raw.dock_hiding === true };
}
