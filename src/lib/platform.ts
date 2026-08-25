export function isMacOS(): boolean {
  if (typeof navigator === "undefined") return false;
  const platform = navigator.platform || "";
  const ua = navigator.userAgent || "";
  return /Mac|Macintosh/.test(platform) || /Mac OS X/.test(ua);
}

export function isTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

export function applyPlatformClass(): void {
  if (typeof document === "undefined") return;
  document.documentElement.classList.toggle("macos", isMacOS());
}
