import { describe, expect, it, vi } from "vitest";
import { sendToAgentFromPage, type PageSendDeps } from "./sendFromPage";

function deps(overrides: Partial<PageSendDeps> = {}) {
  const calls: string[] = [];
  const value: PageSendDeps = {
    currentPath: () => "/repo",
    openDock: () => void calls.push("dock"),
    request: vi.fn(async () => void calls.push("request")),
    report: vi.fn(),
    ...overrides,
  };
  return { value, calls };
}

describe("sendToAgentFromPage", () => {
  it("asks the panel first and then shows the dock, and delivers the text with the file's checkout", async () => {
    const { value, calls } = deps();
    await expect(sendToAgentFromPage("@a.ts#L1 ", "/repo-wt/x", value)).resolves.toBe(true);
    expect(calls).toEqual(["request", "dock"]);
    expect(value.request).toHaveBeenCalledWith("/repo", "@a.ts#L1 ", "/repo-wt/x");
    expect(value.report).not.toHaveBeenCalled();
  });

  it("falls back to the repository as the checkout", async () => {
    const { value } = deps();
    await sendToAgentFromPage("x", undefined, value);
    expect(value.request).toHaveBeenCalledWith("/repo", "x", "/repo");
  });

  it("reports a refusal from the panel (no live session) instead of staying silent", async () => {
    const { value } = deps({ request: vi.fn(async () => { throw new Error("Open an agent or shell session first to send a reference."); }) });
    await expect(sendToAgentFromPage("x", undefined, value)).resolves.toBe(false);
    expect(value.report).toHaveBeenCalledWith(expect.stringContaining("Open an agent or shell session first"));
  });

  it("reports a request that timed out or was cancelled, and one refused because another is waiting", async () => {
    for (const reason of ["The terminal did not take the text in time.", "Terminal session canceled", "A terminal request is already waiting"]) {
      const { value } = deps({ request: vi.fn(async () => { throw new Error(reason); }) });
      await expect(sendToAgentFromPage("x", undefined, value)).resolves.toBe(false);
      expect(value.report).toHaveBeenCalledWith(expect.stringContaining(reason));
    }
  });

  it("reports when no repository is open, and asks nothing", async () => {
    const { value } = deps({ currentPath: () => null });
    await expect(sendToAgentFromPage("x", undefined, value)).resolves.toBe(false);
    expect(value.request).not.toHaveBeenCalled();
    expect(value.report).toHaveBeenCalledWith(expect.stringContaining("Open a repository"));
  });
});
