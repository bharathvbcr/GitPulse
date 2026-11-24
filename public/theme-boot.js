(function () {
  // Blocking theme boot: mirrors src/lib/stores/themeStore.ts (key
  // "gitpulse_theme_preference", values light | dark | system-default) so the
  // resolved class is on <html> before first paint. The store stays the
  // runtime owner; this only prevents the launch flash. The boot plate in
  // index.html reads that class, so it is theme-correct from the first frame.
  var root = document.documentElement;
  try {
    var pref = localStorage.getItem("gitpulse_theme_preference");
    var dark =
      pref === "dark" ||
      (pref !== "light" && !(window.matchMedia && window.matchMedia("(prefers-color-scheme: light)").matches));
    root.classList.remove("light");
    root.classList.toggle("dark", dark);
    if (!dark) root.classList.add("light");
  } catch (e) {
    /* keep the default dark markup */
  }

  // Hibernation wake skeleton (?wake=1): reads the layout hint stored right
  // before main WebView teardown so Frame 0 paints a chrome-matched wireframe
  // instead of the cold launch plate.
  try {
    var search = window.location && window.location.search ? window.location.search : "";
    var params = new URLSearchParams(search);
    if (params.get("wake") === "1") {
      var sidebarWidth = 260;
      var hasDock = false;
      var rawHint = localStorage.getItem("gitpulse.wake-skeleton.v1");
      if (rawHint) {
        var parsed = JSON.parse(rawHint);
        if (parsed && typeof parsed === "object") {
          if (typeof parsed.sidebarWidth === "number" && isFinite(parsed.sidebarWidth)) {
            sidebarWidth = Math.max(0, Math.min(600, Math.round(parsed.sidebarWidth)));
          }
          if (parsed.hasTerminalDock === true) {
            hasDock = true;
          }
        }
      }
      var sidebarCss = sidebarWidth + "px";
      root.setAttribute("data-wake", "1");
      root.style.setProperty("--gp-wake-sidebar", sidebarCss);
      if (sidebarWidth <= 0) root.setAttribute("data-wake-sidebar", "0");
      if (hasDock) root.setAttribute("data-wake-dock", "1");

      var stampBootPlate = function () {
        var plate = document.getElementById("gp-boot");
        if (!plate) return;
        plate.setAttribute("data-wake", "1");
        plate.style.setProperty("--gp-wake-sidebar", sidebarCss);
        if (sidebarWidth <= 0) plate.setAttribute("data-wake-sidebar", "0");
        if (hasDock) plate.setAttribute("data-wake-dock", "1");
      };
      if (document.getElementById("gp-boot")) {
        stampBootPlate();
      } else if (document.addEventListener) {
        document.addEventListener("DOMContentLoaded", stampBootPlate, { once: true });
      }
    }
  } catch (e) {
    /* keep cold boot plate on restricted storage or malformed hint */
  }

  // The plate must never trap the window: Splash.svelte removes it when the
  // shell is up; if the app never gets that far (a failed chunk, a thrown
  // boot), this takes it away so whatever is underneath, error included, shows.
  setTimeout(function () {
    var plate = document.getElementById("gp-boot");
    if (plate && plate.parentNode) plate.parentNode.removeChild(plate);
    root.removeAttribute("data-wake");
    root.removeAttribute("data-wake-sidebar");
    root.removeAttribute("data-wake-dock");
  }, 6000);
})();
