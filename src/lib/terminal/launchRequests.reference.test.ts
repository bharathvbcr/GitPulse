import { afterEach, describe, expect, it, vi } from "vitest";
import { claimedReference, createTerminalLaunchRequests, TERMINAL_SESSION_CANCELED } from "./launchRequests";
import { AGENT_MESSAGE_MAX_BYTES } from "../workbench/sendToAgent";

afterEach(() => vi.useRealTimers());

describe("requestReference: text for the recipient session, from outside the panel", () => {
  it("is claimed by the panel for that repository as text, not as a session to open", async () => {
    const requests = createTerminalLaunchRequests();
    const sent = requests.requestReference("/repo", "@a.ts#L1-L2 ", "/repo-wt/x");
    expect(requests.take("/other")).toBeNull();
    const claimed = requests.take("/repo");
    expect(claimed && claimedReference(claimed)).toEqual({ text: "@a.ts#L1-L2 ", checkout: "/repo-wt/x" });
    claimed?.complete();
    await expect(sent).resolves.toBeUndefined();
  });

  it("rejects with the panel's reason when no session could take it, so the caller can say so", async () => {
    const requests = createTerminalLaunchRequests();
    const sent = requests.requestReference("/repo", "@a.ts ", "/repo");
    requests.take("/repo")?.complete("Open an agent or shell session first to send a reference.");
    await expect(sent).rejects.toThrow("Open an agent or shell session first");
    expect(requests.busy).toBe(false);
  });

  it("rejects when nothing claims it within the wait, instead of staying silent", async () => {
    vi.useFakeTimers();
    const requests = createTerminalLaunchRequests();
    const sent = requests.requestReference("/repo", "@a.ts ", "/repo");
    const outcome = expect(sent).rejects.toThrow("did not take the text");
    await vi.advanceTimersByTimeAsync(10_000);
    await outcome;
    expect(requests.take("/repo")).toBeNull();
  });

  it("rejects when cancelled, and frees the channel", async () => {
    const requests = createTerminalLaunchRequests();
    const controller = new AbortController();
    const sent = requests.requestReference("/repo", "@a.ts ", "/repo", controller.signal);
    controller.abort();
    await expect(sent).rejects.toThrow(TERMINAL_SESSION_CANCELED);
    expect(requests.busy).toBe(false);
  });

  it("refuses empty text and text past the message bound before asking a panel", async () => {
    const requests = createTerminalLaunchRequests();
    await expect(requests.requestReference("/repo", "  ", "/repo")).rejects.toThrow("nothing to send");
    await expect(requests.requestReference("/repo", "x".repeat(AGENT_MESSAGE_MAX_BYTES + 1), "/repo")).rejects.toThrow("longer than");
    expect(requests.busy).toBe(false);
  });

  it("refuses a second request while one is waiting, as every other request does", async () => {
    const requests = createTerminalLaunchRequests();
    const first = requests.requestReference("/repo", "@a.ts ", "/repo");
    await expect(requests.requestReference("/repo", "@b.ts ", "/repo")).rejects.toThrow();
    requests.take("/repo")?.complete();
    await first;
  });

  it("is not a reference when the request is a session launch", () => {
    expect(claimedReference({ launcher: "claude" })).toBeNull();
    expect(claimedReference({ launcher: "reference" })).toBeNull();
  });
});
