import { tick } from "svelte";

/** Exercise the shared production component in the existing interactive fixture. */
export async function checkStatusPopover() {
  const results: { name: string; pass: boolean }[] = [];
  const check = (name: string, pass: boolean) => results.push({ name, pass });
  /**
   * Every animation that can still change what the next probe reads.
   *
   * `playState === "running"` is the wrong question in both directions. A
   * transition Svelte has scheduled but not yet begun has nothing running, and
   * an animation that has reached its end reports `finished` for the whole gap
   * between its last frame and the frame that dispatches its finish event —
   * and that event is where Svelte does the cleanup these probes depend on. It
   * is there that the slide's inline `overflow:hidden` is reverted and the
   * outroing node leaves the DOM. Presence spans both, because Svelte cancels
   * the animation from the same callback, so the animation outlives every
   * intermediate state a probe must not measure.
   *
   * The refresh spinner and the busy pulse never end. Counting those could
   * only ever exhaust the wait, so an endless animation is not something to
   * wait for; anything with a finite end is.
   */
  const animating = () => document.getAnimations()
    .filter(animation => Number.isFinite(Number(animation.effect?.getComputedTiming().endTime)));
  const describeAnimating = () => {
    // One theme change starts a colour transition on every button, so the full
    // list is unreadable in a CI log and says nothing the first few do not.
    // The count still travels, because "3 shown" and "3 running" are different
    // facts and only one of them is a reason to stop looking.
    const all = animating().map(animation => {
      const target = animation.effect instanceof KeyframeEffect ? animation.effect.target : null;
      const name = animation instanceof CSSAnimation ? animation.animationName
        : animation instanceof CSSTransition ? animation.transitionProperty : "transition";
      return `${name}@${target ? `${target.tagName.toLowerCase()}.${[...target.classList].join(".")}` : "?"}:${animation.playState}`;
    });
    if (!all.length) return "nothing";
    return `${all.length} animation(s): ${all.slice(0, 3).join(", ")}${all.length > 3 ? `, and ${all.length - 3} more` : ""}`;
  };
  /**
   * The longest any one wait took, and what it was waiting for.
   *
   * A green run on a host nobody can log into says only that the margin was
   * positive, never how positive — and that is the whole question here, since
   * the wait this replaces was passing locally with about twenty milliseconds
   * in hand while failing on the runner. Carrying the worst case out with the
   * verdict turns each CI run into a measurement of the headroom rather than
   * one more coin flip whose bias nobody can see.
   */
  const slowest = { ms: 0, waitingFor: "nothing" };
  // The two guards below stretch a slide on purpose. Their waits are the one
  // kind that says nothing about the host, so they are left out of the figure.
  let contrived = false;
  /**
   * Wait until the popover has finished reacting, rather than for a guess at
   * how long that takes.
   *
   * This used to spend a fixed 220ms and only then ask what was still running.
   * Measured under WKWebView, one `transition:slide` costs about 197ms from
   * the click to the node coming to rest, so the fixed budget was racing the
   * transition with roughly twenty milliseconds in hand — and the two hops
   * that make up that difference, the frame that starts the animation and the
   * frame that dispatches its finish event, are exactly what stretches on a
   * loaded runner while a 220ms timer does not. Nothing in the old spelling
   * could report that it had measured too early. It simply handed the probes a
   * pane whose inline `overflow:hidden` had not been reverted yet, or a node
   * the outro had not yet removed, and those are precisely the assertions that
   * failed on CI and nowhere else.
   *
   * Waiting on the animations themselves removes the race in both directions:
   * it cannot end early, it does not pay 220ms when nothing is moving, and it
   * is loud when it cannot finish, because a probe that could not run must
   * never be indistinguishable from one that ran and passed.
   */
  const settle = async (budget = 8000) => {
    await tick();
    // Svelte creates a transition's animation in a microtask after the effect
    // flush, so give the task back before looking; a synchronous look lands in
    // the gap before the transition it is meant to wait for exists.
    await new Promise(resolve => setTimeout(resolve));
    const began = performance.now();
    while (animating().length) {
      const waited = performance.now() - began;
      if (waited > budget) throw new Error(`The popover never settled after ${budget}ms: ${describeAnimating()}`);
      if (!contrived && waited > slowest.ms) { slowest.ms = waited; slowest.waitingFor = describeAnimating(); }
      // A host that has stopped producing frames still has to reach that
      // deadline. Waiting on `requestAnimationFrame` alone is how a stalled
      // runner turns a named failure into the runner's own "did not finish
      // within 60 seconds", which names nothing at all.
      await new Promise(resolve => { requestAnimationFrame(() => resolve(null)); setTimeout(resolve, 50); });
    }
    contrived = false;
    await tick();
  };
  const frame = () => new Promise(resolve => requestAnimationFrame(resolve));
  /**
   * Stretch the transition now under way until it outlasts any fixed budget a
   * wait could carry.
   *
   * This is the runner, reproduced. What a loaded macOS runner does to this
   * fixture is stretch the frame-driven parts of a transition — the frame that
   * starts the animation, and the frame that dispatches its finish event and
   * with it Svelte's cleanup — while leaving the fixture's own timers running
   * at full speed. Starving a healthy browser of frames from inside the page
   * is not possible; blocking the thread only delays the frame and the timer
   * together, and the browser paints as soon as it is released. Stretching the
   * same interval from the other side costs nothing in fidelity: what the
   * probes then face is the state that matters, a pane still mid-slide at the
   * moment they read it, reached without asking the host for anything.
   *
   * It throws unless exactly one transition is under way, so a guard whose
   * slide never started cannot report that the wait handled a slow one.
   */
  const outlastAnyBudget = (rate = 0.06) => {
    const [transition, ...rest] = animating();
    if (!transition || rest.length) throw new Error(`Expected exactly one transition to slow down, saw ${describeAnimating()}`);
    transition.playbackRate = rate;
    contrived = true;
  };
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
  /**
   * Whether any shadow in a computed `box-shadow` paints OUTSIDE the panel.
   *
   * The invariant on native material is that the window server owns the
   * window's shadow; a second one painted by the page shows up as a doubled,
   * offset gutter around the popover. This used to be asserted as
   * `boxShadow === "none"`, which was exact only while the panel drew no
   * shadow at all — an `inset` shadow is clipped to the border box and cannot
   * produce a gutter, so that spelling rejected the glass edge while
   * protecting nothing extra. Colours carry their own commas
   * (`rgba(0, 0, 0, .22)`), so the parenthesised groups come out before the
   * list is split.
   */
  const paintsOuterShadow = (value: string) =>
    value !== "none" && value.replace(/\([^)]*\)/g, "").split(",").some(part => !part.includes("inset"));
  /**
   * The glass ladder as the cascade actually resolves it, for whichever
   * material the shell is currently carrying.
   *
   * The fixture's entire job is to stand in for the native material, so the
   * two have to resolve this ladder identically in both appearances. Reading
   * one material against the other, rather than against literals, keeps the
   * check true as the surface is retuned — what it pins is that the fixture
   * is not tuned separately, which is the only way it can quietly stop
   * representing what ships. An unreadable token would make two empty strings
   * compare equal, so it throws instead of passing vacuously.
   */
  const glassTokens = () => {
    const style = getComputedStyle(element(".status-shell"));
    const tokens = ["--hue-a", "--hue-gain", "--glass-edge", "--glass-sheen-top", "--glass-shade-bottom", "--glass-tint", "--bg", "--surface", "--soft"]
      .map(token => `${token}:${style.getPropertyValue(token).trim()}`);
    const missing = tokens.filter(entry => entry.endsWith(":"));
    if (missing.length) throw new Error(`Glass ladder unreadable: ${missing.join(" ")}`);
    return tokens.join(" ");
  };
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
    check("last-fetch comparison remains explicit", /fetched \d+ min ago|never fetched/.test(element(".sync").textContent ?? "") && !!document.querySelector('[aria-label="3 ahead"]'));
    check("stashes and shortcuts are initially disclosed", !details() && !document.querySelector('[aria-label="2 stashes"], [aria-label="Go"], [aria-label="Tools"], [aria-label="Command palette"]'));
    check("only one primary action is visible", document.querySelectorAll(".primary").length === 1 && element(".primary").textContent?.includes("Review changes") === true);
    check("zero conflicts cannot navigate", button('[aria-label="0 conflicts"]').disabled);
    await click(".primary"); check("review opens the existing work view", action("section:work:overview"));
    await click(".details-toggle");
    check("Details reveals the available stashes and utilities", !!details() && !button('[aria-label="2 stashes"]').disabled && !!document.querySelector('[aria-label="Tools"]'));
    // Split from one `&&`. A panel that outgrew its window, a pane that lost
    // its height cap, and a pane that stopped scrolling are three different
    // defects, and as a single assertion all three reported the same sentence —
    // which is what made this one unactionable when it failed on a runner and
    // passed everywhere else.
    const detailsPane = element(".details");
    check("Details keeps the panel inside the native window", fits());
    check("Details stays within its 190px cap", detailsPane.offsetHeight <= 190);
    check("Details scrolls instead of growing the panel", getComputedStyle(detailsPane).overflowY === "auto");
    check("Details is keyboard scrollable", element(".details").tabIndex === 0);
    // Two guards on the wait itself rather than on the popover, because a wait
    // that ends early is indistinguishable from a component that broke, and
    // that ambiguity is what left these failures un-actionable for a release.
    // Each runs a slide well past any fixed budget, so the probe that follows
    // meets a pane Svelte has not finished with: an outroing node still in the
    // DOM, and a disclosure still under the slide's inline `overflow:hidden`.
    // Both are what a `playState === "running"` test, or a stopwatch, calls
    // settled — and both are exactly what failed on the runner and passed here.
    button(".details-toggle").click();
    await tick(); await frame(); outlastAnyBudget();
    await settle();
    check("a collapse slower than the wait is waited out, not read through", !details());
    button(".details-toggle").click();
    await tick(); await frame(); outlastAnyBudget();
    await settle();
    check("a disclosure slower than the wait is waited out, not read through",
      !!details() && getComputedStyle(element(".details")).overflowY === "auto");
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
    // Three facts in one assertion, and on a runner it reported only that one
    // of them was false. `button()` throws when a selector misses, so a *failed*
    // check here means the click landed and the panel did something else —
    // which of the three, and what it actually showed, is the whole question.
    // Naming the observed values follows what the tasks harness already does
    // for its menu rows.
    const switched = () => ({
      action: action("activate-repo:/Projects/ScholarLM"),
      header: element(".repository").textContent?.includes("ScholarLM") === true,
      closed: !document.querySelector(".repositories"),
    });
    const observed = (state: { action: boolean; header: boolean; closed: boolean }) =>
      state.action && state.header && state.closed
        ? ""
        : ` (last action ${JSON.stringify(document.querySelector(".action")?.textContent ?? null)},`
          + ` header ${JSON.stringify(document.querySelector(".repository")?.textContent ?? null)},`
          + ` chooser ${state.closed ? "closed" : "still open"})`;
    const afterSwitch = switched();
    check(`switching updates the header and closes the chooser${observed(afterSwitch)}`,
      afterSwitch.action && afterSwitch.header && afterSwitch.closed);
    await click('[aria-label="Choose repository"]'); await key("Escape");
    const afterEscape = switched();
    check(`Escape collapses the chooser before dismissing${afterEscape.closed && afterEscape.action ? "" : observed(afterEscape)}`,
      afterEscape.closed && afterEscape.action);
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
    const previewLightGlass = glassTokens();
    shell.setAttribute("data-material", "native"); await settle();
    check("native material does not pay for a second CSS blur", backdrop() === "none" && getComputedStyle(element(".panel")).backgroundColor.endsWith("0.8)"));
    check("native material has no outer painted gutter", getComputedStyle(shell).padding === "0px" && !paintsOuterShadow(getComputedStyle(element(".panel")).boxShadow));
    check("the light fixture is tuned like the native material it stands in for", glassTokens() === previewLightGlass);
    await select(0, "dark");
    const nativeDarkGlass = glassTokens();
    shell.setAttribute("data-material", "preview"); await settle();
    check("the dark fixture is tuned like the native material it stands in for", glassTokens() === nativeDarkGlass);
    await select(0, "light"); await settle();
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
  const diagnostics = `slowest settle ${Math.round(slowest.ms)}ms, waiting for ${slowest.waitingFor}`;
  document.documentElement.setAttribute("data-gp-result", encodeURIComponent(JSON.stringify({ results, diagnostics })));
  const report = new URLSearchParams(location.search).get("report");
  if (report) await fetch(report, { method: "POST", body: document.documentElement.outerHTML });
}
