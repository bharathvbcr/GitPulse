import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const source = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "App.svelte"), "utf8");

function scriptAndTemplate(svelte: string): { script: string; template: string } {
  const match = svelte.match(/<script\b[^>]*>([\s\S]*?)<\/script>/);
  if (!match || match.index === undefined) {
    throw new Error("App.svelte is missing a script block");
  }
  return { script: match[1], template: svelte.slice(match.index + match[0].length) };
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
    expect(template, `load${view} is declared but never rendered`).toMatch(
      new RegExp(`=\\{load${view}\\}`),
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
    expect(template).toContain("openSignal: paletteOpenSignal");
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
    expect(source).toContain('invoke("cmd_exit_app")');
    expect(source).toContain('window.addEventListener("beforeunload"');
    expect(source).toContain("await editorFileSaveQueue.whenIdle()");
    expect(source).toContain("hasUnsavedEditorDrafts()");
    expect(source).toContain("Discard Unsaved Edits and Quit?");
  });
});


describe("App chrome preferences", () => {
  it("drops the header action labels without dropping their accessible name", () => {
    // The words are decoration; the icon plus aria-label is what identifies
    // the button, so a decluttered header stays usable by a screen reader.
    expect(source).toContain('aria-label="Open a repository"');
    expect(source).toContain('aria-label="Clone a repository"');
    expect(source).toContain(
      "{#if $interfaceStore.showHeaderActionLabels}<span>Open...</span>{/if}",
    );
    expect(source).toContain(
      "{#if $interfaceStore.showHeaderActionLabels}<span>Clone...</span>{/if}",
    );
  });

  it("hides the repository tab strip only while a single repository is open", () => {
    // Hiding it with several tabs open would strand the other repositories.
    const idx = source.indexOf("<RepoTabBar");
    expect(idx).toBeGreaterThan(-1);
    expect(source.slice(Math.max(0, idx - 160), idx)).toContain(
      "{#if !$interfaceStore.autoHideRepoTabs || $repoStore.openTabs.length > 1}",
    );
  });

  it("gates the diagnostics button on the shared rule, not an inline error count", () => {
    // showsDiagnosticsButton keys off every recorded entry, so choosing
    // "when recorded" cannot bury warnings the error badge never counted.
    expect(source).toContain(
      "showsDiagnosticsButton($interfaceStore.diagnosticsButton, $diagnostics.length)",
    );
  });
});
