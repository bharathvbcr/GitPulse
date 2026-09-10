import { tick } from "svelte";

/** Exercise the shared production component in the existing interactive fixture. */
export async function checkStatusPopover() {
  const results: { name: string; pass: boolean }[] = [];
  const check = (name: string, pass: boolean) => results.push({ name, pass });
  const settle = async () => { await tick(); await new Promise(resolve => setTimeout(resolve, 220)); await tick(); };
  const element = (selector: string) => {
    const value = document.querySelector(selector);
    if (!(value instanceof HTMLElement)) throw new Error(`Missing fixture element: ${selector}`);
    return value;
  };
  const button = (selector: string) => {
    const value = element(selector);
    if (!(value instanceof HTMLButtonElement)) throw new Error(`Expected button: ${selector}`);
    return value;
  };
  const click = async (selector: string) => { button(selector).click(); await settle(); };
  const key = async (key: string) => { window.dispatchEvent(new KeyboardEvent("keydown", { key, cancelable: true })); await settle(); };
  const select = async (index: number, value: string) => {
    const input = document.querySelectorAll("select")[index];
    if (!input) throw new Error("Fixture selector missing");
    input.value = value; input.dispatchEvent(new Event("change", { bubbles: true })); await settle();
  };
  const action = (id: string) => element(".action").textContent === `Last action: ${id}`;
  const metrics = () => [...document.querySelectorAll<HTMLButtonElement>(".metric")];
  const details = () => document.getElementById("status-details");
  const fits = () => {
    const panel = element(".panel");
    return panel.scrollWidth <= panel.clientWidth + 1 && panel.getBoundingClientRect().height < 624;
  };
  const backdrop = () => getComputedStyle(element(".panel")).getPropertyValue("backdrop-filter");
  const contrastOnGlass = () => {
    const canvas = document.createElement("canvas"); canvas.width = canvas.height = 1;
    const context = canvas.getContext("2d");
    if (!context) throw new Error("Color sampling unavailable");
    const color = (value: string) => {
      context.clearRect(0, 0, 1, 1); context.fillStyle = value; context.fillRect(0, 0, 1, 1);
      return [...context.getImageData(0, 0, 1, 1).data];
    };
    const blend = (front: number[], back: number[]) => front.slice(0, 3).map((c, i) => c * front[3] / 255 + back[i] * (1 - front[3] / 255));
    const luminance = (rgb: number[]) => rgb.slice(0, 3).map(c => c / 255).map(c => c <= .04045 ? c / 12.92 : ((c + .055) / 1.055) ** 2.4).reduce((value, c, i) => value + c * [.2126, .7152, .0722][i], 0);
    const style = getComputedStyle(element(".status-shell"));
    const base = color(getComputedStyle(element(".panel")).backgroundColor);
    const sheen = color(style.getPropertyValue("--sheen"));
    const backgrounds = [0, 255].flatMap(c => {
      const tinted = blend(base, [c, c, c]);
      return [tinted, blend(sheen, tinted)];
    });
    return ["--text", "--muted", "--blue", "--green", "--amber"].every(token => {
      const foreground = luminance(color(style.getPropertyValue(token)));
      return backgrounds.every(rgb => {
        const background = luminance(rgb);
        return (Math.max(foreground, background) + .05) / (Math.min(foreground, background) + .05) >= 4.5;
      });
    });
  };
  const materialFallback = () => {
    // Activate the production media rule in this isolated fixture; this
    // exercises the real CSS cascade without changing the user's OS settings.
    for (const sheet of document.styleSheets) {
      for (const rule of sheet.cssRules) {
        if (rule instanceof CSSMediaRule && rule.conditionText.includes("prefers-reduced-transparency") && rule.cssText.includes("status-shell")) return rule;
      }
    }
    throw new Error("Status material accessibility rule missing");
  };

  try {
    await settle();
    check("first glance has three equal metric cards", metrics().length === 3 && new Set(metrics().map(m => m.offsetWidth)).size === 1);
    check("numbers use the reference's large readable scale", getComputedStyle(element(".metric strong")).fontSize === "27px");
    check("the preview renders an actual backdrop blur", getComputedStyle(element(".panel")).getPropertyValue("backdrop-filter").includes("blur(34px)"));
    check("the panel leaves room for the blurred backdrop", getComputedStyle(element(".panel")).backgroundColor === "rgba(250, 251, 252, 0.8)");
    check("light glass text and accents retain contrast over black and white backdrops", contrastOnGlass());
    check("collapsed panel fits the native window", fits() && Math.abs(element(".panel").getBoundingClientRect().width - 360) <= 1 && element(".panel").offsetHeight < 360);
    check("reference counts render from the snapshot", metrics().map(m => m.querySelector("strong")?.textContent).join(",") === "12,4,0");
    check("last-fetch comparison remains explicit", element(".sync").textContent?.includes("last fetch") === true && !!document.querySelector('[aria-label="3 ahead"]'));
    check("stashes and shortcuts are initially disclosed", !details() && !document.querySelector('[aria-label="2 stashes"], [aria-label="Go"], [aria-label="Tools"], [aria-label="Command palette"]'));
    check("only one primary action is visible", document.querySelectorAll(".primary").length === 1 && element(".primary").textContent?.includes("Review changes") === true);
    check("zero conflicts cannot navigate", button('[aria-label="0 conflicts"]').disabled);
    await click(".primary"); check("review opens the existing work view", action("section:work:overview"));
    await click(".details-toggle");
    check("Details reveals the available stashes and utilities", !!details() && !button('[aria-label="2 stashes"]').disabled && !!document.querySelector('[aria-label="Tools"]'));
    check("Details remains bounded with the footer reachable", fits() && element(".details").offsetHeight <= 190 && getComputedStyle(element(".details")).overflowY === "auto");
    check("Details is keyboard scrollable", element(".details").tabIndex === 0);
    await click('[aria-label="2 stashes"]'); check("stashes reuse the work view", action("section:work:overview"));
    await click('[aria-label="Command palette"]'); check("the palette remains available inside Details", action("palette"));
    await click('button[title="History"]'); check("History shortcut retains its destination", action("section:history:graph"));
    await click('button[title="Copy branch"]'); check("copy keeps its action and local feedback", action("copy-branch") && button('button[title="Copy branch"]').textContent?.includes("Copied") === true);
    await key("Escape"); check("Escape collapses Details first", !details() && action("copy-branch"));
    await key("Escape"); check("Escape then dismisses the popover", action("dismiss"));
    await key("r"); check("R still refreshes", action("refresh"));
    await key("2"); check("metric shortcuts still open work", action("section:work:overview"));
    await click('[aria-label="Choose repository"]');
    check("chooser exposes both repository paths", document.querySelectorAll(".repositories button").length === 2 && element(".repositories").textContent?.includes("/Projects/ScholarLM") === true);
    await click('.repositories button[title="/Projects/ScholarLM"]');
    check("switching updates the header and closes the chooser", action("activate-repo:/Projects/ScholarLM") && element(".repository").textContent?.includes("ScholarLM") === true && !document.querySelector(".repositories"));
    await click('[aria-label="Choose repository"]'); await key("Escape");
    check("Escape collapses the chooser before dismissing", !document.querySelector(".repositories") && action("activate-repo:/Projects/ScholarLM"));
    await select(1, "conflicts");
    check("conflicts receive a specific headline and card", element("h1").textContent === "Conflicts need your attention" && button('[aria-label="2 conflicts"]').classList.contains("attention"));
    await click(".primary"); check("the conflict action opens Resolve", action("section:work:resolve"));
    await select(1, "clean");
    check("clean state shows real zeros and disables all cards", metrics().every(m => m.disabled && m.querySelector("strong")?.textContent === "0"));
    await click(".primary"); check("clean state offers history", action("section:history:graph"));
    await select(1, "loading");
    check("loading shows unknown values and blocks retry", metrics().every(m => m.disabled && m.querySelector("strong")?.textContent === "—") && button(".primary").disabled);
    await select(1, "unavailable");
    check("failure offers recovery and reports the watcher", !button(".primary").disabled && element(".live").textContent === "Not live" && metrics().every(m => m.querySelector("strong")?.textContent === "—"));
    await click(".primary"); check("retry uses the existing refresh route", action("refresh"));
    await select(1, "empty");
    check("empty state offers opening and cloning without metrics", metrics().length === 0 && element(".primary").textContent?.includes("Open repository") === true);
    await click(".secondary"); check("clone remains accessible", action("clone"));
    await select(1, "long names");
    check("long names truncate without widening the panel", fits() && getComputedStyle(element(".branch span")).textOverflow === "ellipsis" && element(".branch").title.includes("many-segments"));
    await select(0, "dark");
    check("dark mode uses a distinct readable glass surface", getComputedStyle(element(".panel")).backgroundColor === "rgba(32, 35, 41, 0.8)" && getComputedStyle(element(".status-shell")).color === "rgb(240, 242, 245)");
    check("dark glass text and accents retain contrast over black and white backdrops", contrastOnGlass());
    const fallback = materialFallback();
    const media = fallback.media.mediaText;
    check("all three accessibility preferences share the opaque fallback", ["prefers-reduced-transparency", "prefers-contrast", "forced-colors"].every(query => media.includes(query)));
    fallback.media.mediaText = "all"; await settle();
    check("dark accessibility fallback is opaque and unfiltered", getComputedStyle(element(".panel")).backgroundColor === "rgb(32, 35, 41)" && backdrop() === "none" && getComputedStyle(element(".panel")).backgroundImage === "none");
    await select(0, "light");
    check("light accessibility fallback is opaque and unfiltered", getComputedStyle(element(".panel")).backgroundColor === "rgb(250, 251, 252)" && backdrop() === "none");
    fallback.media.mediaText = media; await settle();
    check("restoring the preference restores glass without remounting", backdrop().includes("blur(34px)") && getComputedStyle(element(".panel")).backgroundColor.endsWith("0.8)"));
    await select(2, "opaque");
    check("the non-native fallback stays opaque", backdrop() === "none" && getComputedStyle(element(".panel")).backgroundColor === "rgb(250, 251, 252)");
    await select(2, "preview");
    const shell = element(".status-shell");
    shell.setAttribute("data-material", "native"); await settle();
    check("native material does not pay for a second CSS blur", backdrop() === "none" && getComputedStyle(element(".panel")).backgroundColor.endsWith("0.8)"));
    check("native material has no outer painted gutter", getComputedStyle(shell).padding === "0px" && getComputedStyle(element(".panel")).boxShadow === "none");
    shell.setAttribute("data-material", "preview"); await settle();
    await select(0, "dark");
    await select(1, "busy");
    check("active work remains visible in the headline", element("h1").textContent === "Fetching…");
    await click(".details-toggle");
    check("Details retains background-work insights", element('[aria-label="Workspace insights"]').textContent?.includes("1 running elsewhere") === true);
    await select(1, "operation");
    check("parked operations remain visible in both layers", element("h1").textContent?.includes("Merge in progress") === true && element('[aria-label="Workspace insights"]').textContent?.includes("Merge in progress") === true);
    await key("Escape"); await select(0, "light"); await select(1, "changes");
    await click('[aria-label="Choose repository"]'); await click('.repositories button[title="/Projects/GitPulse"]');
    await click('[aria-label="Settings"]'); check("footer settings keeps its route", action("settings"));
    await click(".open-app"); check("footer restores GitPulse", action("show"));
    await click('[aria-label="Quit GitPulse"]'); check("footer retains guarded quit dispatch", action("quit"));
  } catch (error) {
    check(error instanceof Error ? error.message : String(error), false);
  }
  document.documentElement.setAttribute("data-gp-result", encodeURIComponent(JSON.stringify({ results })));
  const report = new URLSearchParams(location.search).get("report");
  if (report) await fetch(report, { method: "POST", body: document.documentElement.outerHTML });
}
