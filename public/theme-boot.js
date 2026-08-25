(function () {
  // Blocking theme boot: mirrors src/lib/stores/themeStore.ts (key
  // "gitpulse_theme_preference", values light | dark | system-default) so the
  // resolved class is on <html> before first paint. The store stays the
  // runtime owner; this only prevents the launch flash.
  try {
    var pref = localStorage.getItem("gitpulse_theme_preference");
    var dark =
      pref === "dark" ||
      (pref !== "light" && !(window.matchMedia && window.matchMedia("(prefers-color-scheme: light)").matches));
    var root = document.documentElement;
    root.classList.remove("light");
    root.classList.toggle("dark", dark);
    if (!dark) root.classList.add("light");
  } catch (e) {
    /* keep the default dark markup */
  }
})();
