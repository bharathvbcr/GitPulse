import "../src/app.css";
import { mount, tick } from "svelte";
import PrimitivesHost from "./PrimitivesHost.svelte";
import { createChecks } from "./checks";

/**
 * The shared primitives (Toggle, Slider, Dialog) as real DOM. Vitest never runs
 * `$effect`, so the focus trap, the Escape and scrim handling and the outros
 * can only be held here.
 *
 * Nothing waits a fixed time. Every wait is a condition with a bounded
 * deadline, so a slow host stretches a wait and a hang fails it by name.
 *
 * What a page cannot do: send a trusted key press. A synthetic key event does
 * not activate a native button, so Space and Enter on the Toggle are checked
 * as "nothing swallows them" on a native button, and the activation itself as
 * the click they produce. The Slider's page keys and the Dialog's Escape and
 * Tab are our own handlers, so synthetic events exercise them for real.
 */
const DEADLINE_MS = 8000;

const frame = () => new Promise<void>((resolve) => {
  // A hidden or unfocused runner pauses animation frames, so a timer backs it up.
  let done = false;
  const finish = () => { if (!done) { done = true; resolve(); } };
  requestAnimationFrame(finish);
  setTimeout(finish, 50);
});

async function waitFor(what: string, condition: () => unknown, ms = DEADLINE_MS): Promise<void> {
  const end = performance.now() + ms;
  for (;;) {
    await tick();
    if (condition()) return;
    if (performance.now() > end) throw new Error(`timed out after ${ms}ms waiting for ${what}`);
    await frame();
  }
}

/** Animations with a finite end: a pulse that never ends is not something to wait for. */
const finiteAnimations = () => document.getAnimations()
  .filter((animation) => Number.isFinite(Number(animation.effect?.getComputedTiming().endTime)));
const idle = () => waitFor("every finite animation to finish", () => finiteAnimations().length === 0);

const q = <T extends Element = HTMLElement>(selector: string, root: ParentNode = document): T => {
  const found = root.querySelector<T>(selector);
  if (!found) throw new Error(`no element matches ${selector}`);
  return found;
};

function key(target: Element, init: KeyboardEventInit & { key: string }): KeyboardEvent {
  const event = new KeyboardEvent("keydown", { bubbles: true, cancelable: true, ...init });
  target.dispatchEvent(event);
  return event;
}

