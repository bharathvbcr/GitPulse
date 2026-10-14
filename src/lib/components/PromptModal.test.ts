import { afterEach, describe, expect, it } from "vitest";
import { render } from "svelte/server";
import PromptModal from "./PromptModal.svelte";
import { get } from "svelte/store";
import { askConfirm, askText, cancelPrompt, completePrompt, promptError, promptState } from "../stores/modalStore";

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

  it("uses the danger button for destructive confirms", () => {
    void askConfirm({
      title: "Delete task?",
      destructive: true,
      confirmLabel: "Delete task",
    });
    const { body } = render(PromptModal);
    expect(body).toContain("gp-btn-danger");
    expect(body).toContain("Delete task");
  });

  describe("validate", () => {
    const refuseTaken = (value: string) => (value === "taken" ? "That name is taken." : null);

    it("keeps the prompt open and publishes the refusal when the answer is refused", async () => {
      const answer = askText({ title: "New File", validate: refuseTaken });
      let settled = false;
      void answer.then(() => (settled = true));
      completePrompt("taken");
      await Promise.resolve();
      expect(settled).toBe(false);
      expect(get(promptState)).not.toBeNull();
      expect(get(promptError)).toBe("That name is taken.");
      expect(render(PromptModal).body).toContain("That name is taken.");

      completePrompt("free");
      await expect(answer).resolves.toBe("free");
      expect(get(promptError)).toBeNull();
    });

    it("never consults validate when the prompt is cancelled", async () => {
      let calls = 0;
      const answer = askText({
        title: "New File",
        validate: () => {
          calls += 1;
          return "no";
        },
      });
      cancelPrompt();
      await expect(answer).resolves.toBeNull();
      expect(calls).toBe(0);
    });

    it("shows an opening error and the typed name without validating", () => {
      void askText({ title: "New File", initialValue: "a.txt", error: "a.txt already exists" });
      const { body } = render(PromptModal);
      expect(body).toContain("a.txt already exists");
      expect(body).toContain('role="alert"');
    });

    it("leaves prompts without validate exactly as they were", async () => {
      const answer = askText({ title: "Rename" });
      completePrompt("anything");
      await expect(answer).resolves.toBe("anything");
    });
  });
});
