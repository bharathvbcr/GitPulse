import { isTauri } from "../platform";

interface AppliedChrome {
  title: string;
  badge: number;
}

// Last chrome known to be on screen; null until a sync actually lands, so a
// failed write is never mistaken for success.
let applied: AppliedChrome | null = null;
// Monotonic per-call token — only the newest sync may touch the window.
let latest = 0;

export async function syncWindowChrome(title: string, badgeCount: number): Promise<void> {
  if (!isTauri()) return;
  const badge = badgeCount > 0 ? badgeCount : 0;
  // Identical chrome already applied: skip the IPC round-trip entirely.
  if (applied && applied.title === title && applied.badge === badge) return;

  const token = ++latest;
  try {
    const { getCurrentWindow } = await import("@tauri-apps/api/window");
    const window = getCurrentWindow();
    // Re-checked after every await: a newer sync supersedes this one, so a
    // slow stale call bails instead of letting an old badge win.
    if (token !== latest) return;
    await window.setTitle(title);
    if (token !== latest) return;
    await window.setBadgeCount(badge > 0 ? badge : undefined);
    if (token !== latest) return;
    applied = { title, badge };
  } catch {
    applied = null;
    /* chrome updates require a live Tauri window */
  }
}

export function repoWindowTitle(path: string | null, branch: string | null): string {
  if (!path) return "GitPulse";
  const name = path.split(/[\\/]/).filter(Boolean).pop() || path;
  return branch ? `${name} — ${branch}` : name;
}
