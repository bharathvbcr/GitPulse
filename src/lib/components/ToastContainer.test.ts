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
