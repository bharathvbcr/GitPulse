import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { firstScriptBlock } from "./lib/dom/markupText";

const source = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "App.svelte"), "utf8");

function scriptAndTemplate(svelte: string): { script: string; template: string } {
  const block = firstScriptBlock(svelte);
  if (!block) {
    throw new Error("App.svelte is missing a script block");
  }
  return { script: block.inner, template: block.after };
}

function importedBindings(script: string): Set<string> {
  const names = new Set<string>();
  for (const match of script.matchAll(/import\s+(\w+)\s+from\s+/g)) {
    names.add(match[1]);
  }
  for (const match of script.matchAll(/import\s+(?:type\s+)?\{([^}]+)\}/g)) {
    const isTypeOnly = /import\s+type\s+\{/.test(script.slice(match.index, match.index + 20));
    if (isTypeOnly) continue;
    for (const spec of match[1].split(",")) {
      const trimmed = spec.trim();
      if (!trimmed || trimmed.startsWith("type ")) continue;
      const local = trimmed.split(/\s+as\s+/).at(-1)?.trim();
      if (local) names.add(local);
    }
  }
  return names;
}

function usedComponents(template: string): string[] {
  return [...new Set([...template.matchAll(/<([A-Z][A-Za-z0-9]*)\b/g)].map((match) => match[1]))];
}

/**
 * Views the app does not need in order to start. Each is fetched as its own
 * chunk on first use, which is what keeps the entry chunk under the budget the
 * `gitpulse-bundle-budget` Vite plugin enforces — and, through TerminalPanel,
 * keeps the 334 KB xterm runtime out of startup entirely.
 *
 * A plain `import Foo from "./lib/components/Foo.svelte"` anywhere in App.svelte
 * pulls the view straight back into the entry chunk. That is invisible in
 * review and shows up only as a build that fails weeks later on an unrelated
 * change, so it is pinned here.
 */
const DEFERRED_VIEWS = [
  "CodeView",
  "HistoryView",
  "CoverageViewer",
  "HealthPanel",
  "StoragePanel",
  "TerminalPanel",
  "CodeStackViewer",
  "GitHubPanel",
  "ManviOpsPanel",
  "ReflogViewer",
  "ConflictEditor",
  "PulseView",
];

