import { describe, expect, it } from "vitest";
import { get } from "svelte/store";
import {
  LAUNCHERS,
  SESSION_TYPES,
  initialState,
  launcherLabel,
  offeredSessionKind,
  openTab,
  sessionTypesFor,
  tabLabel,
} from "./tabs";
import { agentPromptArgs, createTerminalLaunchRequests } from "./launchRequests";
import { applyUiScale, pageZoom } from "../ui/uiScale";

describe("the browser session type", () => {
  it("is offered after every process launcher, and only where it is built", () => {
    expect(SESSION_TYPES.map((t) => t.kind)).toEqual([...LAUNCHERS.map((l) => l.kind), "browser"]);
    expect(sessionTypesFor(true).some((t) => t.kind === "browser")).toBe(true);
    expect(sessionTypesFor(false)).toBe(LAUNCHERS);
    // The process-only list that settings iterate never names it.
    expect(LAUNCHERS.some((l) => (l.kind as string) === "browser")).toBe(false);
  });

  it("is kept as the next session type only on a host that offers it", () => {
    expect(offeredSessionKind("browser", true)).toBe("browser");
    expect(offeredSessionKind("browser", false)).toBe("shell");
    expect(offeredSessionKind("claude", false)).toBe("claude");
    expect(offeredSessionKind("netscape", true)).toBe("shell");
    expect(offeredSessionKind(undefined, true)).toBe("shell");
  });

  it("opens a tab called Browser until its page names itself", () => {
    const state = openTab(initialState(), "browser");
    const tab = state.tabs.at(-1);
    expect(tab?.launcher).toBe("browser");
    expect(state.activeId).toBe(tab?.id);
    expect(launcherLabel("browser")).toBe("Browser");
    expect(tab && tabLabel(tab)).toBe("Browser");
  });

  it("refuses a prompt or a task launch, since it runs no process to hand one to", () => {
    const before = initialState();
    expect(openTab(before, "browser", "do the thing")).toBe(before);
    expect(openTab(before, "browser", { runId: "r1", title: "Task" })).toBe(before);
  });

  it("can be requested from the palette, without a prompt", async () => {
    expect(agentPromptArgs("browser")).toBeNull();
    expect(() => agentPromptArgs("browser", "inspect the header")).toThrow(/Initial prompts require/);
    const requests = createTerminalLaunchRequests();
    const pending = requests.request("/repo", "browser");
    expect(get(requests)?.launcher).toBe("browser");
    requests.take("/repo")?.complete();
    await expect(pending).resolves.toBeUndefined();
    await expect(requests.request("/repo", "browser", "a prompt")).rejects.toThrow(/Initial prompts require/);
  });
});

describe("the page zoom browser sessions are placed by", () => {
  it("is the scale the webview zoom took, and 1 when the stylesheet carried it", async () => {
    const root = { style: { setProperty: () => {} } };
    await applyUiScale(1.25, { setZoom: async () => {}, root });
    expect(get(pageZoom)).toBe(1.25);
    await applyUiScale(1.25, { setZoom: async () => { throw new Error("refused"); }, root });
    expect(get(pageZoom)).toBe(1);
    await applyUiScale(3, { setZoom: async () => {}, root });
    expect(get(pageZoom)).toBe(1.5);
    await applyUiScale(1.1, { root });
    expect(get(pageZoom)).toBe(1);
  });
});
