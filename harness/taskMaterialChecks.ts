import { isMacOS } from "../src/lib/platform";

export async function checkTaskMaterials(errors: string[]): Promise<{ name: string; pass: true }[]> {
  const results: { name: string; pass: true }[] = [];
  const check = (name: string, pass: boolean) => {
    if (!pass) throw new Error(name);
    results.push({ name, pass: true });
  };
  const wait = async (condition: () => boolean) => {
    const deadline = performance.now() + 8000;
    while (!condition()) {
      // Naming the predicate that never came true is the difference between
      // "the fixture is broken" and "this one element is not rendered on this
      // platform" — the whole 8s deadline used to report the former for both.
      if (performance.now() > deadline) {
        const running = document.getAnimations()
          .filter(animation => animation.playState === "running")
          .map(animation => {
            const effect = animation.effect;
            const target = effect && "target" in effect ? (effect as KeyframeEffect).target : null;
            return `${(animation as CSSAnimation).animationName ?? animation.constructor.name}@${target ? `${target.tagName.toLowerCase()}.${[...target.classList].join(".")}` : "?"}`;
          });
        throw new Error(`Tasks fixture did not reach the expected state: ${condition} | running: ${running.join(", ") || "none"}`);
      }
      await new Promise(resolve => setTimeout(resolve, 30));
    }
  };
  const find = (selector: string): HTMLElement => {
    const element = document.querySelector<HTMLElement>(selector);
    if (!element) throw new Error(`Missing ${selector}`);
    return element;
  };
  const click = (selector: string) => find(selector).click();
  const alpha = (color: string) => {
    if (color === "transparent") return 0;
    const match = color.match(/(?:rgba\([^)]*,|\/)\s*([\d.]+)\s*\)$/);
    return match ? Number(match[1]) : 1;
  };
  const blur = (element: Element) => {
    const style = getComputedStyle(element);
    return style.getPropertyValue("backdrop-filter") || style.getPropertyValue("-webkit-backdrop-filter");
  };
  const fallback = matchMedia("(prefers-reduced-transparency: reduce), (prefers-contrast: more), (forced-colors: active)").matches;
  // Two different questions, and conflating them is what broke this harness on
  // Linux. The `macos` class on <html> is a FIXTURE knob — this page hard-codes
  // it and the Platform control toggles it — and it drives the CSS, so glass
  // checks read it. Whether a macOS-only ELEMENT exists is decided by the
  // component, which calls `isMacOS()` against the webview. On a Mac both agree
  // and the difference is invisible; on the Linux runner the class is still set
  // while `isMacOS()` is false, so anything keyed to the class waited for
  // markup that was never going to render.
  const glasses = () => document.documentElement.classList.contains("macos") && !fallback;
  const macosUi = isMacOS();
  const material = (name: string, selector: string) => {
    const nodes = [...document.querySelectorAll<HTMLElement>(selector)].filter(node => node.getClientRects().length > 0);
    check(`${name}: surfaces present`, nodes.length > 0);
    check(`${name}: ${glasses() ? "shared translucent fill" : "opaque fallback"}`, nodes.every(node => {
      const value = alpha(getComputedStyle(node).backgroundColor);
      return glasses() ? value > 0 && value < 1 : value === 1;
    }));
  };
  const floating = (name: string, selector: string) => {
    check(`${name}: ${glasses() ? "34px backdrop blur" : "blur removed"}`, glasses() ? blur(find(selector)).includes("blur(34px)") : blur(find(selector)) === "none");
  };
  const closed = async (selector: string) => wait(() => !document.querySelector(selector));
  const theme = find("#theme");
  if (!(theme instanceof HTMLSelectElement)) throw new Error("Missing theme control");
  await wait(() => document.querySelectorAll("[data-task-card]").length === 6);

  for (const mode of ["dark", "light"]) {
    theme.value = mode; theme.dispatchEvent(new Event("change", { bubbles: true }));
    material(`${mode} cards`, "[data-task-card]");
    check(`${mode}: cards do not stack filters`, [...document.querySelectorAll("[data-task-card]")].every(node => blur(node) === "none"));

    if (document.querySelector(".navigator")) {
      const scopes = [...document.querySelectorAll<HTMLButtonElement>(".navigator .gp-seg-btn")];
      for (const scope of [...scopes, ...scopes].reverse()) {
        scope.click();
        await new Promise(resolve => requestAnimationFrame(resolve));
      }
      // The scope pill is macOS-only by construction: TaskBoard renders
      // `.gp-liquid-selection` under `{#if macos && selected}`, and
      // macAppearance asserts its absence everywhere else. Waiting for one
      // unconditionally could only ever spend the full deadline off macOS, and
      // reported it as the fixture never loading rather than as a pill that is
      // not coming. Settling is the part that is true on every platform.
      const pills = () => document.querySelectorAll(".navigator .gp-liquid-selection").length;
      await wait(() => pills() === (macosUi ? 1 : 0) && document.getAnimations().every(animation => animation.playState !== "running"));
      check(`${mode}: rapid scope changes settle to one selected control`, document.querySelectorAll('.navigator [aria-pressed="true"]').length === 1);
      if (macosUi) {
        check(`${mode}: scope pill stays decorative`, find(".navigator .gp-liquid-selection").getAttribute("aria-hidden") === "true" && getComputedStyle(find(".navigator .gp-liquid-selection")).pointerEvents === "none");
        check(`${mode}: scope pill belongs to the selected control`, !!document.querySelector('.navigator [aria-pressed="true"] > .gp-liquid-selection'));
      }
      click('[aria-label="New workspace"]');
      await wait(() => !!document.querySelector(".workspace-editor"));
      material(`${mode} workspace`, ".workspace-editor,.workspace-editor input:not([type=checkbox]),.workspace-editor textarea");
      floating(`${mode} workspace header`, ".workspace-editor > header");
      const workspace = find(".workspace-editor");
      const workspaceBody = find(".workspace-editor .sheet-body");
      const workspaceSave = find(".workspace-editor footer");
      workspaceBody.scrollTop = 200;
      await new Promise(resolve => requestAnimationFrame(resolve));
      check(`${mode}: workspace header remains over scrolling content`, Math.abs(find(".workspace-editor > header").getBoundingClientRect().top - workspace.getBoundingClientRect().top) < 2);
      check(`${mode}: workspace save bar stays in the sheet chrome`, Math.abs(workspaceSave.getBoundingClientRect().bottom - workspace.getBoundingClientRect().bottom) < 2);
      check(`${mode}: workspace save bar is not an opaque slab`, alpha(getComputedStyle(workspaceSave).backgroundColor) < 1);
      click('[aria-label="Close workspace settings"]');
      await closed(".workspace-editor");
    }

    click("[data-task-card]");
    await wait(() => !!document.querySelector(".task-editor"));
    material(`${mode} task details`, ".task-editor,.task-editor .gp-field:not(.draft),.task-editor select");
    floating(`${mode} task header`, ".task-editor > header");
    // A saved task is two panes, and only the open one is drawn. The Agent
    // probe below therefore has to open its pane first: `material` measures
    // client rects, so a hidden pane would report "no surfaces" rather than a
    // colour. The assist is on the Task pane now, so it needs no click.
    check(`${mode}: a saved task offers its two panes`,
      [...document.querySelectorAll("[data-sheet-tab]")].map(tab => tab.getAttribute("data-sheet-tab")).join(",") === "task,agent");
    check(`${mode}: only the selected pane is drawn`,
      [...document.querySelectorAll(".task-editor .pane")].filter(pane => pane.getClientRects().length > 0).length === 1);
    await wait(() => (document.querySelector(".manvi-assist .change-link")?.getClientRects().length ?? 0) > 0);
    check(`${mode}: merged Manvi section has Change link and no Model input`,
      Boolean(document.querySelector(".manvi-assist .change-link"))
      && ![...document.querySelectorAll(".manvi-assist label")].some(label => label.firstChild?.textContent?.trim() === "Model"));
    material(`${mode} Manvi assist`, ".manvi-assist .gp-field:not(.draft),.manvi-assist textarea");
    click('[data-sheet-tab="agent"]');
    await wait(() => (find(".task-runs").getClientRects().length > 0) && !document.querySelector(".task-runs [role=alert]"));
    material(`${mode} agent run controls`, ".task-runs .gp-field,.task-runs select");
    click('[data-sheet-tab="task"]');
    await wait(() => (document.querySelector('input[name="task-title"]')?.getClientRects().length ?? 0) > 0);
    const editor = find(".task-editor");
    check(`${mode}: task revision attribute is present`, editor.hasAttribute("data-task-revision"));
    const body = find(".task-editor .sheet-body");
    const saveBar = find(".task-editor > footer");
    body.scrollTop = 200;
    await new Promise(resolve => requestAnimationFrame(resolve));
    check(`${mode}: header remains over scrolling content`, Math.abs(find(".task-editor > header").getBoundingClientRect().top - editor.getBoundingClientRect().top) < 2);
    check(`${mode}: save bar stays in the sheet chrome`, Math.abs(saveBar.getBoundingClientRect().bottom - editor.getBoundingClientRect().bottom) < 2);
    check(`${mode}: save bar is not an opaque slab`, alpha(getComputedStyle(saveBar).backgroundColor) < 1);
    click('[aria-label="Close task details"]');
    await closed(".task-editor");

    click(".automation .summary > button");
    await wait(() => document.querySelectorAll(".automation .settings input").length === 2);
    floating(`${mode} automatic settings`, ".automation .settings");
    material(`${mode} automatic settings`, ".automation .settings");
    click(".automation .settings .check:nth-child(2) input");
    await wait(() => document.querySelectorAll(".automation .pair input").length === 2);
    material(`${mode} automatic fields`, ".automation .pair input");
    document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    await closed(".automation .settings");
    check(`${mode}: Escape returns focus to Auto`, document.activeElement === document.querySelector(".automation .summary > button"));
    click(".automation .summary > button");
    await wait(() => !!document.querySelector(".automation .settings"));
    find(".columns").dispatchEvent(new PointerEvent("pointerdown", { bubbles: true }));
    await closed(".automation .settings");
    check(`${mode}: clicking the board dismisses Auto`, true);

    click('button[aria-label="Inbox"]');
    await wait(() => !!document.querySelector(".inbox .empty"));
    material(`${mode} inbox`, ".inbox");
    click(".inbox > details > summary");
    await wait(() => !!document.querySelector(".inbox .native-settings"));
    click(".inbox .native-settings > summary");
    // Desktop notification controls exist only where the host could deliver a
    // banner: `desktopNotificationsSupported` refuses windows and linux before
    // any probe runs, and that panel renders its unavailable notice instead.
    // Waiting for the fieldset alone could only ever spend the deadline there.
    // Waiting for whichever one arrives, then measuring the one that did, keeps
    // this an assertion about the panel rather than a second copy of the rule
    // deciding which half is shown — a copy that would drift out of step the
    // moment the rule changed.
    const notifyPanel = ".inbox .native-settings";
    await wait(() => !!document.querySelector(`${notifyPanel} fieldset, ${notifyPanel} [data-testid="notify-unavailable"]`));
    if (document.querySelector(`${notifyPanel} fieldset`)) {
      material(`${mode} notification buttons`, `${notifyPanel} button`);
    } else {
      check(`${mode}: a host that cannot deliver banners says so instead of offering dead controls`,
        (find(`${notifyPanel} [data-testid="notify-unavailable"]`).textContent ?? "").trim().length > 0);
    }
    check(`${mode}: no surfaced transport errors`, !document.querySelector("#board [role=alert]"));
    click('button[aria-label="Inbox"]');
    await closed(".inbox");
  }
  theme.value = "dark"; theme.dispatchEvent(new Event("change", { bubbles: true }));
  check("No uncaught runtime errors", errors.length === 0);
  return results;
}
