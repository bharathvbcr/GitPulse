(function () {
  // Blocking theme boot: mirrors src/lib/stores/themeStore.ts (key
  // "gitpulse_theme_preference", values light | dark | system-default) so the
  // resolved class is on <html> before first paint. The store stays the
  // runtime owner; this only prevents the launch flash. The boot plate in
  // index.html reads that class, so it is theme-correct from the first frame.
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

  // The plate must never trap the window: Splash.svelte removes it when the
  // shell is up; if the app never gets that far (a failed chunk, a thrown
  // boot), this takes it away so whatever is underneath, error included, shows.
  setTimeout(function () {
    var plate = document.getElementById("gp-boot");
    if (plate && plate.parentNode) plate.parentNode.removeChild(plate);
  }, 6000);
})();