describe("App view code splitting", () => {
  const { script, template } = scriptAndTemplate(source);

  it.each(DEFERRED_VIEWS)("loads %s lazily rather than at startup", (view) => {
    expect(
      script,
      `${view} is statically imported, which puts it back in the entry chunk`,
    ).not.toMatch(new RegExp(`import\\s+${view}\\s+from`));
    expect(script, `${view} has no dynamic loader`).toContain(
      `const load${view} = () => import(`,
    );
    // Used in the template, not necessarily as `load={…}` on a LazyView:
    // a loader may be handed to a view that owns the section it belongs to
    // (History takes the reflog's, for Graph's HEAD-moves list). What must never happen is a loader that
    // is declared and then reaches nothing, which is a chunk nobody loads.
    // A lazy view's own loaders travel in its LazyView `props={{ … }}`.
    expect(template, `load${view} is declared but never rendered`).toMatch(
      new RegExp(`(=\\{|:\\s*)load${view}\\b`),
    );
  });

  /**
   * The default tab. Deferring it would trade a smaller entry chunk for a
   * round trip on every launch, which is the opposite of the point.
   *
   * Work is a section host now, so the property has to be checked through one
   * level of delegation: App imports the host eagerly, and the host imports
   * the pane its default section renders eagerly. Asserting only App's own
   * import would still pass if WorkspaceView had made Overview lazy — which
   * is exactly the startup round trip this forbids.
   */
  it("keeps the default Work view eager, through its host", () => {
    expect(script).toMatch(/import\s+WorkspaceView\s+from/);
    expect(template).toContain("<WorkspaceView");
    const host = readFileSync(
      join(dirname(fileURLToPath(import.meta.url)), "lib/components/WorkspaceView.svelte"),
      "utf8",
    );
    expect(host).toMatch(/import\s+WorkView\s+from/);
    expect(host).toContain("<WorkView />");
  });

  /**
   * LazyView keys its cache on the loader's identity, so an inline arrow would
   * be a fresh function on every render: cache miss, refetch, and a remount of
   * the view under the user on every parent update.
   */
  it("passes only stable module-scope loaders, never inline arrows", () => {
    expect(template).not.toMatch(/load=\{\s*\(\)\s*=>/);
  });
});

/**
 * Overlays the app does not need in order to start.
 *
 * None is on screen at launch and most sessions open none of them, yet all six
 * were parsed at boot because App mounted them unconditionally to hold their
 * `isOpen` prop. They are mounted on FIRST open and kept mounted after, so the
 * exit transition still plays and the second open costs nothing.
 */
const DEFERRED_OVERLAYS = [
  "RebaseModal",
  "CloneModal",
  "SettingsModal",
  "ShortcutsModal",
  "CommandPalette",
  "DiagnosticsModal",
  "BranchCleanupModal",
  "SetupWizard",
];

describe("App overlay code splitting", () => {
  const { script, template } = scriptAndTemplate(source);

  it.each(DEFERRED_OVERLAYS)("loads %s lazily rather than at startup", (overlay) => {
    expect(
      script,
      `${overlay} is statically imported, which puts it back in the entry chunk`,
    ).not.toMatch(new RegExp(`import\\s+${overlay}\\s+from`));
    expect(script, `${overlay} has no dynamic loader`).toContain(
      `const load${overlay} = () => import(`,
    );
    expect(template, `load${overlay} is declared but never rendered`).toMatch(
      new RegExp(`load=\\{load${overlay}\\}`),
    );
  });

  it("keeps each overlay mounted once opened so its exit transition can play", () => {
    // Unmounting on close would cut the out: transition mid-fade and re-run
    // the component's setup on every reopen. The latch is what makes deferral
    // invisible rather than a new flicker.
    for (const latch of [
      "rebaseMounted",
      "cloneMounted",
      "settingsMounted",
      "shortcutsMounted",
      "diagnosticsMounted",
      "branchCleanupMounted",
      "setupWizardMounted",
    ]) {
      expect(script, `${latch} latch missing`).toContain(`let ${latch} = $state(false)`);
      expect(template, `${latch} does not gate a render`).toContain(`{#if ${latch}}`);
    }
  });

  /**
   * The palette registers its own ⌘K listener on mount, so before it has ever
   * been opened that listener does not exist. Without App answering the first
   * press, deferring the palette would silently break the chord that opens it
   * — the failure mode of "it works on the second try, sometimes".
   */
  it("answers the first ⌘K itself, before the palette chunk exists", () => {
    expect(script).toContain("if (!paletteMounted && (e.metaKey || e.ctrlKey) && e.key === \"k\")");
    expect(script).toContain("function openCommandPalette()");
    expect(script).toContain("paletteOpenSignal += 1");
    expect(script).toContain('window.addEventListener("gitpulse:palette", openDeferredPalette)');
    expect(script).toContain('window.removeEventListener("gitpulse:palette", openDeferredPalette)');
    expect(script).toContain("if (!paletteMounted) openCommandPalette()");
    expect(template).toContain("openSignal: paletteOpenSignal");
    expect(template).toContain("onClone: openCloneDialog");
    expect(template).toContain("onRebase: openRebaseDialog");
    expect(script).toContain("clone: openCloneDialog");
    expect(script).toContain("rebase: openRebaseDialog");
  });

  it("routes the native menu's palette action through the same arming path", () => {
    expect(script).toContain("palette: () => openCommandPalette()");
  });
});

describe("App overlay wiring", () => {
  it("imports every PascalCase component the template instantiates", () => {
    const { script, template } = scriptAndTemplate(source);
    const imported = importedBindings(script);
    const missing = usedComponents(template).filter((name) => !imported.has(name));
    expect(missing).toEqual([]);
    expect(imported.has("PromptModal")).toBe(true);
  });

  it("no longer strips the commit-search bar across every view", () => {
    // It used to be a full-width row App stacked above the sidebar whenever
    // `showsCommitFilter` said so — one of four horizontal bands before any
    // content. The search is the view bar's field now, so App must not mount
    // a bar of its own at all.
    expect(source).not.toContain("<FilterBar");
    expect(source).not.toContain("<PageSearch");
    expect(source).not.toContain("showsCommitFilter");
  });

  it("lands native Search Commits on History's graph before focusing the field", () => {
    // The field filters commits only on Graph; elsewhere it finds or runs the
    // palette. Switching the view alone left History's Diff section showing,
    // where the menu item focused a find instead of the commit filter.
    const fn = source.slice(
      source.indexOf("async function focusCommitSearch"),
      source.indexOf("onMount(() => {"),
    );
    expect(fn).toContain("tabForCommitSearch");
    expect(fn).toContain("repoStore.setActiveTab(target, COMMIT_FILTER_SECTION)");
    expect(fn).toContain("focusPageSearch()");
    expect(source).toContain("focusFilter: () => void focusCommitSearch()");
  });

  it("sends ⌘F to the field on every page, never by view or section", () => {
    // History's Diff and Code used to keep the chord for toolbar find bars of
    // their own. Those panes find through the field now, so ⌘F switches
    // nothing: it focuses the field, and the page decides what the field does.
    const handler = source.slice(source.indexOf("isCommitSearchChord(e) &&"));
    const branch = handler.slice(0, handler.indexOf("return;"));
    expect(branch).toContain("fieldOwnsFindChord({");
    expect(branch).toContain('onRepositorySurface: $interfaceStore.globalSurface === "repository"');
    expect(branch).toContain("defaultPrevented: e.defaultPrevented");
    expect(branch).toContain("focusPageSearch();");
    expect(branch).not.toContain("focusCommitSearch");
  });

  it("opens Clone and Rebase for the palette the view bar runs inline", () => {
    // The field's palette is mounted outside App, so its Clone and Rebase
    // commands cannot call App's openers directly.
    expect(source).toContain('window.addEventListener("gitpulse:clone", openCloneDialog)');
    expect(source).toContain('window.addEventListener("gitpulse:rebase", openRebaseDialog)');
  });

  it("keeps PromptModal and DiagnosticsModal in separate crash boundaries", () => {
    const promptIdx = source.indexOf("<PromptModal");
    // Diagnostics is deferred to its first open, so it reaches the tree
    // through LazyMount — the boundary isolation this case is about is
    // unchanged by that, and must stay unchanged by it.
    const diagIdx = source.indexOf("load={loadDiagnosticsModal}");
    expect(promptIdx).toBeGreaterThan(-1);
    expect(diagIdx).toBeGreaterThan(-1);
    const between =
      promptIdx < diagIdx ? source.slice(promptIdx, diagIdx) : source.slice(diagIdx, promptIdx);
    expect(between).toContain("</svelte:boundary>");
  });

  it("guards browser unload and native quit while editor drafts are dirty", () => {
    expect(source).toContain('listen<void>("gitpulse-exit-requested"');
    expect(source).toContain('invoke("cmd_set_exit_guard_ready")');
    // The quit's choice for sessions the terminal host keeps reaches the
    // backend with the exit itself (`quitChoice.ts`).
    // Managed runs a hosted Manvi keeps get the same choice, in the same call.
    expect(source).toContain('invoke("cmd_exit_app", quitChoices)');
    expect(source).toContain("...(terminals ? { terminals } : {})");
    expect(source).toContain("...(managedRuns.managed ? { managed: managedRuns.managed } : {})");
    expect(source).toContain("decideQuitTerminals({");
    expect(source).toContain('window.addEventListener("beforeunload"');
    expect(source).toContain("await editorFileSaveQueue.whenIdle()");
    expect(source).toContain("hasUnsavedEditorDrafts()");
    expect(source).toContain("Quit with Unsaved Edits?");
    expect(source).toContain("Conflict drafts are retained for recovery when local storage is available");
    // Every deferred save is flushed before the native exit, through the one
    // registry the stores join, not a list App keeps by hand.
    const flushCall = source.indexOf("    flushBeforeExit();\n    exitApproved = true;");
    expect(flushCall).toBeGreaterThan(-1);
    expect(flushCall).toBeLessThan(source.indexOf('await invoke("cmd_exit_app"'));
    expect(source.indexOf("decideQuitTerminals({")).toBeLessThan(flushCall);
    expect(source).toContain("for (const failure of flushPendingWrites())");
    expect(source).not.toContain("conflictSessions.flush()");
  });

  it("asks about managed runs on every quit, whichever terminal backend holds the sessions", () => {
    const start = source.indexOf("async function answerNativeExitRequest()");
    const body = source.slice(start, source.indexOf("\n  }\n", start));
    const managed = body.indexOf("decideQuitManagedRuns({");
    // Not inside the host-only branch: the sidecar closes its runs with the terminal host on or off.
    expect(managed).toBeGreaterThan(-1);
    expect(managed).toBeLessThan(body.indexOf("decideQuitTerminals({"));
    expect(body).toContain("listRuns: listAllLiveRuns");
    expect(body).toMatch(/if \(managedRuns\.cancelled\) \{ cancelNativeExitRequest\(\); return; \}/);
    expect(source).toContain('import { listAllLiveRuns } from "./lib/workbench/client"');
  });

  it("answers every native quit request it saw, so a held quit or logout is never stranded", () => {
    const start = source.indexOf("async function answerNativeExitRequest()");
    const end = source.indexOf("\n  }\n", start);
    expect(start).toBeGreaterThan(-1);
    const body = source.slice(start, end);
    const returns = body.match(/return;/g) ?? [];
    const cancelled = body.match(/cancelNativeExitRequest\(\);\s*return;/g) ?? [];
    // The one return that answers nothing is the repeat of an open request.
    expect(body).toContain("if (exitRequestPending) return;");
    expect(cancelled.length).toBe(returns.length - 1);
    expect(cancelled.length).toBeGreaterThanOrEqual(5);
    expect(body).toContain("cancelNativeExitRequest();\n      diagnostics.error(\"desktop:exit\"");

    // The acknowledgement goes out before the confirmation can await anything.
    const listener = source.slice(source.indexOf('listen<void>("gitpulse-exit-requested"'));
    expect(listener.indexOf('answer: "seen"')).toBeGreaterThan(-1);
    expect(listener.indexOf('answer: "seen"')).toBeLessThan(listener.indexOf("answerNativeExitRequest()"));
  });

  it("owns boot unlistens through createListenerTracker, not an inline array", () => {
    // The tracker is the one owner of the late-registration race. An inline
    // `unsubs` + `disposed` copy next to it is how the helper goes unwired
    // while App still has to solve the same problem.
    const { script } = scriptAndTemplate(source);
    expect(script).toContain('import { createListenerTracker } from "./lib/dom/listenerTracker"');
    expect(script).toContain("createListenerTracker()");
    expect(script).not.toContain("const unsubs:");
    expect(script).not.toMatch(/let disposed = false;/);
  });

  it("scans GitHub alerts when a repository opens and warns on serious findings", () => {
    const { script } = scriptAndTemplate(source);
    expect(script).toContain("maybeNotifyGithubAlerts");
    expect(script).toContain("loadGithubAlerts");
    expect(script).toContain("autoScanGithubAlerts");
    expect(script).toContain('setActiveTab("insights", "health")');
    expect(script).toContain('diagnostics.warn("github-alerts:check"');
    expect(script).toContain("scanGithubAlerts(path)");
    expect(script).toContain("interfaceStore.subscribe");
  });

  it("forwards repoStore.error without tracking the diagnostics/toast/setError writes", () => {
    const { script } = scriptAndTemplate(source);
    expect(script).toMatch(/import\s*\{[^}]*\buntrack\b[^}]*\}\s*from\s*"svelte"/);
    const start = script.indexOf("const err = repoError;");
    expect(start).toBeGreaterThan(-1);
    const effect = script.slice(script.lastIndexOf("$effect", start), start + 280);
    expect(effect).toContain("untrack(() => {");
    expect(effect.indexOf("untrack(() => {")).toBeLessThan(effect.indexOf("diagnostics.error"));
    expect(effect.indexOf("untrack(() => {")).toBeLessThan(effect.indexOf("toastStore.error"));
    expect(effect.indexOf("untrack(() => {")).toBeLessThan(effect.indexOf("repoStore.setError(null)"));
  });

  it("catches up the harness journal only on a genuine repository path change", () => {
    const { script } = scriptAndTemplate(source);
    // Keyed on the canonical path store and a derived trust flag, both
    // compared by value, so a status poll does not re-run it.
    expect(script).toContain("const trustRequired = $derived($repoStore.trustRequired);");
    const start = script.indexOf("const path = $currentRepoPath;\n    if (trustRequired) return;");
    expect(start).toBeGreaterThan(-1);
    const effect = script.slice(start, start + 400);
    expect(effect).toContain("untrack(() => {");
    expect(effect.indexOf("untrack(() => {")).toBeLessThan(
      effect.indexOf("harnessStore.activateRepository"),
    );
    expect(effect.indexOf("untrack(() => {")).toBeLessThan(effect.indexOf("harnessStore.catchUp"));
  });
});


