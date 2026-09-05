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
});

describe("the live region exists before the content lands in it", () => {
  const source = readFileSync(new URL("./ToastContainer.svelte", import.meta.url), "utf8");

  it("puts aria-live on a persistent wrapper, not on each inserted toast", () => {
    // A live region has to be in the DOM before content arrives to be watched;
    // a region inserted together with its content is mostly not announced.
    const cards = source.slice(source.indexOf("{#each $toastStore as toast"));
    expect(cards).not.toContain("aria-live");
    expect(source).toContain('aria-live="assertive"');
    expect(source).toContain('aria-live="polite"');
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

  it("pauses countdowns on hover and focus", () => {
    expect(source).toContain("onmouseenter={() => toastStore.pauseAll()}");
    expect(source).toContain("onmouseleave={() => toastStore.resumeAll()}");
    expect(source).toContain("onfocusin={() => toastStore.pauseAll()}");
    expect(source).toContain("onfocusout={() => toastStore.resumeAll()}");
  });
});
