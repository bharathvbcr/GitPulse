import { isTauri } from "../platform";

export async function syncWindowChrome(title: string, badgeCount: number): Promise<void> {
  if (!isTauri()) return;
  try {
    const { getCurrentWindow } = await import("@tauri-apps/api/window");
    const window = getCurrentWindow();
    await window.setTitle(title);
    await window.setBadgeCount(badgeCount > 0 ? badgeCount : undefined);
  } catch {
    /* chrome updates require a live Tauri window */
  }
}

export function repoWindowTitle(path: string | null, branch: string | null): string {
  if (!path) return "GitPulse";
  const name = path.split(/[\\/]/).filter(Boolean).pop() || path;
  return branch ? `${name} — ${branch}` : name;
}
