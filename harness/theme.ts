/**
 * Sets the theme and waits until the root element carries it.
 *
 * `themeStore.setTheme` applies the class inside a View Transition's update
 * callback whenever motion is allowed (`applyWithTransition` in
 * src/lib/stores/themeStore.ts), and that callback runs a frame or more later.
 * Under Reduce Motion it applied at once, which is how a page could read the
 * root's classes straight after `setTheme` + one settle: on the WebKit runner
 * with motion on, appearance read "macos light" after `setTheme("dark")`.
 * Waiting on the class itself is what a page means, and it is bounded, so a
 * theme that never lands still fails by name.
 */
export async function setThemeAndLand(
  store: { setTheme(theme: "light" | "dark"): void },
  theme: "light" | "dark",
  deadlineMs = 3000,
): Promise<void> {
  store.setTheme(theme);
  const root = document.documentElement;
  const other = theme === "dark" ? "light" : "dark";
  const started = performance.now();
  while (!root.classList.contains(theme) || root.classList.contains(other)) {
    if (performance.now() - started > deadlineMs) {
      throw new Error(`theme "${theme}" did not land within ${deadlineMs} ms: root classes "${root.className}"`);
    }
    await new Promise((resolve) => setTimeout(resolve, 16));
  }
}
