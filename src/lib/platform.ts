export function isMacOS(): boolean {
  if (typeof navigator === "undefined") return false;
  const platform = navigator.platform || "";
  const ua = navigator.userAgent || "";
  // iOS says "like Mac OS X", and desktop-mode iPads report MacIntel.
  // Neither should receive desktop chrome (including traffic-light spacing).
  if (/iPad|iPhone|iPod/.test(ua) || /iPad|iPhone|iPod/.test(platform)) return false;
  if (navigator.maxTouchPoints > 1) return false;
  return /Mac/.test(platform) || /Macintosh|Mac OS X/.test(ua);
}

export function isTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

export function applyPlatformClass(): void {
  if (typeof document === "undefined") return;
  document.documentElement.classList.toggle("macos", isMacOS());
}
