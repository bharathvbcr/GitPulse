export async function checkTaskMaterials(errors: string[]): Promise<{ name: string; pass: true }[]> {
  const results: { name: string; pass: true }[] = [];
  const check = (name: string, pass: boolean) => {
    if (!pass) throw new Error(name);
    results.push({ name, pass: true });
  };
  const wait = async (condition: () => boolean) => {
    const deadline = performance.now() + 8000;
    while (!condition()) {
      if (performance.now() > deadline) throw new Error("Tasks fixture did not reach the expected state");
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
  const glasses = () => document.documentElement.classList.contains("macos") && !fallback;
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
      await wait(() => document.querySelectorAll(".navigator .gp-liquid-selection").length === 1 && document.getAnimations().every(animation => animation.playState !== "running"));
      check(`${mode}: rapid scope changes settle to one selected control`, document.querySelectorAll('.navigator [aria-pressed="true"]').length === 1);
      check(`${mode}: scope pill stays decorative`, find(".navigator .gp-liquid-selection").getAttribute("aria-hidden") === "true" && getComputedStyle(find(".navigator .gp-liquid-selection")).pointerEvents === "none");
      check(`${mode}: scope pill belongs to the selected control`, !!document.querySelector('.navigator [aria-pressed="true"] > .gp-liquid-selection'));
      click('[aria-label="New workspace"]');
      await wait(() => !!document.querySelector(".workspace-editor"));
      material(`${mode} workspace`, ".workspace-editor,.workspace-editor input:not([type=checkbox]),.workspace-editor textarea");
      floating(`${mode} workspace header`, ".workspace-editor > header");
      click('[aria-label="Close workspace settings"]');
      await closed(".workspace-editor");
    }

    click("[data-task-card]");
    await wait(() => !!document.querySelector(".task-runs") && !document.querySelector(".task-runs [role=alert]"));
    material(`${mode} task details`, ".task-editor,.task-editor .gp-field,.task-editor select");
    floating(`${mode} task header`, ".task-editor > header");
    click(".enhancements > .heading");
    await wait(() => document.querySelector<HTMLInputElement>(".enhancements input[list]")?.value === "fixture");
    material(`${mode} suggestions`, ".enhancements .gp-field");
    material(`${mode} agent run controls`, ".task-runs .gp-field,.task-runs select");
    const editor = find(".task-editor");
    const body = find(".task-editor .sheet-body");
    body.scrollTop = 200;
    await new Promise(resolve => requestAnimationFrame(resolve));
    check(`${mode}: header remains over scrolling content`, Math.abs(find(".task-editor > header").getBoundingClientRect().top - editor.getBoundingClientRect().top) < 2);
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
    click(".inbox .native-settings > summary");
    await wait(() => !!document.querySelector(".inbox .native-settings fieldset"));
    material(`${mode} notification buttons`, ".inbox .native-settings button");
    check(`${mode}: no surfaced transport errors`, !document.querySelector("#board [role=alert]"));
    click('button[aria-label="Inbox"]');
    await closed(".inbox");
  }
  theme.value = "dark"; theme.dispatchEvent(new Event("change", { bubbles: true }));
  check("No uncaught runtime errors", errors.length === 0);
  return results;
}
