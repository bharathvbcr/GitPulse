import { readFileSync } from "node:fs";
import { describe, expect, it, beforeEach } from "vitest";
import { render } from "svelte/server";
import ToastContainer from "./ToastContainer.svelte";
import { toastStore } from "../stores/toastStore";

describe("ToastContainer", () => {
  beforeEach(() => {
    toastStore.clear();
  });

  it("renders toast container with region and aria attributes", () => {
    const { body } = render(ToastContainer);
    expect(body).toContain('role="region"');
    expect(body).toContain('aria-label="Notifications"');
  });

  it("renders active toasts with proper roles and content", () => {
    toastStore.success("Repository cloned successfully");
    toastStore.error("Failed to push to remote", {
      label: "Retry",
      onClick: () => {},
    });

    const { body } = render(ToastContainer);
    expect(body).toContain('role="status"');
    expect(body).toContain('aria-live="polite"');
    expect(body).toContain("Repository cloned successfully");
    expect(body).toContain("Failed to push to remote");
    expect(body).toContain("Retry");
  });

  it("collapses several toasts onto the newest card", () => {
    toastStore.info("Oldest notice");
    toastStore.warning("Middle notice");
    toastStore.success("Newest notice");

    const { body } = render(ToastContainer);
    const pile = body.slice(body.indexOf('data-testid="toast-pile"'));
    expect(pile.match(/role="presentation"/g)).toHaveLength(1);
    expect(pile.match(/class="[^"]*\bpeek\b/g)).toHaveLength(2);
    expect(pile).toContain("Newest notice");
    expect(pile).not.toContain("Oldest notice");
    expect(pile).not.toContain("Middle notice");
    expect(body).toContain("Oldest notice");
    expect(body).toContain("Middle notice");
  });

  it("draws a single toast as one card", () => {
    toastStore.info("Only notice");
    const { body } = render(ToastContainer);
    const pile = body.slice(body.indexOf('data-testid="toast-pile"'));
    expect(pile.match(/role="presentation"/g)).toHaveLength(1);
    expect(pile).not.toContain("peek");
    expect(pile).toContain("Only notice");
  });
});

describe("the live region exists before the content lands in it", () => {
  const source = readFileSync(new URL("./ToastContainer.svelte", import.meta.url), "utf8");

  it("puts aria-live on a persistent wrapper, not on each inserted toast", () => {
    // A live region has to be in the DOM before content arrives to be watched;
    // a region inserted together with its content is mostly not announced.
    const cards = source.slice(source.indexOf('data-testid="toast-pile"'));
    expect(cards).not.toContain("aria-live");
    expect(source).toContain('aria-live="assertive"');
    expect(source).toContain('aria-live="polite"');
    expect(source.indexOf('aria-live="assertive"')).toBeLessThan(source.indexOf('data-testid="toast-pile"'));
  });

  it("announces errors assertively and everything else politely", () => {
    // One region cannot be both, and flipping politeness at runtime is not
    // reliably honoured.
    expect(source).toContain('$toastStore.filter((t) => t.kind === "error")');
    expect(source).toContain('$toastStore.filter((t) => t.kind !== "error")');
  });

  it("hides the visual card from assistive tech so nothing is read twice", () => {
    expect(source).toContain('aria-hidden="true"');
  });

  it("pauses countdowns on hover and focus, and stays paused while focus remains inside", () => {
    expect(source).toContain("onmouseenter={engage}");
    expect(source).toContain("onmouseleave={release}");
    expect(source).toContain("onfocusin={engage}");
    expect(source).toContain("onfocusout={release}");
    expect(source).toContain("toastStore.pauseAll()");
    expect(source).toContain("toastStore.resumeAll()");
    expect(source).toContain('event.type === "focusout" && focusStayedInside(pile, event.relatedTarget)');
    expect(source).toContain('event.type === "mouseleave" && focusStayedInside(pile, active)');
    expect(source).toContain("toasts.slice(-stack.shown)");
    const style = source.slice(source.indexOf("<style>"));
    expect(style).not.toMatch(/background(?:-color)?:\s*rgb\(var\(--c-(?:bg|surface|surface-hover)\)\)/);
  });
});
