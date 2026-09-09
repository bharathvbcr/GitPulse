import { get } from "svelte/store";
import { afterEach, describe, expect, it, vi } from "vitest";
import { repoStore, type RepoState } from "../stores/repoStore";
import { VIEW_REGISTRY } from "../views/viewRegistry";
import { buildCommands, helpCommands, repoUnavailable, worktreeUnavailable } from "./catalog";
import { PALETTE_MODES } from "./model";
import { interfaceStore } from "../stores/interfaceStore";
import { themeStore } from "../stores/themeStore";
import { completePrompt, cancelPrompt, promptState } from "../stores/modalStore";
import { openSetupWizard } from "../tools/onboardingStore";
import { promptQuickCommit } from "../commit/quickCommit";

vi.mock("../tools/onboardingStore", () => ({ openSetupWizard: vi.fn() }));
vi.mock("../commit/quickCommit", () => ({ promptQuickCommit: vi.fn().mockResolvedValue({ ok: true }) }));

const snapshot = (patch: Partial<RepoState> = {}): RepoState => ({ ...get(repoStore), ...patch });
const ready = () => snapshot({ currentPath: "/repo", currentBranch: "main", isLoading: false, isBare: false, operation: { operation: null, probeFailed: false } });
afterEach(() => { cancelPrompt(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

describe("command catalog and context", () => {
  it("gives every view and every section a working command from the shared registry", async () => {
    const navigate = vi.spyOn(repoStore, "setActiveTab").mockImplementation(() => {});
    const commands = buildCommands(ready(), () => {});
    for (const view of Object.values(VIEW_REGISTRY)) {
      const command = commands.find(command => command.id === view.id);
      expect(command?.disabledReason).toBeUndefined();
      await command?.action();
      expect(navigate).toHaveBeenLastCalledWith(view.id);
      for (const section of view.sections ?? []) {
        const command = commands.find(command => command.id === `${view.id}:${section.id}`);
        expect(command?.description).toContain(section.summary);
        await command?.action();
        expect(navigate).toHaveBeenLastCalledWith(view.id, section.id);
      }
    }
  });
  it("opens global task boards without an active repository", async () => {
    const navigate = vi.spyOn(interfaceStore, "setGlobalSurface").mockImplementation(() => {});
    const command = buildCommands(snapshot({ currentPath: null }), () => {}).find(item => item.id === "tasks");
    expect(command).toBeDefined();
    expect(command?.disabledReason).toBeUndefined();
    await command?.action();
    expect(navigate).toHaveBeenCalledExactlyOnceWith("tasks");
  });
  it("keeps global actions reachable while unavailable repository commands explain why", () => {
    const commands = buildCommands(snapshot({ currentPath: null }), () => {});
    for (const id of ["fleet", "open_repo", "settings", "theme", "shortcuts", "optional_tools_setup"]) {
      expect(commands.find(command => command.id === id)?.disabledReason).toBeUndefined();
    }
    for (const id of ["fetch", "pull", "push", "refresh", "new_branch", "rename_branch", "stash", "stash_pop", "terminal-dock", "code", "history:diff"]) {
      expect(commands.find(command => command.id === id)?.disabledReason).toBe("Open a repository first.");
    }
  });
  it("distinguishes loading, bare, unknown operation, active operation, and conflicts", () => {
    expect(repoUnavailable(snapshot({ currentPath: "/repo", isLoading: true }))).toContain("loading");
    expect(worktreeUnavailable({ ...ready(), isBare: true })).toContain("bare repository");
    expect(worktreeUnavailable({ ...ready(), operation: { operation: null, probeFailed: true } })).toContain("unavailable");
    expect(worktreeUnavailable({ ...ready(), operation: { probeFailed: false, operation: { kind: "Merge", current_step: null, total_steps: null, head_ref: null, incoming_ref: null, conflicted_paths: [], conflicted_total: 0, available: [] } } })).toContain("Resolve");
    expect(worktreeUnavailable({ ...ready(), statuses: [{ path: "a", status_code: "UU", is_staged: false, is_conflicted: true, additions: 0, deletions: 0 }] })).toContain("conflicted");
  });
  it("does not silently offer stash or commit operations on an empty tree", () => {
    const commands = buildCommands(ready(), () => {});
    expect(commands.find(command => command.id === "quick_commit")?.disabledReason).toBe("Nothing to commit.");
    expect(commands.find(command => command.id === "stash_pop")?.disabledReason).toContain("No stash");
    const unknown = buildCommands({ ...ready(), stashFailed: true }, () => {});
    expect(unknown.find(command => command.id === "stash_pop")?.disabledReason).toContain("unavailable");
    const detached = buildCommands({ ...ready(), currentBranch: null }, () => {});
    expect(detached.find(command => command.id === "rename_branch")?.disabledReason).toContain("local branch");
  });
  it("makes mode commands stay open and prompt commands hand off focus", async () => {
    const changeMode = vi.fn();
    const commands = buildCommands(ready(), changeMode);
    expect(new Set(commands.map(command => command.id)).size).toBe(commands.length);
    for (const mode of PALETTE_MODES.filter(mode => mode.mode !== "commands")) {
      const command = commands.find(command => command.id === `search:${mode.mode}`);
      expect(command?.keepOpen).toBe(true);
      await command?.action();
      expect(changeMode).toHaveBeenLastCalledWith(mode.mode);
    }
    for (const id of ["settings", "new_branch", "quick_commit", "shortcuts", "diagnostics"]) {
      expect(commands.find(command => command.id === id)?.closeBefore).toBe(true);
    }
    for (const command of helpCommands(changeMode).filter(command => command.id.startsWith("help_") && command.keepOpen)) {
      await command.action();
      expect(PALETTE_MODES.map(mode => mode.mode)).toContain(changeMode.mock.lastCall?.[0]);
    }
  });
  it("returns mutation outcomes instead of discarding failures", async () => {
    vi.spyOn(repoStore, "fetch").mockResolvedValue({ ok: false, error: "Offline" });
    const command = buildCommands(ready(), () => {}).find(command => command.id === "fetch");
    await expect(command?.action()).resolves.toEqual({ ok: false, error: "Offline" });
  });
  it("opens host-owned dialogs and explains when a host cannot provide them", async () => {
    const host = { onClone: vi.fn(), onRebase: vi.fn() };
    const commands = buildCommands(ready(), () => {}, host);
    for (const id of ["clone_repo", "rebase"]) {
      const command = commands.find(command => command.id === id);
      expect(command?.disabledReason).toBeUndefined();
      expect(command?.closeBefore).toBe(true);
      await command?.action();
    }
    expect(host.onClone).toHaveBeenCalledOnce();
    expect(host.onRebase).toHaveBeenCalledOnce();
    expect(buildCommands(ready(), () => {}).find(command => command.id === "clone_repo")?.disabledReason).toContain("unavailable");
    const detached = buildCommands({ ...ready(), currentBranch: null }, () => {}, host);
    expect(detached.find(command => command.id === "rebase")?.disabledReason).toContain("branch");
  });
  it("delegates repository and appearance actions to their existing owners", async () => {
    const mutation = { ok: true };
    const fetch = vi.spyOn(repoStore, "fetch").mockResolvedValue(mutation);
    const pull = vi.spyOn(repoStore, "pull").mockResolvedValue(mutation);
    const push = vi.spyOn(repoStore, "push").mockResolvedValue(mutation);
    const stash = vi.spyOn(repoStore, "stashSave").mockResolvedValue(mutation);
    const pop = vi.spyOn(repoStore, "stashPop").mockResolvedValue(mutation);
    const refresh = vi.spyOn(repoStore, "refresh").mockResolvedValue();
    const pick = vi.spyOn(repoStore, "pickAndOpenRepo").mockResolvedValue();
    const close = vi.spyOn(repoStore, "closeActiveTab").mockResolvedValue();
    const next = vi.spyOn(repoStore, "nextTab").mockResolvedValue();
    const prev = vi.spyOn(repoStore, "prevTab").mockResolvedValue();
    const reopen = vi.spyOn(repoStore, "reopenLastClosed").mockResolvedValue();
    const fleet = vi.spyOn(interfaceStore, "setFleetOpen").mockImplementation(() => {});
    const terminal = vi.spyOn(interfaceStore, "toggleTerminalDock").mockImplementation(() => {});
    const avatars = vi.spyOn(interfaceStore, "toggleGraphAvatars").mockImplementation(() => {});
    const zoomIn = vi.spyOn(interfaceStore, "zoomIn").mockImplementation(() => {});
    const zoomOut = vi.spyOn(interfaceStore, "zoomOut").mockImplementation(() => {});
    const zoomReset = vi.spyOn(interfaceStore, "resetZoom").mockImplementation(() => {});
    const theme = vi.spyOn(themeStore, "toggle").mockImplementation(() => {});
    const appearance = vi.spyOn(themeStore, "setPreference").mockImplementation(() => {});
    const commands = buildCommands(ready(), () => {});
    const delegates = [
      ["fetch", fetch], ["pull", pull], ["push", push], ["stash", stash], ["stash_pop", pop], ["refresh", refresh],
      ["open_repo", pick], ["close_tab", close], ["next_tab", next], ["prev_tab", prev], ["reopen_tab", reopen],
      ["fleet", fleet], ["terminal-dock", terminal], ["toggle_author_avatars", avatars],
      ["zoom_in", zoomIn], ["zoom_out", zoomOut], ["zoom_reset", zoomReset], ["theme", theme], ["quick_commit", promptQuickCommit],
    ] as const;
    for (const [id, spy] of delegates) {
      await commands.find(command => command.id === id)?.action();
      expect(spy, id).toHaveBeenCalledOnce();
    }
    expect(fleet).toHaveBeenCalledWith(true);
    for (const preference of ["system", "light", "dark"] as const) {
      await commands.find(command => command.id === `theme_${preference}`)?.action();
      expect(appearance).toHaveBeenLastCalledWith(preference);
    }
  });
  it("dispatches each supported overlay event and the optional tools wizard", async () => {
    const dispatchEvent = vi.fn(); vi.stubGlobal("window", { dispatchEvent });
    const commands = buildCommands(ready(), () => {});
    for (const [id, event] of [["settings", "gitpulse:settings"], ["mcp_setup", "gitpulse:settings"], ["shortcuts", "gitpulse:shortcuts"], ["diagnostics", "gitpulse:diagnostics"]]) {
      await commands.find(command => command.id === id)?.action();
      expect(dispatchEvent.mock.lastCall?.[0].type).toBe(event);
    }
    await helpCommands(() => {}).find(command => command.id === "help_shortcuts")?.action();
    expect(dispatchEvent.mock.lastCall?.[0].type).toBe("gitpulse:shortcuts");
    await commands.find(command => command.id === "optional_tools_setup")?.action();
    expect(openSetupWizard).toHaveBeenCalledWith("devmap", "explain");
  });
  it("prompts before branch changes, propagates outcomes, and rejects a changed repository", async () => {
    let current = ready();
    vi.spyOn(repoStore, "subscribe").mockImplementation(listener => { listener(current); return () => {}; });
    const create = vi.spyOn(repoStore, "createBranch").mockResolvedValue({ ok: true });
    const rename = vi.spyOn(repoStore, "renameBranch").mockResolvedValue({ ok: false, error: "Branch exists" });
    const commands = buildCommands(current, () => {});
    const startCreate = commands.find(command => command.id === "new_branch");
    let action = startCreate?.action();
    expect(get(promptState)?.options.title).toBe("Create New Branch");
    completePrompt(" feature/new "); await expect(action).resolves.toEqual({ ok: true });
    expect(create).toHaveBeenCalledWith("feature/new");
    action = commands.find(command => command.id === "rename_branch")?.action();
    completePrompt(" renamed "); await expect(action).resolves.toEqual({ ok: false, error: "Branch exists" });
    expect(rename).toHaveBeenCalledWith("main", "renamed");
    action = startCreate?.action(); cancelPrompt(); await expect(action).resolves.toEqual({ ok: false });
    action = startCreate?.action(); current = { ...current, currentPath: "/other" }; completePrompt("wrong-repo");
    await expect(action).rejects.toThrow("active repository changed");
    expect(create).toHaveBeenCalledOnce();
  });
  it("routes recent/open repositories by path, disambiguates their names, and verifies activation", async () => {
    const tab = (id: string, path: string, isActive: boolean) => ({ id, path, name: "same", label: `${id}/same`, pinned: false, isActive, isBare: false, isDirty: false, isLoading: false, error: null, currentBranch: "main", conflictedCount: 0 });
    let current = { ...ready(), activeTabId: "b", currentPath: "/two/same", openTabs: [tab("a", "/one/same", false), tab("b", "/two/same", true), tab("c", "/three/same", false)], recentRepos: ["/one/same", "/four/same"] };
    vi.spyOn(repoStore, "subscribe").mockImplementation(listener => { listener(current); return () => {}; });
    const open = vi.spyOn(repoStore, "openRepo").mockImplementation(async path => { current = { ...current, currentPath: path }; return true; });
    const move = vi.spyOn(repoStore, "moveTabBy").mockImplementation(() => {});
    const navigate = vi.spyOn(repoStore, "setActiveTab").mockImplementation(() => {});
    const commands = buildCommands(current, () => {});
    expect(commands.filter(command => command.id.startsWith("recent:"))).toHaveLength(1);
    expect(commands.find(command => command.id === "switch:a")?.description).toBe("/one/same");
    await commands.find(command => command.id === "switch:a")?.action();
    expect(open).toHaveBeenLastCalledWith("/one/same");
    await commands.find(command => command.id === "recent:/four/same")?.action();
    expect(open).toHaveBeenLastCalledWith("/four/same");
    open.mockResolvedValueOnce(false);
    await expect(commands.find(command => command.id === "switch:a")?.action()).rejects.toThrow("could not be opened");
    open.mockResolvedValueOnce(false);
    await expect(commands.find(command => command.id === "recent:/four/same")?.action()).rejects.toThrow("could not be opened");
    await commands.find(command => command.id === "move_tab_left")?.action(); expect(move).toHaveBeenLastCalledWith("b", -1);
    await commands.find(command => command.id === "move_tab_right")?.action(); expect(move).toHaveBeenLastCalledWith("b", 1);
    await helpCommands(() => {}).find(command => command.id === "help_map_docs")?.action(); expect(navigate).toHaveBeenLastCalledWith("code", "map");
  });
});
