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
  "BlameViewer",
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
    // (History takes the reflog's). What must never happen is a loader that
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
    // content. The bar lives inside History's section bar now, which is the
    // only view it filters, so App must not mount it at all.
    expect(source).not.toContain("<FilterBar");
    expect(source).not.toContain("showsCommitFilter");
  });

  it("switches to Graph before focusing commit search when the bar is unmounted", () => {
    const fn = source.slice(
      source.indexOf("async function focusCommitSearch"),
      source.indexOf("onMount(() => {"),
    );
    expect(fn).toContain("tabForCommitSearch");
    expect(fn).toContain("repoStore.setActiveTab(target)");
    expect(fn).toContain("FOCUS_COMMIT_SEARCH_EVENT");
    expect(source).toContain("focusFilter: () => void focusCommitSearch()");
  });

  it("asks the chord owner about the section, not only about the view", () => {
    // History's Diff section reads a file's lines and owns ⌘F for finding
    // inside them; its Graph and Reflog sections are commit lists and hand
    // the chord to the filter that narrows them. Passing only the view made
    // ⌘F focus the commit filter over an open diff.
    expect(source).toContain(
      "ownsCommitSearchChord($repoStore.activeTab, activeSectionFor($repoStore.activeTab, $repoStore.viewSections))",
    );
    expect(source).toContain('import { activeSectionFor } from "./lib/views/viewRegistry"');
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
    expect(source).toContain('invoke("cmd_exit_app", terminals ? { terminals } : {})');
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
  it("merges Open and Clone into one header menu without dropping their actions", () => {
    // The two title-bar pills became one menu so Clone stops occupying a
    // second hit target. The trigger's accessible name, and the two item
    // labels, live on HeaderRepoMenu — App only has to keep both actions wired.
    expect(source).toContain("<HeaderRepoMenu");
    expect(source).toContain("onOpen={() => void repoStore.pickAndOpenRepo()}");
    expect(source).toContain("onClone={openCloneDialog}");
    expect(source).not.toContain(
      "{#if $interfaceStore.showHeaderActionLabels}<span>Open...</span>{/if}",
    );
    expect(source).not.toContain(
      "{#if $interfaceStore.showHeaderActionLabels}<span>Clone...</span>{/if}",
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
    expect(source).toContain("<RepoTabBar onOpen={() => void repoStore.pickAndOpenRepo()} />");
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

/**
 * The title-bar row is `min-w-full w-max`, so everything in it counts toward
 * the scroller's width. A drag spacer with a minimum width, or padding after
 * the last control, made the row wider than the header by that much while
 * every control was in view: the overflow cue then sat on the right edge over
 * empty space (at ~912-940px wide), and its chevron scrolled into nothing.
 */
describe("title-bar overflow", () => {
  const start = source.indexOf("bind:this={headerScroller}");
  const end = source.indexOf("<ScrollCue target={headerScroller}", start);
  const region = source.slice(start, end);

  it("is measured from the controls, not from empty space", () => {
    expect(start).toBeGreaterThanOrEqual(0);
    expect(end).toBeGreaterThan(start);
    const row = /<div class="([^"]*min-w-full w-max[^"]*)"([^>]*)>/.exec(region);
    expect(row).not.toBeNull();
    // No padding after the last control...
    expect(row![1]).not.toMatch(/(^|\s)(px|pr|pe)-/);
    // ...and no spacer element after it either; the row itself is the drag
    // region, which a bare attribute limits to direct hits on the row.
    expect(row![2]).toMatch(/\sdata-tauri-drag-region(\s|$)/);
    expect(region.match(/data-tauri-drag-region/g)).toHaveLength(1);
  });
});
