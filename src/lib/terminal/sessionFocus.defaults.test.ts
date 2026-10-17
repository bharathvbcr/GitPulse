import { beforeEach, describe, expect, it, vi } from "vitest";
import { writable } from "svelte/store";

const state = writable({
  openTabs: [{ id: "t1", path: "/r/app" }],
  activeTabId: "t1" as string | null,
});
const fakeRepoStore = Object.assign(
  { subscribe: state.subscribe },
  {
    activateTab: vi.fn(async (_id: string) => {}),
    setTerminalOpen: vi.fn((_open: boolean) => true),
    openRepo: vi.fn(async (_path: string) => true),
  },
);
const setGlobalSurface = vi.fn();

vi.mock("../stores/repoStore", () => ({ repoStore: fakeRepoStore }));
vi.mock("../stores/interfaceStore", () => ({
  interfaceStore: { setGlobalSurface: (surface: string) => setGlobalSurface(surface) },
}));

const { focusTerminalSession, repoStoreFocus } = await import("./sessionFocus");

beforeEach(() => {
  fakeRepoStore.activateTab.mockClear();
  fakeRepoStore.setTerminalOpen.mockClear();
  fakeRepoStore.openRepo.mockClear();
  setGlobalSurface.mockClear();
  state.set({ openTabs: [{ id: "t1", path: "/r/app" }], activeTabId: "t1" });
});

describe("the app's own stores behind the session jump", () => {
  it("shows the repository surface, reads the tabs, and defers to the next render", async () => {
    repoStoreFocus.showRepositorySurface();
    expect(setGlobalSurface).toHaveBeenCalledWith("repository");
    expect(repoStoreFocus.snapshot()).toEqual({ openTabs: [{ id: "t1", path: "/r/app" }], activeTabId: "t1" });
    await expect(repoStoreFocus.afterRender()).resolves.toBeUndefined();
  });

  it("hands tab, dock and open requests to the repository store unchanged", async () => {
    await repoStoreFocus.activateTab("t2");
    expect(fakeRepoStore.activateTab).toHaveBeenCalledWith("t2");
    expect(repoStoreFocus.setTerminalOpen(true)).toBe(true);
    expect(fakeRepoStore.setTerminalOpen).toHaveBeenCalledWith(true);
    await expect(repoStoreFocus.openRepo("/r/other")).resolves.toBe(true);
    expect(fakeRepoStore.openRepo).toHaveBeenCalledWith("/r/other");
  });

  it("jumps to a session in the tab already in front without switching", async () => {
    const reveal = vi.fn();
    const outcome = await focusTerminalSession({ repoPath: "/r/app", reveal, family: null } as never);
    expect(outcome).toEqual({ ok: true, switchedRepo: false, openedDock: true });
    expect(setGlobalSurface).toHaveBeenCalledWith("repository");
    expect(reveal).toHaveBeenCalledTimes(1);
    expect(fakeRepoStore.activateTab).not.toHaveBeenCalled();
  });

  it("switches to the tab that owns a session running elsewhere", async () => {
    state.set({
      openTabs: [
        { id: "t1", path: "/r/app" },
        { id: "t2", path: "/r/other" },
      ],
      activeTabId: "t1",
    });
    const reveal = vi.fn();
    const outcome = await focusTerminalSession({ repoPath: "/r/other", reveal, family: null } as never);
    expect(outcome).toMatchObject({ ok: true, switchedRepo: true });
    expect(fakeRepoStore.activateTab).toHaveBeenCalledWith("t2");
  });
});