describe("App chrome preferences", () => {
  it("has one Open entry point, on the tab strip, with Clone wired beside it", () => {
    // The title bar used to carry its own Open/Clone menu next to the tab
    // strip's + Open: two controls for one action. The tab strip owns both now.
    expect(source).not.toContain("HeaderRepoMenu");
    expect(source).toContain(
      "<RepoTabBar onOpen={() => void repoStore.pickAndOpenRepo()} onClone={openCloneDialog} />",
    );
  });

  it("plates Tasks on the shared workspace surface so macOS glass does not drop field text to black", () => {
    // The repository pane carries gp-workspace; that plate is the contrast
    // floor under translucent chrome. Tasks sat on the transparent shell, so
    // WKWebView painted native fields (search, task editor, workspace editor)
    // with light-scheme black text on the dark hue field.
    const start = source.indexOf('paneCrashes.report("tasks"');
    const end = source.indexOf('paneCrashes.report("fleet"');
    expect(start).toBeGreaterThan(-1);
    expect(end).toBeGreaterThan(start);
    const tasks = source.slice(start, end);
    expect(tasks).toContain("gp-workspace");
    expect(tasks).toContain("bg-background");
  });

  it("hands a crashed sidebar back to its boundary when the repository changes", () => {
    // The sidebar is no longer rebuilt per repository, so the recovery that
    // rebuild gave a crashed pane has to be explicit. The browser harness
    // `tab-switch` forces the crash and proves the recovery.
    const start = source.indexOf('paneCrashes.report("sidebar"');
    expect(start).toBeGreaterThan(-1);
    expect(source.slice(Math.max(0, start - 120), start)).toContain("failed={sidebarFailed}");
    expect(source).toMatch(/\{#snippet sidebarFailed[\s\S]*?<RepoPaneRecovery \{reset\} \/>[\s\S]*?\{\/snippet\}/);
  });

  it("isolates StatusBar crashes under the status pane boundary", () => {
    const start = source.indexOf('paneCrashes.report("status"');
    expect(start).toBeGreaterThan(-1);
    const window = source.slice(Math.max(0, start - 220), start + 160);
    expect(window).toContain("<svelte:boundary failed={paneFailed}");
    expect(window).toContain("<StatusBar");
  });

  it("keeps the repository bar mounted so chrome and shortcuts stay reachable", () => {
    // Unmounting the entire bar when hiding the tab strip would drop Fleet,
    // Tasks, Open, Recents and the app-wide keydown listener.
    const idx = source.indexOf("<RepoTabBar");
    expect(idx).toBeGreaterThan(-1);
    expect(source).toContain("<RepoTabBar onOpen={() => void repoStore.pickAndOpenRepo()} onClone={openCloneDialog} />");
    expect(source.slice(Math.max(0, idx - 160), idx)).not.toContain("autoHideRepoTabs");
  });

  it("gates the diagnostics button on the shared rule, not an inline error count", () => {
    // showsDiagnosticsButton keys off every recorded entry, so choosing
    // "when recorded" cannot bury warnings the error badge never counted.
    expect(source).toContain(
      "showsDiagnosticsButton($interfaceStore.diagnosticsButton, $diagnostics.length)",
    );
  });
});


describe("App display preferences", () => {
  /**
   * Three preferences that live in the document rather than in a component.
   * A store field with no applier is the classic half-wired setting: it
   * persists, it renders as selected, and it changes nothing on screen.
   */
  it.each([
    ["applyAccent", "applyAccent($interfaceStore.accent, $themeStore)"],
    ["applyReduceMotion", "applyReduceMotion($interfaceStore.reduceMotion)"],
    ["applyTabWidth", "applyTabWidth($interfaceStore.tabWidth)"],
  ])("applies %s from the store", (name, call) => {
    expect(source, `${name} is never imported`).toContain(`import { ${name} }`);
    expect(source, `${name} is imported but never called from the store`).toContain(call);
  });

  it("re-applies the UI scale only when the scale itself changes", () => {
    // interfaceStore publishes a fresh object per preference write; reading
    // `$interfaceStore.uiFontScale` inside the effect re-ran the webview
    // setZoom IPC on every unrelated emission.
    const { script } = scriptAndTemplate(source);
    expect(script).toContain("const uiFontScale = $derived($interfaceStore.uiFontScale);");
    const call = script.indexOf("applyUiScale(scale");
    expect(call).toBeGreaterThan(-1);
    const effect = script.slice(script.lastIndexOf("$effect(() => {", call), call);
    expect(effect).toContain("const scale = uiFontScale;");
    expect(effect).not.toContain("$interfaceStore");
  });

  it("re-applies the accent when the resolved theme flips", () => {
    // Each accent is a light/dark pair; keying only on the choice would leave
    // the dark shade painted on a light window.
    const { script } = scriptAndTemplate(source);
    const call = script.indexOf("applyAccent($interfaceStore.accent");
    expect(call).toBeGreaterThan(-1);
    expect(script.slice(call)).toContain("$themeStore");
  });
});

const viewBarSource = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "lib", "components", "ViewBar.svelte"),
  "utf8",
);