export async function checkPrimitives() {
  const { results, check, stopped } = createChecks();
  const state = () => q('[data-testid="state"]').dataset;
  const active = () => document.activeElement as HTMLElement | null;
  const dialog = () => document.querySelector<HTMLElement>('[role="dialog"]');
  const scrim = () => document.querySelector<HTMLElement>("[data-dialog-scrim]");

  try {
    mount(PrimitivesHost, { target: q("#stage") });
    await tick();
    await idle();

    // ---- Toggle --------------------------------------------------------
    const bare = q<HTMLButtonElement>('[data-testid="toggles"] button[role="switch"]');
    check("the Toggle is a native button with role=switch, so Space and Enter activate it", () =>
      bare.tagName === "BUTTON" && bare.type === "button" && bare.getAttribute("role") === "switch");
    check("the Toggle is named by its aria-label and starts off", () =>
      bare.getAttribute("aria-label") === "Bare switch" && bare.getAttribute("aria-checked") === "false");
    bare.focus();
    check("the Toggle takes keyboard focus", () => active() === bare && bare.tabIndex >= 0);
    check("Space on the Toggle is not swallowed by a handler, so the button's own activation runs", () => !key(bare, { key: " " }).defaultPrevented);
    check("Enter on the Toggle is not swallowed either", () => !key(bare, { key: "Enter" }).defaultPrevented);
    bare.click();
    await tick();
    check("a press turns the Toggle on and aria-checked follows", () => bare.getAttribute("aria-checked") === "true" && state().on === "true" && state().changes === "1");
    bare.click();
    await tick();
    check("a second press turns it off again", () => bare.getAttribute("aria-checked") === "false" && state().changes === "2");
    const labelledSwitch = q<HTMLButtonElement>("#" + CSS.escape(q<HTMLLabelElement>('[data-testid="toggles"] label').htmlFor));
    q<HTMLLabelElement>('[data-testid="toggles"] label').click();
    await tick();
    check("clicking a Toggle's visible label turns it on", () => labelledSwitch.getAttribute("aria-checked") === "true" && state().labelled === "true");
    const disabledSwitch = q<HTMLButtonElement>('[data-testid="toggles"] button[aria-label="Disabled switch"]');
    disabledSwitch.click();
    await tick();
    check("a disabled Toggle is disabled and asks for nothing", () => disabledSwitch.disabled && state().disabledChanges === "0" && disabledSwitch.getAttribute("aria-checked") === "false");

    // ---- Checkbox ------------------------------------------------------
    const boxes = '[data-testid="checkboxes"]';
    const cbState = () => q('[data-testid="checkbox-state"]').dataset;
    const box = (name: string) => q<HTMLInputElement>(`${boxes} input[aria-label="${name}"]`);
    const bareBox = box("Bare box");
    check("the Checkbox is a native checkbox input wearing gp-check, named by its aria-label, and starts unticked", () =>
      bareBox.tagName === "INPUT" && bareBox.type === "checkbox" && bareBox.classList.contains("gp-check")
      && bareBox.getAttribute("aria-label") === "Bare box" && !bareBox.checked);
    check("the Checkbox is drawn, not the browser's: appearance none, a 16px square on the xs radius", () => {
      const style = getComputedStyle(bareBox);
      return style.appearance === "none" && bareBox.getBoundingClientRect().width === 16 && bareBox.getBoundingClientRect().height === 16
        && style.borderTopLeftRadius === "4px";
    });
    bareBox.focus();
    check("the Checkbox takes keyboard focus and does not swallow Space", () => active() === bareBox && bareBox.tabIndex >= 0 && !key(bareBox, { key: " " }).defaultPrevented);
    bareBox.click();
    await tick();
    check("a click ticks the Checkbox and checked follows", () => bareBox.checked && cbState().ticked === "true" && cbState().tickChanges === "1");
    bareBox.click();
    await tick();
    check("a second click unticks it", () => !bareBox.checked && cbState().ticked === "false" && cbState().tickChanges === "2");
    const labelledBox = box("Labelled box");
    const labelRow = labelledBox.closest("label");
    check("a labelled Checkbox sits inside one <label>, with the description linked", () =>
      !!labelRow && labelRow.htmlFor === labelledBox.id
      && document.getElementById(labelledBox.getAttribute("aria-describedby") ?? "")?.textContent?.trim() === "Says what ticking commits to.");
    q<HTMLElement>("span.font-medium", labelRow ?? document).click();
    await tick();
    check("clicking a Checkbox's visible label ticks it", () => labelledBox.checked && cbState().confirmed === "true");
    const mixedBox = box("Mixed box");
    check("indeterminate is reflected on the input and drawn as an accent-filled bar", () =>
      mixedBox.indeterminate && mixedBox.matches(":indeterminate") && !mixedBox.checked
      && getComputedStyle(mixedBox).backgroundColor !== "rgba(0, 0, 0, 0)" && getComputedStyle(mixedBox, "::before").opacity === "1");
    mixedBox.click();
    await tick();
    check("pressing an indeterminate Checkbox clears it through the parent", () => !mixedBox.indeterminate && cbState().mixed === "false");
    const refusing = box("Refusing box");
    refusing.click();
    await tick();
    check("a parent that refuses the change keeps the old state: the box is put back", () => !refusing.checked);
    const disabledBox = box("Disabled box");
    disabledBox.click();
    await tick();
    check("a disabled Checkbox is disabled, dimmed to .4 and ignores clicks", () =>
      disabledBox.disabled && !disabledBox.checked && cbState().disabledTicks === "0" && getComputedStyle(disabledBox).opacity === "0.4");
    check("an unticked Checkbox is transparent with no glyph", () =>
      !bareBox.checked && getComputedStyle(bareBox).backgroundColor === "rgba(0, 0, 0, 0)" && getComputedStyle(bareBox, "::before").opacity === "0");
    bareBox.click();
    await tick();
    // The fill fades in over --gp-dur-instant; read it once that has finished.
    await idle();
    check("a ticked Checkbox fills with the accent and shows its glyph", () =>
      bareBox.checked && getComputedStyle(bareBox).backgroundColor !== "rgba(0, 0, 0, 0)" && getComputedStyle(bareBox, "::before").opacity === "1");

    // ---- Slider --------------------------------------------------------
    const range = q<HTMLInputElement>('[data-testid="slider"] input[type="range"]');
    check("the Slider is a native range input labelled by its visible label", () =>
      range.type === "range" && q<HTMLLabelElement>('[data-testid="slider"] label').htmlFor === range.id && range.getAttribute("aria-valuetext") === "20%");
    range.focus();
    const up = key(range, { key: "PageUp" });
    await tick();
    check("Page Up moves ten steps (20 to 70) and aria-valuetext follows format", () =>
      up.defaultPrevented && range.value === "70" && range.getAttribute("aria-valuetext") === "70%" && state().level === "70");
    const down = key(range, { key: "PageDown" });
    await tick();
    check("Page Down moves ten steps back (70 to 20)", () => down.defaultPrevented && range.value === "20" && range.getAttribute("aria-valuetext") === "20%");
    key(range, { key: "PageDown" });
    await tick();
    check("Page Down clamps at the minimum", () => range.value === "0" && range.getAttribute("aria-valuetext") === "0%");
    key(range, { key: "PageUp" });
    key(range, { key: "PageUp" });
    key(range, { key: "PageUp" });
    await tick();
    check("Page Up clamps at the maximum", () => range.value === "100" && range.getAttribute("aria-valuetext") === "100%");
    check("the visible value label shows the formatted value", () => q('[data-testid="slider"] span[aria-hidden="true"]').textContent?.trim() === "100%");
    const inputsBefore = state().inputs;
    const arrow = key(range, { key: "ArrowUp" });
    const modified = key(range, { key: "PageDown", ctrlKey: true });
    await tick();
    check("arrow keys are left to the native input, and a modified page key is ignored", () =>
      !arrow.defaultPrevented && !modified.defaultPrevented && range.value === "100" && state().inputs === inputsBefore);

    // ---- Dialog --------------------------------------------------------
    /** Press the opener by keyboard-style focus, then wait for the dialog and its intro to settle. */
    async function open(openerId: string): Promise<{ opener: HTMLElement; card: HTMLElement }> {
      const opener = q(openerId);
      opener.focus();
      opener.click();
      await waitFor(`the dialog opened by ${openerId}`, dialog);
      await idle();
      return { opener, card: q('[role="dialog"]') };
    }
    /** Run `action`, then report which surfaces ran a real outro and wait for the dialog to leave. */
    async function closeBy(action: () => void) {
      const card = dialog();
      const backdrop = scrim();
      action();
      await tick();
      const ran = (el: HTMLElement | null) => !!el && finiteAnimations().some((animation) => {
        const effect = animation.effect;
        return effect instanceof KeyframeEffect && effect.target === el && Number(effect.getTiming().duration) > 0;
      });
      // An outro is only ever on screen for a moment, so keep looking until the
      // dialog is gone and remember each surface that was seen animating.
      const outro = { scrim: ran(backdrop), card: ran(card) };
      await waitFor("the dialog to leave the DOM", () => {
        outro.scrim ||= ran(backdrop);
        outro.card ||= ran(card);
        return !dialog() && !scrim();
      });
      await idle();
      return outro;
    }
    const focusables = (card: HTMLElement) => [...card.querySelectorAll<HTMLElement>("button:not([disabled]), input:not([disabled])")];

    for (const [name, openerId, closer, sizeAttr] of [
      ["md", "#open-md", "md-cancel", "md"],
      ["sheet", "#open-sheet", "sheet-done", "sheet"],
    ] as const) {
      const closesBefore = Number(state().closes);
      let { opener, card } = await open(openerId);
      check(`${name}: it is a modal dialog of size ${sizeAttr}, named by its title`, () =>
        card.getAttribute("aria-modal") === "true" && card.dataset.dialogSize === sizeAttr && !!card.getAttribute("aria-labelledby")
        && document.getElementById(card.getAttribute("aria-labelledby") ?? "")?.tagName === "H2");
      check(`${name}: initial focus lands inside the dialog, on a control and not the Close button`, () =>
        card.contains(active()) && active()?.getAttribute("aria-label") !== "Close" && active() !== card);
      const items = focusables(card);
      check(`${name}: the card holds a Close button, a field and its footer buttons`, () => items.length >= 3 && items.some((el) => el.getAttribute("aria-label") === "Close"));
      items.at(-1)?.focus();
      const forward = key(items.at(-1) ?? card, { key: "Tab" });
      check(`${name}: Tab from the last control wraps to the first, inside the card`, () => forward.defaultPrevented && active() === items[0] && card.contains(active()));
      const backward = key(items[0], { key: "Tab", shiftKey: true });
      check(`${name}: Shift+Tab from the first control wraps to the last`, () => backward.defaultPrevented && active() === items.at(-1));
      items.find((el) => el.tagName === "INPUT")?.focus();

      let outro = await closeBy(() => { key(active() ?? card, { key: "Escape" }); });
      check(`${name}: Escape closes the dialog and fires onclose once`, () => !dialog() && Number(state().closes) === closesBefore + 1);
      check(`${name}: focus returns to the opener that opened it by keyboard`, () => active() === opener);
      check(`${name}: the scrim and the card both ran an outro on Escape`, () => outro.scrim && outro.card);

      ({ opener, card } = await open(openerId));
      outro = await closeBy(() => {
        const backdrop = q("[data-dialog-scrim]");
        backdrop.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true }));
        backdrop.click();
      });
      check(`${name}: a press and click on the scrim closes it, with both outros`, () => !dialog() && outro.scrim && outro.card && active() === opener);

      ({ card } = await open(openerId));
      card.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true }));
      q("[data-dialog-scrim]").click();
      await tick();
      await frame();
      await idle();
      check(`${name}: a press that starts in the card and ends on the scrim does not close it`, () => !!dialog());
      q("[data-dialog-body]", card).click();
      await tick();
      check(`${name}: a click inside the card does not close it`, () => !!dialog());
      card.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", isComposing: true, bubbles: true, cancelable: true }));
      await tick();
      check(`${name}: the Escape that cancels an IME composition does not close it`, () => !!dialog());
      outro = await closeBy(() => q(`#${closer}`).click());
      check(`${name}: a footer button closes it, with both outros`, () => !dialog() && outro.scrim && outro.card);
    }

    // ---- dismissable={false} ------------------------------------------
    const lockedClosesBefore = Number(state().closes);
    const locked = await open("#open-locked");
    check("locked: it has no Close button, and focus starts on its inner control", () =>
      !locked.card.querySelector('[aria-label="Close"]') && active()?.id === "locked-inner");
    const lockedEscape = key(active() ?? locked.card, { key: "Escape" });
    q("[data-dialog-scrim]").dispatchEvent(new PointerEvent("pointerdown", { bubbles: true }));
    q("[data-dialog-scrim]").click();
    await tick();
    await frame();
    await idle();
    check("locked: Escape and the scrim do nothing, and it ran no outro", () =>
      !lockedEscape.defaultPrevented && !!dialog() && state().locked === "true" && Number(state().closes) === lockedClosesBefore);
    const lockedOutro = await closeBy(() => q("#locked-finish").click());
    check("locked: only its own footer button closes it, with both outros, and focus returns", () =>
      !dialog() && lockedOutro.scrim && lockedOutro.card && active() === locked.opener);

    check("no animation is left running when the page is done", () => finiteAnimations().length === 0);
  } catch (error) {
    stopped(error);
  }
  document.documentElement.setAttribute("data-gp-result", encodeURIComponent(JSON.stringify({ results })));
  const report = new URLSearchParams(location.search).get("report");
  if (report) await fetch(report, { method: "POST", body: document.documentElement.outerHTML });
}

if (new URLSearchParams(location.search).has("check")) await checkPrimitives();
else mount(PrimitivesHost, { target: q("#stage") });
