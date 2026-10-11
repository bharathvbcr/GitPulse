import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import TerminalPanel from "./TerminalPanel.svelte";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "TerminalPanel.svelte"),
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
    expect(source).not.toContain("session.repoPath.split(");
    expect(source).not.toMatch(/session\.repoPath\s*[!=]==\s*repoPath/);
    expect(source).toContain('data-testid="session-agent"');
  });

  it("links a resumed conversation's session back to its task, as it does an attempt's", () => {
    expect(source).toContain("{#if row.taskRunId}");
    expect(source).toContain("openListedTask(row.taskRunId)");
  });

  it("refits the visible session when its host is shown again", () => {
    const body = source.slice(source.indexOf("$effect(() => {", source.indexOf("function handleChord")));
    expect(body).toContain("if (!visible || mode !== \"shell\") return");
    expect(body).toContain("session?.reveal()");
    expect(body).toContain('if (!visible || mode !== "shell" || activeId !== id) return;');
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

describe("TerminalPanel rendering", () => {
  it("renders the header with mode switcher", () => {
    const { body } = render(TerminalPanel);
    expect(body).toContain("Terminal");
    expect(body).toContain("Shell");
    expect(body).toContain("Console");
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