describe("the two header rows", () => {
  it("puts the title bar above the view bar, and the repository tabs inside the title bar", () => {
    const header = source.indexOf("<header");
    const headerEnd = source.indexOf("</header>");
    const tabs = source.indexOf("<RepoTabBar");
    const viewBar = source.indexOf("<ViewBar");
    expect(header).toBeGreaterThan(-1);
    expect(tabs).toBeGreaterThan(header);
    expect(tabs).toBeLessThan(headerEnd);
    // Row 2 follows row 1 and sits under the same crash boundary.
    expect(viewBar).toBeGreaterThan(headerEnd);
    expect(source.indexOf("</svelte:boundary>", viewBar)).toBeGreaterThan(viewBar);
    expect(source.slice(source.indexOf("<svelte:boundary"), header)).toContain('paneCrashes.report("header"');
  });

  it("drops the wordmark: the title bar carries the logo mark and no control after it but the tabs", () => {
    const row = source.slice(source.indexOf("<header"), source.indexOf("</header>"));
    expect(row).toContain("<Logo size={19}");
    expect(row).not.toContain(">GitPulse<");
    // The old inline Refresh moved to the view bar.
    expect(row).not.toContain("Refresh Status");
    expect(row).not.toContain("RefreshCw");
  });

  it("orders the host-wide chips Manvi then Diagnostics, last, so the badge sits at the corner", () => {
    const row = source.slice(source.indexOf('class="gp-titlebar-actions'), source.indexOf("</header>"));
    expect(row.indexOf("<HarnessBadge")).toBeGreaterThan(-1);
    expect(row.indexOf("<HarnessBadge")).toBeLessThan(row.indexOf('aria-label="Open Diagnostics"'));
  });

  it("makes each row its own container and gives both the dense glass fill", () => {
    expect(source.slice(source.indexOf("<header"), source.indexOf("<header") + 200)).toContain("@container");
    expect(source.slice(source.indexOf("<header"), source.indexOf("<header") + 300)).toContain("gp-repo-tabs");
    expect(viewBarSource).toContain("@container gp-glass gp-repo-tabs");
    expect(viewBarSource).toMatch(/<nav[^>]*\bdata-viewbar\b/s);
  });

  it("stamps the traffic-light inset a class the full-screen rule can collapse", () => {
    expect(source).toContain('class="gp-traffic-inset h-full shrink-0"');
    expect(source).not.toContain("w-[76px]");
  });
});

