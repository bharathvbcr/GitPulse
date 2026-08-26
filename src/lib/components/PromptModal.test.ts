import { afterEach, describe, expect, it } from "vitest";
import { render } from "svelte/server";
import PromptModal from "./PromptModal.svelte";
import { askConfirm, askText, cancelPrompt } from "../stores/modalStore";

afterEach(() => {
  cancelPrompt();
});

describe("PromptModal", () => {
  it("renders nothing while no prompt is pending", () => {
    const { body } = render(PromptModal);
    expect(body).not.toContain('role="dialog"');
  });

  it("renders a text prompt from the modal store", () => {
    void askText({
      title: "Quick Commit",
      message: "Stage all changes",
      placeholder: "feat: …",
    });
    const { body } = render(PromptModal);
    expect(body).toContain('role="dialog"');
    expect(body).toContain('aria-label="Quick Commit"');
    expect(body).toContain("Quick Commit");
    expect(body).toContain("Stage all changes");
    expect(body).toContain("feat: …");
  });

  it("renders a confirm prompt without a text field", () => {
    void askConfirm({
      title: "Delete branch?",
      message: "This cannot be undone.",
    });
    const { body } = render(PromptModal);
    expect(body).toContain('aria-label="Delete branch?"');
    expect(body).toContain("This cannot be undone.");
    expect(body).not.toContain("<input");
  });
});
