import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { get } from "svelte/store";
import { afterEach, describe, expect, it, vi } from "vitest";
import { repoStore, type RepoState } from "../stores/repoStore";
import { VIEW_REGISTRY } from "../views/viewRegistry";
import { buildCommands, helpCommands, repoUnavailable, worktreeUnavailable } from "./catalog";
import { PALETTE_MODES, rankItems } from "./model";
import { interfaceStore } from "../stores/interfaceStore";
import { themeStore } from "../stores/themeStore";
import { completePrompt, cancelPrompt, promptState } from "../stores/modalStore";
import { openSetupWizard } from "../tools/onboardingStore";
import { promptQuickCommit } from "../commit/quickCommit";
import { LAUNCHERS } from "../terminal/tabs";
import { terminalLaunchRequests } from "../terminal/launchRequests";
import { explorerLens, resetExplorerLens } from "../files/explorerLens";

vi.mock("../tools/onboardingStore", () => ({ openSetupWizard: vi.fn() }));
vi.mock("../commit/quickCommit", () => ({ promptQuickCommit: vi.fn().mockResolvedValue({ ok: true }) }));

const snapshot = (patch: Partial<RepoState> = {}): RepoState => ({ ...get(repoStore), ...patch });
const TARGET = { tabId: "tab-1", path: "/repo" };
// The store answers whether a target is still in front; these tests drive the
// published state by hand, so the answer is read from it.
const inFront = () => vi.spyOn(repoStore, "targetIsActive").mockImplementation(target => Boolean(target) && get(repoStore).currentPath === target?.path && get(repoStore).activeTabId === target?.tabId);
const ready = () => snapshot({ activeTabId: "tab-1", currentPath: "/repo", currentBranch: "main", isLoading: false, isBare: false, operation: { operation: null, probeFailed: false } });
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
  it("keeps a door to the reflog after it folded into Graph, landing on Graph's HEAD-moves list", async () => {
    // The registry's section loop no longer covers it (Reflog is not a
    // section), so this is the test that the retirement kept its door.
    const calls: string[] = [];
    vi.spyOn(interfaceStore, "setGlobalSurface").mockImplementation(() => {});
    vi.spyOn(repoStore, "setActiveTab").mockImplementation((tab, section) => { calls.push(`tab:${tab}:${section}`); });
    vi.spyOn(repoStore, "setHistoryList").mockImplementation(list => { calls.push(`list:${list}`); });
    const command = buildCommands(ready(), () => {}).find(item => item.id === "history:head-moves");
    expect(command?.label).toContain("Reflog");
    expect(command?.disabledReason).toBeUndefined();
    await command?.action();
    // Order matters: naming Graph resets the list to commits, so the list
    // must be switched after it.
    expect(calls).toEqual(["tab:history:graph", "list:moves"]);
    expect(buildCommands(ready(), () => {}).some(item => item.id === "history:reflog")).toBe(false);
  });
  it("keeps a door to Search and Blame, which are toggles inside Explorer rather than sections", async () => {
    // They were sections of Code, and the registry generated their commands.
    // Folding them into Explorer must not cost the palette its way to them:
    // each still answers to its old id and label, lands on Explorer, and
    // arrives with its toggle on.
    const navigate = vi.spyOn(repoStore, "setActiveTab").mockImplementation(() => {});
    const surface = vi.spyOn(interfaceStore, "setGlobalSurface").mockImplementation(() => {});
    resetExplorerLens();
    const commands = buildCommands(ready(), () => {});
    const search = commands.find(command => command.id === "code:search");
    expect(search?.label).toBe("Open Search — find text across the repository");
    expect(search?.disabledReason).toBeUndefined();
    await search?.action();
    expect(navigate).toHaveBeenLastCalledWith("code", "explorer");
    expect(surface).toHaveBeenLastCalledWith("repository");
    expect(get(explorerLens)).toMatchObject({ rail: "search", searchFocusPending: true, lens: "edit" });
    const blame = commands.find(command => command.id === "code:blame");
    expect(blame?.label).toBe("Open Blame — line authorship and code age");
    await blame?.action();
    expect(navigate).toHaveBeenLastCalledWith("code", "explorer");
    expect(get(explorerLens).lens).toBe("blame");
    expect(buildCommands(snapshot({ currentPath: null }), () => {}).find(command => command.id === "code:search")?.disabledReason).toBeTruthy();
    // Ids stay unique now that two are hand-written beside the generated ones.
    const ids = commands.map(command => command.id);
    expect(new Set(ids).size).toBe(ids.length);
    resetExplorerLens();
  });
  it("still finds secrets after the Secrets section folded into Health", async () => {
    // "Scan working tree for secrets" was the retired section's door. Typing
    // "secrets" must still land on the pane that scans them, not on nothing.
    const navigate = vi.spyOn(repoStore, "setActiveTab").mockImplementation(() => {});
    const commands = buildCommands(ready(), () => {});
    expect(commands.some(c => c.id === "insights:secrets")).toBe(false);
    const sections = rankItems(commands, "secrets").filter(c => c.id.startsWith("insights:"));
    expect(sections[0]?.id).toBe("insights:health");
    expect(sections[0]?.label).toBe("Scan dependencies and secrets");
    await sections[0]?.action();
    expect(navigate).toHaveBeenLastCalledWith("insights", "health");
  });
  it("finds the dependency scan under both of the words it replaced", () => {
    // "Scan npm vulnerabilities and updates" and "Scan working tree for
    // secrets" became this one command. The scans start when Health mounts
    // (harness/healthChecks.js: "opening Health starts the secrets scan
    // together with the dependency scan"), so the command is only honest while
    // both old queries still rank it first among the Insights sections.
    const commands = buildCommands(ready(), () => {});
    for (const query of ["npm", "vulnerabilities", "outdated", "dependencies", "secrets", "working tree"]) {
      const first = rankItems(commands, query).find(c => c.id.startsWith("insights:"));
      expect(first?.id, `"${query}" should reach Health`).toBe("insights:health");
    }
  });
  it("reveals the repository surface when opening any view or view section from the palette", async () => {
    const surface = vi.spyOn(interfaceStore, "setGlobalSurface").mockImplementation(() => {});
    const commands = buildCommands(ready(), () => {});
    const workCommand = commands.find(c => c.id === "work");
    await workCommand?.action();
    expect(surface).toHaveBeenCalledWith("repository");

    surface.mockClear();
    const sectionCommand = commands.find(c => c.id === "work:overview");
    await sectionCommand?.action();
    expect(surface).toHaveBeenCalledWith("repository");
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
    for (const id of ["fleet", "agents", "open_repo", "settings", "theme", "shortcuts", "optional_tools_setup"]) {
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
    const terminal = vi.spyOn(repoStore, "toggleTerminal").mockImplementation(() => false);
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
    // Every repository command names the repository the palette was built for.
    expect(fetch).toHaveBeenCalledWith(undefined, TARGET);
    expect(pull).toHaveBeenCalledWith(undefined, undefined, undefined, TARGET);
    expect(push).toHaveBeenCalledWith(undefined, undefined, false, TARGET);
    expect(stash).toHaveBeenCalledWith(undefined, undefined, TARGET);
    expect(pop).toHaveBeenCalledWith(TARGET);
    expect(fleet).toHaveBeenCalledWith(true);
    for (const preference of ["system", "light", "dark"] as const) {
      await commands.find(command => command.id === `theme_${preference}`)?.action();
      expect(appearance).toHaveBeenLastCalledWith(preference);
    }
  });
  it("dispatches each supported overlay event and the optional tools wizard", async () => {
    const dispatchEvent = vi.fn(); vi.stubGlobal("window", { dispatchEvent });
    const commands = buildCommands(ready(), () => {});
    for (const [id, event] of [["settings", "gitpulse:settings"], ["mcp_setup", "gitpulse:settings"], ["shortcuts", "gitpulse:shortcuts"], ["diagnostics", "gitpulse:diagnostics"], ["clean_branches", "gitpulse:branch-cleanup"]]) {
      await commands.find(command => command.id === id)?.action();
      expect(dispatchEvent.mock.lastCall?.[0].type).toBe(event);
    }
    await helpCommands(() => {}).find(command => command.id === "help_shortcuts")?.action();
    expect(dispatchEvent.mock.lastCall?.[0].type).toBe("gitpulse:shortcuts");
    await commands.find(command => command.id === "optional_tools_setup")?.action();
    expect(openSetupWizard).toHaveBeenCalledWith("devmap", "explain");
  });
  it("prompts before branch changes, propagates outcomes, and hands the store the repository the palette was built for", async () => {
    let current = ready();
    vi.spyOn(repoStore, "subscribe").mockImplementation(listener => { listener(current); return () => {}; });
    const create = vi.spyOn(repoStore, "createBranch").mockResolvedValue({ ok: true });
    const rename = vi.spyOn(repoStore, "renameBranch").mockResolvedValue({ ok: false, error: "Branch exists" });
    const commands = buildCommands(current, () => {});
    const startCreate = commands.find(command => command.id === "new_branch");
    let action = startCreate?.action();
    expect(get(promptState)?.options.title).toBe("Create branch");
    completePrompt(" feature/new "); await expect(action).resolves.toEqual({ ok: true });
    expect(create).toHaveBeenCalledWith("feature/new", undefined, TARGET);
    action = commands.find(command => command.id === "rename_branch")?.action();
    completePrompt(" renamed "); await expect(action).resolves.toEqual({ ok: false, error: "Branch exists" });
    expect(rename).toHaveBeenCalledWith("main", "renamed", TARGET);
    action = startCreate?.action(); cancelPrompt(); await expect(action).resolves.toEqual({ ok: false });
    // A repository that changed while the prompt was open is the store's to
    // refuse (repoStore.test.ts): the palette names the one it was built for
    // and never "whichever is in front now".
    action = startCreate?.action(); current = { ...current, activeTabId: "tab-2", currentPath: "/other" }; completePrompt("wrong-repo");
    await action;
    expect(create).toHaveBeenLastCalledWith("wrong-repo", undefined, TARGET);
    expect(create).toHaveBeenCalledTimes(2);
  });
  it("offers a command for every launcher, opening the dock on the repository it was run in", async () => {
    // Starting a session used to require opening the dock and then finding
    // the launcher dropdown and the + beside it.
    const current = ready();
    vi.spyOn(repoStore, "subscribe").mockImplementation((listener) => { listener(current); return () => {}; });
    inFront();
    const open = vi.spyOn(repoStore, "setTerminalOpen").mockImplementation(() => true);
    const requested: { path: string; launcher: string; prompt: string | undefined }[] = [];
    vi.spyOn(terminalLaunchRequests, "request").mockImplementation(async (path, launcher, prompt) => {
      requested.push({ path, launcher, prompt });
    });

    const commands = buildCommands(current, () => {});
    for (const launcher of LAUNCHERS) {
      const command = commands.find((item) => item.id === `terminal-new-${launcher.kind}`);
      expect(command?.disabledReason, launcher.kind).toBeUndefined();
      await command?.action();
    }
    expect(requested).toEqual(LAUNCHERS.map((launcher) => ({ path: "/repo", launcher: launcher.kind, prompt: undefined })));
    // The dock has to be shown too, or the request expires unclaimed against
    // a panel that never becomes visible.
    expect(open).toHaveBeenCalledTimes(LAUNCHERS.length);
    expect(open).toHaveBeenLastCalledWith(true);
  });

  it("refuses to start a session in a repository the user has since switched away from", async () => {
    let current = ready();
    vi.spyOn(repoStore, "subscribe").mockImplementation((listener) => { listener(current); return () => {}; });
    inFront();
    const open = vi.spyOn(repoStore, "setTerminalOpen").mockImplementation(() => true);
    const request = vi.spyOn(terminalLaunchRequests, "request").mockResolvedValue(undefined);

    const commands = buildCommands(current, () => {});
    const command = commands.find((item) => item.id === "terminal-new-shell");
    current = { ...current, currentPath: "/elsewhere" };
    await expect(command?.action()).rejects.toThrow("active repository changed");
    // Nothing may be started, and no dock opened, in the wrong repository.
    expect(request).not.toHaveBeenCalled();
    expect(open).not.toHaveBeenCalled();
  });

  it("disables every session command with no repository open", () => {
    const commands = buildCommands(snapshot({ currentPath: null }), () => {});
    for (const launcher of LAUNCHERS) {
      expect(commands.find((item) => item.id === `terminal-new-${launcher.kind}`)?.disabledReason).toBe("Open a repository first.");
    }
    expect(commands.find((item) => item.id === "terminal-dock")?.disabledReason).toBe("Open a repository first.");
  });

  it("routes recent/open repositories by path, disambiguates their names, and verifies activation", async () => {
    const tab = (id: string, path: string, isActive: boolean) => ({ id, path, name: "same", label: `${id}/same`, pinned: false, isActive, isBare: false, isDirty: false, isLoading: false, error: null, currentBranch: "main", conflictedCount: 0, changedCount: 0 });
    let current = { ...ready(), activeTabId: "b", currentPath: "/two/same", openTabs: [tab("a", "/one/same", false), tab("b", "/two/same", true), tab("c", "/three/same", false)], recentRepos: ["/one/same", "/four/same"] };
    vi.spyOn(repoStore, "subscribe").mockImplementation(listener => { listener(current); return () => {}; });
    const open = vi.spyOn(repoStore, "openRepo").mockImplementation(async path => { current = { ...current, currentPath: path }; return true; });
    const move = vi.spyOn(repoStore, "moveTabBy").mockImplementation(() => true);
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

describe("palette labels agree with the native menu", () => {
  // The palette is sentence case (UI_RULES §3); the native menu keeps title
  // case, which is the platform convention. They may differ in case and in a
  // leading "Open " or "Use " (the palette is a flat list; the menu's
  // submenus supply that context), and the palette may say more after the
  // menu's words ("Fetch" and "Fetch all remotes"). They may not use other
  // words for the same action. The Rust file is the menu's only table.
  const menu = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..", "src-tauri", "src", "desktop", "menu.rs"), "utf8");
  const menuLabel = (action: string): string => {
    const found = new RegExp(`item\\(app, actions::${action}, "([^"]+)"`).exec(menu);
    if (!found) throw new Error(`menu.rs has no item for actions::${action}`);
    return found[1];
  };
  const words = (label: string): string => label.toLowerCase().replace(/…/g, "").replace(/\s+/g, " ").trim();
  const bare = (label: string): string => words(label).replace(/^(open|use) /, "");
  const SHARED: ReadonlyArray<readonly [paletteId: string, menuAction: string]> = [
    ["open_repo", "OPEN"], ["quick_commit", "QUICK_COMMIT"], ["new_branch", "CREATE_BRANCH"], ["rename_branch", "RENAME_BRANCH"],
    ["rebase", "REBASE"], ["stash", "STASH"], ["stash_pop", "STASH_POP"], ["fetch", "FETCH"], ["pull", "PULL"], ["push", "PUSH"],
    ["refresh", "REFRESH"], ["theme_system", "THEME_SYSTEM"], ["theme_light", "THEME_LIGHT"], ["theme_dark", "THEME_DARK"],
    ["zoom_in", "ZOOM_IN"], ["zoom_out", "ZOOM_OUT"], ["zoom_reset", "RESET_ZOOM"], ["settings", "SETTINGS"],
    ["diagnostics", "DIAGNOSTICS"], ["optional_tools_setup", "SETUP_TOOLS"],
  ];

  it.each(SHARED)("%s says what the menu's %s item says", (paletteId, menuAction) => {
    const command = buildCommands(ready(), () => {}).find(item => item.id === paletteId);
    expect(command, `palette has no command ${paletteId}`).toBeDefined();
    expect(bare(command?.label ?? "").startsWith(bare(menuLabel(menuAction)))).toBe(true);
  });

  it("is sentence case outside surface names, while the menu stays title case", () => {
    const commands = buildCommands(ready(), () => {});
    expect(commands.find(item => item.id === "fetch")?.label).toBe("Fetch all remotes");
    expect(commands.find(item => item.id === "stash")?.label).toBe("Stash working tree");
    expect(menuLabel("STASH")).toBe("Stash Working Tree");
    const titleCased = commands
      .map(item => item.label.split(" — ")[0])
      .filter(label => /^[A-Z][a-z]+ [A-Z][a-z]+/.test(label) && !/^(Open|Toggle|New|Switch to) [A-Z]/.test(label));
    expect(titleCased).toEqual([]);
  });
});
