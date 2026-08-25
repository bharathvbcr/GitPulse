import { describe, expect, it } from "vitest";
import {
  askConfirm,
  askText,
  cancelPrompt,
  completePrompt,
  promptState,
} from "../modalStore";
import { get } from "svelte/store";

describe("modalStore prompts", () => {
  it("askText resolves the confirmed string", async () => {
    const pending = askText({ title: "Rename branch" });
    expect(get(promptState)?.options.title).toBe("Rename branch");
    completePrompt("feat/x");
    await expect(pending).resolves.toBe("feat/x");
    expect(get(promptState)).toBeNull();
  });

  it("askText resolves null on Escape / backdrop / explicit null", async () => {
    const escaped = askText({ title: "t" });
    cancelPrompt();
    await expect(escaped).resolves.toBeNull();

    const nulled = askText({ title: "t" });
    completePrompt(null);
    await expect(nulled).resolves.toBeNull();

    // A non-string answer (host bug) must not leak through as a string.
    const coerced = askText({ title: "t" });
    completePrompt(true);
    await expect(coerced).resolves.toBeNull();
  });

  it("askConfirm resolves true only on explicit confirm and false on cancel", async () => {
    const yes = askConfirm({ title: "Delete branch?" });
    expect(get(promptState)?.options.mode).toBe("confirm");
    completePrompt(true);
    await expect(yes).resolves.toBe(true);

    const noEscape = askConfirm({ title: "Delete branch?" });
    cancelPrompt();
    await expect(noEscape).resolves.toBe(false);

    const noNull = askConfirm({ title: "Delete branch?" });
    completePrompt(null);
    await expect(noNull).resolves.toBe(false);
  });

  it("a newer prompt retires the older one as cancelled instead of deadlocking it", async () => {
    const first = askText({ title: "first" });
    const second = askText({ title: "second" });
    await expect(first).resolves.toBeNull();
    expect(get(promptState)?.options.title).toBe("second");
    completePrompt("ok");
    await expect(second).resolves.toBe("ok");
  });

  it("completing with no open prompt is a no-op", () => {
    expect(() => completePrompt("x")).not.toThrow();
    expect(() => cancelPrompt()).not.toThrow();
    expect(get(promptState)).toBeNull();
  });
});