/**
 * The view switcher's row is `min-w-full w-max`, so everything in it counts
 * toward the scroller's width. A drag spacer with a minimum width, or padding
 * after the last control, made the row wider than the scroller by that much
 * while every control was in view: the overflow cue then sat on the right edge
 * over empty space (at ~912-940px wide), and its chevron scrolled into
 * nothing. The view bar keeps the spacer OUTSIDE the scroller for that reason.
 */
describe("title-bar overflow", () => {
  const start = viewBarSource.indexOf("bind:this={scroller}");
  const end = viewBarSource.indexOf("<ScrollCue target={scroller}", start);
  const region = viewBarSource.slice(start, end);

  it("is measured from the controls, not from empty space", () => {
    expect(start).toBeGreaterThanOrEqual(0);
    expect(end).toBeGreaterThan(start);
    const row = /<div class="([^"]*min-w-full[^"]*w-max[^"]*|[^"]*w-max[^"]*min-w-full[^"]*)"([^>]*)>/.exec(region);
    expect(row).not.toBeNull();
    // No padding after the last control...
    expect(row![1]).not.toMatch(/(^|\s)(px|pr|pe)-/);
    // ...and no spacer element after it either; the row itself is the drag
    // region, which a bare attribute limits to direct hits on the row.
    expect(row![2]).toMatch(/\sdata-tauri-drag-region(\s|$)/);
    expect(region.match(/data-tauri-drag-region/g)).toHaveLength(1);
  });

  it("refuses the tab-changing native menu items while a prompt is open", () => {
    const gate = source.slice(source.indexOf("canDispatch: (event) =>"), source.indexOf("open: () => void repoStore.pickAndOpenRepo()"));
    expect(gate).toContain("!exitRequestPending");
    expect(gate).toContain("!(isPromptOpen() && blockedWhilePrompting(event.id))");
    expect(gate.indexOf("blockedWhilePrompting")).toBeLessThan(gate.indexOf("canDispatchMenuEvent("));
  });
});
