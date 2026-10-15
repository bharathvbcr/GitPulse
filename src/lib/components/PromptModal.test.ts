import { afterEach, describe, expect, it } from "vitest";
import { render } from "svelte/server";
import PromptModal from "./PromptModal.svelte";
import { askChoice, askConfirm, askText, cancelPrompt, completePrompt, ALTERNATE_ANSWER } from "../stores/modalStore";

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

  it("shows a third, danger answer only when a choice asks for one", async () => {
    const plain = askConfirm({ title: "Delete?", confirmLabel: "Delete" });
    expect(render(PromptModal).body).not.toContain("Stop Them and Quit");
    cancelPrompt();
    expect(await plain).toBe(false);

    const choice = askChoice({ title: "Leave the Terminals Running?", confirmLabel: "Quit and Leave Running", alternateLabel: "Stop Them and Quit" });
    const { body } = render(PromptModal);
    expect(body).toContain("Stop Them and Quit");
    expect(body).toContain("Quit and Leave Running");
    completePrompt(ALTERNATE_ANSWER);
    expect(await choice).toBe("alternate");

    const confirmed = askChoice({ title: "t", alternateLabel: "a" });
    completePrompt(true);
    expect(await confirmed).toBe("confirm");
    const cancelled = askChoice({ title: "t", alternateLabel: "a" });
    cancelPrompt();
    expect(await cancelled).toBeNull();
  });

  it("an alternate answer never confirms a plain confirm", async () => {
    const plain = askConfirm({ title: "Delete?" });
    completePrompt(ALTERNATE_ANSWER);
    expect(await plain).toBe(false);
  });
});
