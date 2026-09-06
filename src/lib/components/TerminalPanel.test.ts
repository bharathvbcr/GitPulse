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
  it("invokes cmd_terminal_run with repoPath and args", () => {
    expect(source).toContain('invoke<TerminalRunResponse>("cmd_terminal_run"');
    expect(source).toContain("repoPath,");
    expect(source).toContain("args: tokenized.argv,");
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

  it("tokenizes commands and checks for safety before execution in Console mode", () => {
    expect(source).toContain("tokenizeCommand(textToRun)");
    expect(source).toContain("validationError = tokenized.error");
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

  it("provides copy and clear output affordances in Console mode", () => {
    expect(source).toContain("clearOutput");
    expect(source).toContain("copyOutput");
  });
});

describe("TerminalPanel session ownership", () => {
  it("holds no PTY lifecycle of its own", () => {
    // The memo guard this replaces existed because an effect reading
    // $repoStore re-ran on every ~6s status poll and would have killed the
    // user's live shell per emission. There is no such effect now: a session
    // lives and dies with its own component, and the repository boundary is
    // App's `{#key $repoStore.currentPath}` around the dock.
    expect(source).not.toContain("ptyLifecycleKey");
    expect(source).not.toContain("spawnEpoch");
    expect(source).not.toContain("liveCleanupTarget");
  });

  it("keys each session on its tab id so a close cannot recycle a live shell", () => {
    // An unkeyed each would reuse the component of a closed tab for its
    // neighbour, attaching a live PTY to the wrong tab instead of ending it.
    expect(source).toContain("{#each tabState.tabs as tab (tab.id)}");
  });

  it("keeps sessions mounted while the Console tab is showing", () => {
    // Unmounting disposes the xterm and kills the shell; the mode switch
    // hides the strip instead, the same rule TerminalDock applies to the
    // whole dock. An {:else} against the console branch would kill them.
    const sessionIdx = source.indexOf("<TerminalSession");
    expect(sessionIdx).toBeGreaterThan(-1);
    expect(source).toContain('class:hidden={mode !== "shell"}');
    expect(source.indexOf('{#if mode === "console"}')).toBeGreaterThan(sessionIdx);
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
  it("offers a new tab per launcher instead of restarting the one session", () => {
    // The old pills called selectLauncher, which killed the running shell to
    // start the chosen CLI in its place — switching cost you your session.
    expect(source).not.toContain("selectLauncher");
    expect(source).toContain("{#each LAUNCHERS as launcher (launcher.kind)}");
    expect(source).toContain("onclick={() => newTab(launcher.kind)}");
  });

  it("disables opening past the ceiling and says why", () => {
    expect(source).toContain("disabled={!canOpenTab(tabState)}");
    expect(source).toContain("${MAX_TERMINAL_TABS} terminal sessions are open");
  });

  it("routes chords through the shared parser rather than inline key tests", () => {
    expect(source).toContain("terminalTabChord(event)");
    expect(source).toContain("onChord={handleChord}");
  });

  it("ignores chords while the Console tab is showing", () => {
    // Console has no tabs; claiming ⌃⇧W there would swallow a keystroke that
    // belongs to the command input.
    const body = source.slice(source.indexOf("function handleChord"));
    expect(body.indexOf('if (mode !== "shell") return false;')).toBeLessThan(
      body.indexOf("terminalTabChord(event)"),
    );
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

/**
 * Regression: the amber strip asserted a cause. `[Output exceeded cap]` was
 * printed for every prefix — including a stream the engine never finished
 * reading, whose byte count was often zero. The backend now says which, and
 * the panel renders that instead of guessing.
 */
describe("TerminalPanel truncation disclosure", () => {
  it("renders the backend's reason rather than naming a cap", () => {
    expect(source).toContain("entry.result.truncation_reason");
    expect(source).not.toContain("Output exceeded cap");
  });

  it("still discloses the prefix when the reason is missing", () => {
    // A prefix with no explanation is still a prefix; silence would let it
    // read as the whole output.
    expect(source).toContain('"reason unavailable"');
  });
});
