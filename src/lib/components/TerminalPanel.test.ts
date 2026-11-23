import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import TerminalPanel from "./TerminalPanel.svelte";
import DockHeader from "./DockHeader.svelte";
import { stripMarkupTags } from "../dom/markupText";
import { sessionCountLabel } from "../terminal/sessionList";

const headerProps = {
  section: "shell" as const,
  onSection: () => {},
  os: "linux" as const,
  repoName: "repo",
  repoPath: "/repo",
  sessionCountLabel: "",
  onToggleSessionList: () => {},
};

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "TerminalPanel.svelte"),
  "utf8",
);
// The dock's header row moved out of the panel into DockHeader.svelte; the
// assertions that used to read its markup from the panel read it from here.
const headerSource = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "DockHeader.svelte"),
  "utf8",
);

describe("TerminalPanel source contracts & safety hygiene", () => {
  // Rendered in harness/terminal.html: a Console command runs once,
  // tokenized, against this repository; one that does not tokenize is
  // refused before anything runs; a tab chord in the Console opens nothing;
  // an incomplete output states the backend's reason, or "reason unavailable"
  // without one, never an invented cap; Clear empties the output.
  it("hands Console launch requests to the Console", () => {
    expect(source).toContain("consoleLaunchRequests");
  });
  it("delegates every PTY session endpoint to TerminalSession", () => {
    // The strip owns which sessions exist, never how one talks to its shell.
    // Asserted as an absence so a session endpoint cannot creep back into the
    // panel and become a second owner of the PTY protocol.
    expect(source).toContain('import TerminalSession from "./TerminalSession.svelte";');
    for (const endpoint of [
      "cmd_terminal_spawn",
      "cmd_terminal_write",
      "cmd_terminal_resize",
      "cmd_terminal_kill",
    ]) {
      expect(source).not.toContain(endpoint);
    }
  });

  it("journals executed terminal actions into harnessStore", () => {
    expect(source).toContain("harnessStore.recordAction({");
  });

  it("attributes every terminal journal row to the repository that started it", () => {
    // Two here (Console run: succeeded, failed). The session-start row moved
    // to TerminalSession with the session; TerminalSession.test.ts holds it.
    const journalCalls = source.match(/harnessStore\.recordAction\(\{\s+repoPath,/g) ?? [];
    expect(journalCalls).toHaveLength(2);
  });

  it("offers a copy of each Console run", () => {
    expect(source).toContain("copyOutput");
  });
});

describe("TerminalPanel session ownership", () => {
  it("binds to the path it was given, not to the live currentPath", () => {
    // A hidden panel that still read $repoStore.currentPath would display
    // and journal against whichever repo the user had switched to, while its
    // PTY was still sitting in the worktree it was spawned in.
    expect(source).not.toContain("$repoStore.currentPath");
    expect(source).not.toContain("repoStore");
    expect(source).toContain("repoPath = null");
  });

  it("takes cross-repository navigation as a prop rather than reaching for the store", () => {
    // The Sessions list can jump to a shell running in another repository,
    // which means switching repository tabs — something this panel must not
    // do itself, because it cannot see the store that would tell it where it
    // is. The dock supplies the capability; this only renders the failure.
    expect(source).toContain("onGoToSession");
    expect(source).toContain("await onGoToSession(session)");
  });

  it("names each listed session by repository and checkout, matched by identity", () => {
    // Rows used to show `repoPath.split(...).pop()` — an agent worktree's
    // slug with no repository — and decided "other repository" by exact
    // string, so the panel's own checkout spelled differently read as
    // elsewhere. Both now come from `sessionRow` (terminal/checkoutLabel.ts).
    expect(source).toContain("sessionRow(session, repoPath, checkouts, pathOpts)");
    expect(source).toContain("groupSessionsByRepository(filteredSessions, repoPath, checkouts, pathOpts)");
    expect(source).toContain('data-testid="session-path"');
    expect(source).not.toContain("other checkout");
    // Each session is a direct child of the list. A section around the
    // repository made the group's first Go to the one a reader activated.
    const list = source.slice(
      source.indexOf('aria-label="Sessions across repositories"'),
      source.indexOf("{#if waitingForCapacity"),
    );
    expect(list).not.toContain("<section");
    expect(list).toContain('data-testid="session-group"');
    expect(source).not.toContain("session.repoPath.split(");
    expect(source).not.toMatch(/session\.repoPath\s*[!=]==\s*repoPath/);
    expect(source).toContain('data-testid="session-agent"');
  });

  it("links a resumed conversation's session back to its task, as it does an attempt's", () => {
    expect(source).toContain("{#if sessionInfo.taskRunId}");
    expect(source).toContain("openListedTask(sessionInfo.taskRunId)");
  });

  it("refits the visible session when its host is shown again", () => {
    const body = source.slice(source.indexOf("const revealedId = $derived"));
    expect(body).toContain('const revealedId = $derived(visible && mode === "shell" ? activeId : null);');
    expect(body).toContain("revealTab(id)");
    expect(body).toContain('if (!visible || mode !== "shell" || activeId !== id) return;');
  });

  it("re-runs the reveal effect only when the shown tab changes, never for the dock's props, another tab's output or a mount", () => {
    // reveal() takes focus. Before this, the effect re-ran when the dock rebuilt
    // its props object (every repository-store emission, `visible` unchanged),
    // when a background tab printed its first line (`unread`) and when any tab
    // mounted (`sessions`), each taking focus out of the session-type menu's
    // search box at a moment set by something else (harness/terminal.html
    // asserts the behaviour). It may read one derived value and nothing else.
    const start = source.indexOf("$effect(() => {", source.indexOf("const revealedId = $derived"));
    const effect = source.slice(start, source.indexOf("\n  });", start));
    const synchronous = effect.slice(0, effect.indexOf("void tick()")).replace(/^\s*\/\/.*$/gm, "");
    expect(synchronous).toContain("const id = revealedId;");
    expect(synchronous).toContain("untrack(() => {");
    const tracked = synchronous.replace(/untrack\(\(\) => \{[\s\S]*?\n    \}\);/, "");
    expect(tracked).not.toMatch(/\b(unread|sessions|browsers|visible|mode|activeId)\b/);
    expect(effect.slice(effect.indexOf("void tick()"))).toContain("revealTab(id)");
  });

  it("holds no PTY lifecycle of its own", () => {
    // The memo guard this replaces existed because an effect reading
    // $repoStore re-ran on every ~6s status poll and would have killed the
    // user's live shell per emission. There is no such effect now: a session
    // lives and dies with its own component. The dock keeps one panel per
    // visited repository tab, bound to that tab's path, so a switch cannot
    // retarget a hidden shell either.
    expect(source).not.toContain("ptyLifecycleKey");
    expect(source).not.toContain("spawnEpoch");
    expect(source).not.toContain("liveCleanupTarget");
  });

});

describe("TerminalPanel dock sections", () => {
  it("derives the pane mode from the section, so Agents and Console both take the panes off screen", () => {
    expect(source).toContain("let section = $state<DockSection>(get(dockSection));");
    expect(source).toContain('const mode = $derived<PtyMode>(section === "shell" || section === "files" ? "shell" : "console");');
    expect(source).not.toMatch(/\bmode = "(shell|console)"/);
    expect(source).toContain("dockSection.set(next);");
  });

  it("hides the strip and the pane canvas in the lenses instead of unmounting them", () => {
    expect(source).toContain('class:hidden={mode !== "shell"}');
    expect(source).not.toMatch(/\{#if (section|mode) === "shell"\}\s*<div class="terminal-strip-row/);
    expect(source).toContain("<DockAgentsSection");
    expect(source).toContain('{#if section === "agents"}');
  });

  it("keeps the explorer's open-target row to the house segmented control, without a second Explorer title", () => {
    const aside = source.slice(source.indexOf('data-testid="terminal-explorer-sidebar"'), source.indexOf('data-testid="terminal-explorer-send-file"'));
    expect(aside).toContain('class="gp-segmented min-w-0" data-size="sm" role="group" aria-label="Open files in"');
    expect(aside.match(/class="gp-seg-btn/g)?.length).toBe(3);
    expect(aside).not.toContain('<span class="truncate">Explorer</span>');
    // Narrow asides keep icons with an accessible name rather than clipping labels.
    expect(aside).toContain('<span class="@max-2xs:sr-only">Split</span>');
  });

  it("gives the sidebar toggle its own icon, distinct from the Files section", () => {
    const toggle = source.slice(source.indexOf('data-testid="terminal-explorer-sidebar-toggle"'), source.indexOf("</button>", source.indexOf('data-testid="terminal-explorer-sidebar-toggle"')));
    expect(toggle).toContain("<PanelLeft size={13} />");
    expect(toggle).not.toContain("FolderTree");
    expect(toggle).not.toContain("3rd split pane");
  });

  it("counts agents elsewhere that need the reader in the Agents dot, and tells the header when the active tab is listed", () => {
    const attention = source.slice(source.indexOf("const agentAttention"), source.indexOf("function selectAgentRow"));
    expect(attention).toContain("agentRowsElsewhere.filter((row) => row.attention)");
    expect(attention).toContain("const activeSessionListed");
    expect(source).toContain("{activeSessionListed}");
  });

  it("gives a narrow tab strip's room to the tabs and keeps the active tab in view", () => {
    const narrow = source.slice(source.indexOf("@container terminal-strip (max-width: 520px)"));
    // The Follow toggle (also in Agents) gives way unless it is on; both split buttons stay.
    expect(narrow).toMatch(/\.terminal-tab-open-split,\s*\.terminal-follow-agent:not\(\[aria-pressed="true"\]\) \{\s*display: none;/);
    expect(source).toContain('class="terminal-tab-open-split gp-icon-btn');
    expect(source).toContain('class="terminal-follow-agent gp-icon-btn');
    expect(source).not.toMatch(/terminal-split-(right|down)/);
    // Resizing the strip re-runs the reveal, which keeps clear of the scroll cue.
    const reveal = source.slice(source.indexOf("let tabStripWidth"), source.indexOf("async function splitActive"));
    expect(reveal).toContain("observeResize(scroller");
    expect(reveal).toContain("void tabStripWidth;");
    expect(reveal).toContain("TAB_STRIP_CUE_INSET_PX,");
  });

  it("lets the Files rail keep focus when entering Files also reveals the panes", () => {
    // Agents/Console -> Files flips `mode` back to shell, so the pane-reveal
    // effect focuses the active pane after a tick. The rail must win.
    const files = source.slice(source.indexOf("let filesFocusReady"), source.indexOf("let selectedExplorerFile"));
    expect(files).toContain("if (paneRevealPending) return;");
    expect(files).toContain("filesFocusOwed = true;");
    const reveal = source.slice(source.indexOf("const revealedId = $derived"), source.indexOf("// Any split sibling brought on screen"));
    expect(reveal).toContain("paneRevealPending = true;");
    // The rail takes its owed focus only after the pane has been revealed.
    expect(reveal.indexOf("settleFilesFocus();", reveal.indexOf("revealTab(id);"))).toBeGreaterThan(reveal.indexOf("revealTab(id);"));
  });

  it("shows the explorer in Files without writing the saved preference", () => {
    expect(source).toContain('(explorerPreferenceOpen || section === "files")');
    expect(source).toContain("explorerPreferenceOpen");
  });

  it("takes the section chords before the tab and split chords, from the panel and from xterm", () => {
    const sectionChord = source.slice(source.indexOf("function handleSectionChord"), source.indexOf("function handleChord"));
    expect(sectionChord).toContain("dockSectionForChord(event, $hostPlatform.os)");
    expect(sectionChord).toContain("event.preventDefault();");
    // xterm asks `onChord` (handleChord) before it sends a key to the PTY, so
    // the section chord must be answered there or a focused shell receives it.
    const chords = source.slice(source.indexOf("function handleChord"));
    expect(chords.indexOf("handleSectionChord(event)")).toBeGreaterThan(-1);
    expect(chords.indexOf("handleSectionChord(event)")).toBeLessThan(chords.indexOf('if (mode !== "shell") return false;'));
    const keys = source.slice(source.indexOf("function handlePanelKey"));
    expect(keys.indexOf("handleSectionChord(event)")).toBeLessThan(keys.indexOf("handleChord(event)"));
    expect(source).toContain("onChord={handleChord}");
  });

  it("moves the pair switcher and split indicator into the strip row, and the strip's session search and tab options into the dock menu", () => {
    const strip = source.slice(source.indexOf('class="terminal-strip-row'), source.indexOf("Only the tabs scroll"));
    expect(strip).toContain('data-testid="terminal-pair-switcher"');
    expect(strip).toContain('data-testid="terminal-header-split-indicator"');
    expect(strip).toContain('data-testid="terminal-hidden-split-alert"');
    expect(source).not.toContain('aria-label="Find terminal session" aria-expanded');
    expect(source).not.toContain('aria-label="Terminal tab options"');
    expect(headerSource).toContain('data-testid="dock-menu-find-session"');
    expect(headerSource).toContain('data-testid="dock-menu-tab-options"');
  });
});

describe("TerminalPanel rendering", () => {
  it("renders the header with the section switcher", () => {
    const { body } = render(TerminalPanel);
    expect(body).toContain('aria-label="Dock"');
    expect(body).toContain("Shell");
    expect(body).toContain("Agents");
    expect(body).toContain("Files");
    expect(body).toContain("Console");
  });

  it("labels the session count as open sessions against the limit", () => {
    // The count lives in the header's ⋯ menu now, so the menu is rendered open.
    const { body } = render(DockHeader, {
      props: { ...headerProps, sessionCountLabel: sessionCountLabel(3, 32), menuOpen: true },
    });
    const match = body.match(/data-testid="terminal-session-count"[^>]*>([\s\S]*?)<\/button>/);
    expect(match).not.toBeNull();
    const text = stripMarkupTags(match?.[1] ?? "").replace(/\s+/g, " ").trim();
    // The old control was an unlabeled `0/32` icon button.
    expect(text).toMatch(/^All sessions \d+ open \/ \d+ limit$/);
    expect(text).not.toBe("0/32");
    expect(headerSource).toContain('aria-controls="terminal-session-list"');
    expect(source).toContain('data-testid="session-group"');
    expect(source).toContain("sessionListNotice");
    expect(source).not.toContain('aria-label="Terminal shortcuts"');
    expect(source).not.toContain("{$terminalSessions.length}/{$terminalSessionLimit}");
  });
});

describe("TerminalPanel tab strip", () => {
  // Rendered in harness/terminal.html: the new-tab chord opens a second tab;
  // a new shell tab starts in the directory of the shell beside it; session
  // search filters and activates; the strip scrolls and compresses when
  // crowded and scrolls a new tab into view; the global limit disables New
  // and names itself, and raising it frees a place at once; close kills only
  // its own session; the Console switch preserves sessions; the four chrome
  // popovers sit above the grid and the launcher menu overlays it without
  // shifting it. What stays is what the fixture does not reach.
  it("never restarts the one session to change launcher", () => {
    expect(source).not.toContain("selectLauncher");
  });
  it("says the strip itself is full apart from the session limit", () => {
    // The fixture's session limit is reached long before the strip's bound.
    expect(source).toContain("const stripFull = $derived(!canOpenTab(tabState, MAX_STRIP_TABS))");
    expect(source).toContain("already holds {MAX_STRIP_TABS} tabs");
  });
  it("consumes a queued request only once its tab holds a session slot", () => {
    // Consumed when the tab opened, a request lost the race for the last
    // slots and was never retried. It stays queued until admitted, and a
    // refusal drops the tab and leaves the request waiting.
    expect(source).toContain("onAdmitted={() => admitTab(tab.id)}");
    expect(source).toContain("onRefused={() => refuseTab(tab.id)}");
    const opener = source.slice(source.indexOf("const admitting = new Set"), source.indexOf("function admitTab"));
    expect(opener).toContain("admitting.add(opened.id)");
    expect(opener.match(/consumeTaskTerminalRequest\(/g)).toHaveLength(1);
    // The refusal unmarks the tab only by dropping it, so the tab is never on
    // the strip unmarked, and it keeps the request for the next free slot.
    const refuse = source.slice(source.indexOf("function refuseTab"), source.indexOf("function refuseTab") + 600);
    expect(refuse).toContain("dropTab(id, true)");
    expect(refuse).not.toContain("admitting.delete(id)");
    // A tab closed by hand before it took a slot must give its room back, or
    // every such close shrinks this panel's room for queued starts for good.
    const drop = source.slice(source.indexOf("function dropTab"), source.indexOf("function handleChord"));
    expect(drop).toContain("admitting.delete(id)");
    expect(drop).toContain("if (keepRequest) releaseTaskTerminal(holder, request);");
    expect(drop).toContain("else consumeTaskTerminalRequest(request);");
  });

  it("routes chords through the shared parser rather than inline key tests", () => {
    expect(source).toContain("terminalTabChord(event)");
    expect(source).toContain("onChord={handleChord}");
  });

  it("drops the session handle when its tab closes", () => {
    // Leaving it in the record keeps a disposed component reachable and lets
    // the reveal effect call into it.
    const body = source.slice(source.indexOf("function dropTab"));
    expect(body).toContain("const { [id]: _gone, ...rest } = sessions;");
  });

  it("has a single mount that focuses the command input", () => {
    expect(source.match(/onMount\(/g)?.length).toBe(1);
    expect(source).toContain("inputEl?.focus()");
  });

  it("cancels the pending copy-feedback timer on teardown", () => {
    // The handle is captured so a fast unmount cannot fire the reset into a
    // dead component; rapid copies replace the timer instead of stacking.
    expect(source).toContain("let copiedResetTimer: ReturnType<typeof setTimeout> | null = null;");
    // Two clears total: one in copyOutput (replacing a pending reset), one in cleanup.
    expect(source.match(/clearTimeout\(copiedResetTimer\)/g)?.length).toBe(2);
    const start = source.indexOf("return () => {", source.indexOf("onMount("));
    expect(source.indexOf("clearTimeout(copiedResetTimer)", start)).toBeGreaterThan(start);
  });
});


describe("TerminalPanel agent-session record", () => {
  const claim = source.slice(
    source.indexOf("const claimed = terminalLaunchRequests.take("),
    source.indexOf("claimed.complete(opened ? undefined : capacityTitle);"),
  );

  it("remembers a prompt launch for every PROMPT_LAUNCHERS value, Grok and Antigravity included", () => {
    // `AgentTerminal.launcher` is `PromptLauncher`; a handwritten
    // `claude || codex` filter dropped the session row Coverage and Health
    // use to reveal a Grok or Antigravity run that did spawn.
    expect(claim).toContain("isPromptLauncher(claimed.launcher)");
    expect(claim).not.toMatch(/claimed\.launcher === "(claude|codex|grok|agy)"/);
    expect(claim).toContain("claimed.prompt !== undefined");
    expect(source).toMatch(/import \{[^}]*\bisPromptLauncher\b[^}]*\} from "\.\.\/terminal\/launchRequests"/);
  });
});

describe("TerminalPanel session failures are visible in Shell mode", () => {
  function body(name: string): string {
    const start = source.indexOf(`async function ${name}(`);
    expect(start).toBeGreaterThan(-1);
    const next = source.indexOf("\n  }\n", start);
    return source.slice(start, next);
  }

  it("renders the session error panel-level, ahead of the Console block", () => {
    // The old field was only drawn inside `{#if mode === "console"}`, so a
    // failed Go to / Close in Shell mode showed nothing at all.
    const strip = source.indexOf('data-testid="terminal-session-error"');
    expect(strip).toBeGreaterThan(-1);
    expect(strip).toBeLessThan(source.indexOf('{#if section === "console"}'));
    expect(source).toContain('role="alert"');
    expect(source).toContain('aria-label="Dismiss"');
  });

  it("writes session failures to sessionError, never to the Console's validationError", () => {
    for (const name of ["openListedTask", "goToSession", "closeListed", "requestClose"]) {
      const text = body(name);
      expect(text, name).toContain("sessionError =");
      expect(text, name).not.toContain("validationError =");
    }
  });

  it("offers the Claude + Antigravity pair split preset and passes snippet split support to TerminalSession", () => {
    expect(source).toContain("function openPairSplit()");
    expect(source).toContain('data-testid="terminal-pair-split-preset"');
    expect(source).toContain("onOpenSnippetSplit=");
  });

  it("subscribes to scoped child terminal spawns and agent open-file split requests scoped by repository identity", () => {
    expect(source).toContain("agentEvents.subscribePanelEvents({");
    expect(source).toContain("unsubAgentPanel();");
    expect(source).toContain("function handleChildSpawned(payload: TerminalChildSpawnedPayload)");
    expect(source).toContain("openAttachedChildTab(tabState, {");
    expect(source).toContain("function handleAgentOpenFile(payload: TerminalOpenFileRequestPayload)");
    expect(source).toContain("sameRepo(payload.repo_path, repoPath, pathOpts)");
  });

  it("gates cold-boot split hydration on detached session adoption, keeps split panes at their minimum height by scrolling, formats drops for recipient launcher, and guards 8-pane tab drops", () => {
    expect(source).toContain("detachedAdoptionSettled");
    expect(source).toContain("serializeSplitWorkspace(");
    // Panes are fractions of a canvas at least `minimumSplitHeight` tall, so a
    // short dock scrolls instead of crushing a split's grids to no rows; a
    // split pane's own min-height is never zeroed again.
    expect(source).toContain("minimumSplitHeight(splitRoot, activeId, zoomedPaneId)");
    expect(source).toContain("style:min-height={`${paneCanvasMinHeight}px`}");
    expect(source).not.toContain('.terminal-pane { min-height: 0; }');
    expect(source).toContain("function sendPathsToPreferredSession(paths: readonly string[])");
    expect(source).toContain("repoPath={tab.checkout ?? repoPath}");
    expect(source).toContain("fallbackPath={tab.fallbackPath}");
    expect(source).toContain("!hasPane(splitRoot, draggedTabId) && leafPaneIds(splitRoot).length >= MAX_SPLIT_PANES");
  });

  it("exposes Follow Agent toggle, live-reveals or refreshes touched files and plan artifacts, and wires onSendToAgent to TerminalSession", () => {
    expect(source).toContain('data-testid="terminal-follow-agent-toggle"');
    expect(source).toContain("onTouchedFile: handleAgentTouchedFile");
    expect(source).toContain("function handleAgentTouchedFile(payload: TerminalAgentTouchedFilePayload)");
    expect(source).toContain("reloadIfClean()");
    expect(source).toContain("onSendToAgent={sendReferenceToAgent}");
  });

  it("lets Follow Agent drive only the one pane it owns, and end when the user closes or takes it", () => {
    // The decision lives in terminal/followAgent.ts (behaviour-tested there);
    // these pin that the panel routes every touched file through it. Follow
    // used to retarget whichever file pane was visible — the user's own — and
    // open a fresh split per edit once that pane was closed.
    const handler = source.slice(
      source.indexOf("function handleAgentTouchedFile("),
      source.indexOf("function handleFileDropOnPane("),
    );
    expect(handler).toContain("decideFollow(followTarget, tabState.tabs, followPaneFile)");
    expect(handler).toContain('decision.kind === "stop"');
    expect(handler).not.toContain("existingVisibleFile");
    expect(handler).toContain("reuseSameFileOnly: true");
    expect(handler).toContain("if (!before.has(shown.id)) adoptFollowPaneById(shown.id);");
    expect(source).toContain("followPaneOpen(target, tabs)");
    // Every way follow turns on goes through startFollowing, which records the pane it owns.
    expect(source.match(/followAgent = true;/g)).toHaveLength(1);
    expect(source).toContain("startFollowing(fileId);");
    expect(source).toContain("onclick={() => { if (followAgent) stopFollowing(); else startFollowing(); }}");
  });

  it("wires parent-child supervision (↳ badge, Stop children, onChildClosed) and 1-click Claude <-> Antigravity pair handoff", () => {
    expect(source).toContain("onChildClosed: handleChildClosed");
    expect(source).toContain("function handleChildClosed(payload: TerminalChildClosedPayload)");
    expect(source).toContain('data-testid="terminal-tab-child-badge"');
    expect(source).toContain('data-testid="terminal-stop-children-btn"');
    expect(source).toContain('data-testid="terminal-pair-handoff-btn"');
    expect(source).toContain("buildPairHandoffPrompt(");
  });

  it("offers all four Workspace Split Presets and delegates isolated worktree provisioning to the dock host", () => {
    expect(source).toContain('data-testid="terminal-preset-agent-diff"');
    expect(source).toContain('data-testid="terminal-pair-split-preset"');
    expect(source).toContain('data-testid="terminal-preset-fullstack"');
    expect(source).toContain('data-testid="terminal-preset-worktree-agent"');
    expect(source).toContain("onCreateAgentWorktree");
    expect(source).not.toContain('"cmd_add_worktree"');
    expect(source).toContain("openCheckoutTab(");
  });

  it("renders the durable session ledger Reopen N agents banner with shell, corrupt, and per-row error handling (S2-03 & S2-05)", () => {
    expect(source).toContain("subscribeSessionLedger()");
    expect(source).toContain('data-testid="terminal-session-ledger-banner"');
    expect(source).toContain("reopenBannerHeading(repoReopenableOffers.length)");
    expect(source).toContain('data-testid="terminal-ledger-not-restorable"');
    expect(source).toContain('data-testid="terminal-ledger-unreadable"');
    expect(source).toContain('data-testid="terminal-ledger-row-error"');
    expect(source).toContain("reopenSessionLedgerOffer(");
    expect(source).toContain("reopenAllSessionLedgerOffers(");
    expect(source).toContain("dismissSessionLedgerRow(");
  });

  it("reports open dock tab counts to panelTabCounts and offers all sessionTypes in the empty state", () => {
    expect(source).toContain("panelTabCounts.report(");
    expect(source).toContain("panelTabCounts.forget(");
    const emptyStart = source.indexOf('{:else if tabState.tabs.length === 0}');
    const emptyEnd = source.indexOf('tabState.tabs as tab', emptyStart);
    expect(emptyStart).toBeGreaterThan(-1);
    expect(emptyEnd).toBeGreaterThan(emptyStart);
    const emptyState = source.slice(emptyStart, emptyEnd);
    expect(emptyState).toContain("{#each sessionTypes as launcher (launcher.kind)}");
    expect(emptyState).toContain("data-testid={`terminal-empty-launch-${launcher.kind}`}");
    expect(emptyState).toContain("canCreateKind(launcher.kind)");
    expect(emptyState).not.toContain("openBrowserFor(");
    expect(emptyState).toContain("newTab(launcher.kind)");
  });

  it("surfaces the active session summary, 3-tier split tab strip markers, constant-height split pane headers, and live session retitling", () => {
    expect(headerSource).toContain('data-testid="terminal-active-session-summary"');
    expect(source).toContain('data-testid="terminal-header-split-indicator"');
    expect(source).toContain('data-active={isActiveTab ? "true" : "false"}');
    expect(source).toContain('data-in-split={inVisibleSplit ? "true" : "false"}');
    expect(source).toContain('data-testid="terminal-tab-split-slot"');
    expect(source).toContain('data-testid="terminal-split-pane-header"');
    expect(source).toContain('data-testid="terminal-split-active-badge"');
    expect(source).toContain("removePaneFromSplit(");
    expect(source).toContain("zoomSplitPane(");
    expect(source).toContain("effectiveSessionTitle(tab, effectiveAgentStatuses[tab.id])");
    // Constant-height header inside split panes with tonal step only (never border-b hairline) and no destructive Close Browser aria-label collision.
    expect(source).toContain("h-7 shrink-0");
    expect(source).toContain("aria-label={`Remove ${tabLabel(tab)} from split`}");
  });

  it("wires zoomed hidden-sibling alerts, center swap drops, split-pane pinning, Open in split, container-query narrow headers, inline rename, and cross-surface slot markers", () => {
    // P0.2: zoomed hidden sibling attention alert + visibleIds unread clearing
    expect(source).toContain("summarizeHiddenSplitSiblings(");
    expect(source).toContain('data-testid="terminal-hidden-split-alert"');
    expect(source).toContain('data-testid="terminal-zoomed-hidden-alert"');
    expect(source).toContain("revealHiddenSplitUrgent()");

    // P0.3: durable ledger row title display
    expect(source).toContain('data-testid="terminal-ledger-row-title"');

    // P1.5: center swap drop zone + draggable split-pane header
    expect(source).toContain("swapPanes(splitRoot,");
    expect(source).toContain('data-testid="terminal-tab-drop-swap"');

    // P1.6: split-pane pinning + 1-click Open in split
    expect(source).toContain("chooseSplitReplacementSlot(");
    expect(source).toContain('data-testid="terminal-split-pin-btn"');
    expect(source).toContain('data-testid="terminal-tab-pin-toggle"');
    expect(source).toContain('data-testid="terminal-tab-open-in-split"');

    // P1.8: @container split-pane narrow header responsiveness + inline rename + Console active summary return
    expect(source).toContain("@container split-pane");
    expect(source).toContain('data-testid="terminal-inline-rename-input"');
    expect(headerSource).toContain("→ Shell");

    // P2.9: multi-agent pair handoff targets + Sessions across repositories Active & Split #slot badges
    expect(source).toContain("resolvePairHandoffTargets(");
    expect(source).toContain('data-testid="session-split-slot"');
    expect(source).toContain('data-testid="session-active-badge"');
    expect(source).toContain('data-testid="session-attention-badge"');

    // Native hook attention fallback (`$sessionActivity`), 8-pinned-pane activeId preservation,
    // non-child reclaimed title hydration without launchName pollution, and inline rename focus restore
    expect(source).toContain("activityToAgentStatus(");
    expect(source).toContain("activityAttentionLabel($sessionActivity");
    expect(source).toContain('choice?.mode === "blocked"');
    expect(source).toContain("all 8 split panes are pinned");
    expect(source).toContain("title: meta.parentSessionId ? (reclaimed.title ?? null) : null");
    expect(source).toContain("restoreFocusAfterInlineRename(");
  });

  it("supports multiple grouped terminal pairs with pair capsules, header switcher, pair presets, and non-destructive switching", () => {
    expect(source).toContain('data-testid="terminal-tab-group"');
    expect(source).toContain('data-testid="terminal-tab-group-badge"');
    expect(source).toContain('data-testid="terminal-tab-group-unsplit"');
    expect(source).toContain('data-testid="terminal-pair-switcher"');
    expect(source).toContain('data-testid="terminal-pair-switcher-btn"');
    expect(source).toContain('data-testid="terminal-group-rename-input"');
    expect(source).toContain('data-testid="terminal-preset-shell-pair"');
    expect(source).toContain('data-testid="terminal-preset-agent-shell-pair"');
    expect(source).toContain("upsertSplitGroup(");
    expect(source).toContain("coLocateTabsForGroups(");
    expect(source).toContain("summarizeSplitGroupAttention(");
  });

  it("docks a non-splitting File Explorer sidebar with user-controlled open targets, exposes File Explorer + icons in Session Typer, and supports predictable tab drag-and-drop + tab context menu", () => {
    // 1. Docked File Explorer sidebar (never forces a 3rd split pane on toggle) + user-controlled Tab | Split | Code target
    expect(source).toContain('data-testid="terminal-explorer-sidebar-toggle"');
    expect(source).toContain('data-testid="terminal-explorer-sidebar"');
    expect(source).toContain('data-testid="terminal-explorer-target-tab"');
    expect(source).toContain('data-testid="terminal-explorer-target-split"');
    expect(source).toContain('data-testid="terminal-explorer-target-code"');
    expect(source).toContain("resolveExplorerFileAction(");
    expect(source).not.toContain("function toggleFilePaneSplit()");

    // 2. Session Typer includes File Explorer via dockTabTypesFor + renders LauncherIcon on trigger and presets
    expect(source).toContain("dockTabTypesFor(");
    expect(source).toContain("offeredDockTabKind(");
    expect(source).toContain('data-testid="terminal-launcher-toggle-explorer"');
    expect(source).toContain('data-testid="terminal-empty-launch-file"');
    const launcherTrigger = source.slice(
      source.indexOf("data-terminal-launcher"),
      source.indexOf("data-terminal-new"),
    );
    expect(launcherTrigger).toContain("<LauncherIcon kind={nextLauncher}");

    // 3. Predictable tab-strip drag-and-drop (before/after drop bars, detach dropzone, group reorder) + right-click context menu
    expect(source).toContain("classifyTabStripDropZone(");
    expect(source).toContain("applyTabStripDrop(");
    expect(source).toContain("detachTabToStripEnd(");
    expect(source).toContain("reorderSplitGroupInStrip(");
    expect(source).toContain("moveSplitGroupToStripEnd(");
    expect(source).toContain('data-testid="terminal-tab-drop-before"');
    expect(source).toContain('data-testid="terminal-tab-drop-after"');
    expect(source).toContain('data-testid="terminal-tab-detach-dropzone"');
    expect(source).toContain('data-testid="terminal-tab-split-dropzone"');
    expect(source).toContain('data-testid="terminal-tab-context-menu"');
    expect(source).toContain("dockFileTabToSidebar(");
    expect(source).toContain('"Switch to tab"');
    expect(source).toContain('"Split left"');
    expect(source).toContain('"Split right"');
    expect(source).toContain('aria-hidden="true" data-testid="terminal-tab-split-slot"');
    expect(source).not.toContain("if (!meta.filePath) continue;");

    // 4. Clicking + (openBeside) opens a standalone Browser tab via newTab rather than forcing a 2-pane split
    const openBesideBody = source.slice(
      source.indexOf("async function openBeside("),
      source.indexOf("async function closeListed("),
    );
    expect(openBesideBody).not.toContain("openBrowserFor(");
    expect(openBesideBody).toContain("newTab(launcher)");

    // 5. Cold-boot file split preserves an anchor shell, folder drops on file panes forward to session, and ledger rows suppress generic startup titles
    const initialTabsBody = source.slice(
      source.indexOf("function initialTabs(): TabState"),
      source.indexOf("let tabState = $state<TabState>"),
    );
    expect(initialTabsBody).toContain('launch.launcher === "file"');
    expect(initialTabsBody).toContain('launch.fileOpenMode !== "tab"');
    const fileDropBody = source.slice(
      source.indexOf("function handleFileDropOnPane("),
      source.indexOf("function canEdgeSplitDrop("),
    );
    expect(fileDropBody).toContain("filesOnly");
    const ledgerRowBlock = source.slice(
      source.indexOf('data-testid="terminal-ledger-row"'),
      source.indexOf('data-testid="terminal-ledger-reopen-btn"'),
    );
    expect(ledgerRowBlock).toContain("isGenericLauncherTitle(");
  });

  it("exposes right-click context menus for tabs, split panes, pair badges, and split toolbar controls", () => {
    // 1. Portaled tab, split toolbar, and pair group menus exist with keyboard/accessibility triggers
    expect(source).toContain('data-testid="terminal-tab-context-menu"');
    expect(source).toContain('data-testid="terminal-split-context-menu"');
    expect(source).toContain('data-testid="terminal-group-context-menu"');
    expect(source).toContain("resolveContextMenuPoint(");
    expect(source).toContain('event.key === "ContextMenu"');

    // 2. Split toolbar context menu supports explicit placement (right/left/down/up), session creation, and pairing
    expect(source).toContain('data-testid="terminal-split-ctx-place-right"');
    expect(source).toContain('data-testid="terminal-split-ctx-place-left"');
    expect(source).toContain('data-testid="terminal-split-ctx-place-down"');
    expect(source).toContain('data-testid="terminal-split-ctx-place-up"');
    expect(source).toContain("splitPaneWithNewKind(");
    expect(source).toContain('data-testid="terminal-split-ctx-pair-tab"');

    // 3. Layout & pair controls wire into flipSplitOrientation, swapWithAdjacentSplitPane, moveSplitGroupByStep, equalize, and ungroup
    expect(source).toContain("flipSplitOrientation(");
    expect(source).toContain("swapWithAdjacentSplitPane(");
    expect(source).toContain("moveSplitGroupByStep(");
    expect(source).toContain("equalizeGroupSplits(");
    expect(source).toContain("ungroupAllPairs(");

    // 4. Upgraded tab context menu exposes directional split controls, new-session splits, and bulk close actions
    expect(source).toContain('data-testid="terminal-tab-ctx-split-left"');
    expect(source).toContain('data-testid="terminal-tab-ctx-split-up"');
    expect(source).toContain('data-testid="terminal-tab-ctx-split-new-right"');
    expect(source).toContain("closeOtherTabs(");
    expect(source).toContain("closeTabsToRight(");
    expect(source).toContain("closeEndedTabs(");
  });

  it("wires rich output copy actions, Select-to-Copy toggle, initialCommand, and run-in-new-split-shell handoff", () => {
    expect(source).toContain("setTabInitialCommand(");
    expect(source).toContain("async function runCommandInNewSplitShell(");
    expect(source).toContain('value="copy-md"');
    expect(source).toContain('value="last-cmd"');
    expect(source).toContain('value="run-last-new-shell"');
    expect(source).toContain('data-testid="terminal-tab-opt-copy-on-select"');
    expect(source).toContain("initialCommand={tab.initialCommand}");
    expect(source).toContain("onRunInNewShell={(cmd, sub) => void runCommandInNewSplitShell(tab.id, cmd, sub)}");
  });
});
