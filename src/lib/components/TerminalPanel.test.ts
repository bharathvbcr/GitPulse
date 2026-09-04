import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import TerminalPanel, { planAttach, shouldRefit } from "./TerminalPanel.svelte";

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

  it("invokes PTY session endpoints for interactive shell", () => {
    expect(source).toContain('"cmd_terminal_spawn"');
    expect(source).toContain('"cmd_terminal_write"');
    expect(source).toContain('"cmd_terminal_resize"');
    expect(source).toContain('"cmd_terminal_kill"');
  });

  it("tokenizes commands and checks for safety before execution in Console mode", () => {
    expect(source).toContain("tokenizeCommand(textToRun)");
    expect(source).toContain("validationError = tokenized.error");
  });

  it("journals executed terminal actions into harnessStore", () => {
    expect(source).toContain("harnessStore.recordAction({");
  });

  it("attributes every terminal journal row to the repository that started it", () => {
    const journalCalls = source.match(/harnessStore\.recordAction\(\{\s+repoPath,/g) ?? [];
    expect(journalCalls).toHaveLength(3);
  });

  it("provides copy and clear output affordances in Console mode", () => {
    expect(source).toContain("clearOutput");
    expect(source).toContain("copyOutput");
  });
});

describe("TerminalPanel PTY lifecycle memo guard", () => {
  it("tears down the PTY only on a real mode/path change, never per store emission", () => {
    // repoStore republishes fresh objects every ~6s status poll; the kill +
    // respawn below must sit behind a lifecycle-key comparison, not re-run
    // per emission.
    const guardIdx = source.indexOf("if (key === ptyLifecycleKey) return;");
    expect(guardIdx).toBeGreaterThan(-1);
    const killIdx = source.indexOf("void killPty(liveCleanupTarget)");
    const spawnIdx = source.indexOf("void spawnPty(path)");
    expect(killIdx).toBeGreaterThan(guardIdx);
    expect(spawnIdx).toBeGreaterThan(killIdx);
    // The key derives only from the PTY's real inputs: shell mode + repo path.
    expect(source).toContain('`shell:${path}`');
  });

  it("kills a backend session whose spawn landed after its owner went away", () => {
    // Cleanup can run while cmd_terminal_spawn is pending; without this
    // check the late response adopts an orphaned session.
    const supersedeIdx = source.indexOf("if (epoch !== spawnEpoch)");
    expect(supersedeIdx).toBeGreaterThan(-1);
    expect(source.indexOf("void killPty(spawned.id);")).toBeGreaterThan(supersedeIdx);
  });

  it("invalidates pending spawns on teardown and unmount", () => {
    // One bump in the guarded effect's real-change branch, one in the
    // onMount cleanup, plus the ++spawnEpoch capture in spawnPty itself.
    expect(source.match(/spawnEpoch \+= 1;/g)?.length).toBe(2);
    expect(source).toContain("const epoch = ++spawnEpoch;");
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

describe("TerminalPanel refit guard", () => {
  const dims = (cols: number, rows: number) => ({ cols, rows });

  it("skips the fit when the proposed grid equals the live one", () => {
    expect(shouldRefit(dims(80, 24), dims(80, 24))).toBe(false);
  });

  it("refits when cols or rows actually change", () => {
    expect(shouldRefit(dims(80, 24), dims(81, 24))).toBe(true);
    expect(shouldRefit(dims(80, 24), dims(80, 25))).toBe(true);
  });

  it("skips on an unusable proposal instead of guessing a grid", () => {
    // Hidden panels and mid-layout containers make proposeDimensions()
    // return undefined; fitting then would throw or no-op with churn.
    expect(shouldRefit(dims(80, 24), null)).toBe(false);
    expect(shouldRefit(dims(80, 24), undefined)).toBe(false);
    expect(shouldRefit(dims(80, 24), dims(Number.NaN, 24))).toBe(false);
    expect(shouldRefit(dims(80, 24), dims(80, Number.POSITIVE_INFINITY))).toBe(false);
  });

  it("refits once when there is no baseline yet but the proposal is usable", () => {
    expect(shouldRefit(null, dims(80, 24))).toBe(true);
    expect(shouldRefit(null, undefined)).toBe(false);
  });

  it("treats sub-pixel float noise as unchanged", () => {
    expect(shouldRefit(dims(80, 24), dims(80.4, 23.6))).toBe(false);
    expect(shouldRefit(dims(80, 24), dims(80.6, 24.4))).toBe(true);
  });
});

describe("TerminalPanel container re-attach plan", () => {
  it("opens on first attach", () => {
    const container = { } as Element;
    expect(planAttach(null, container)).toBe("open");
    expect(planAttach(undefined, container)).toBe("open");
  });

  it("skips when the terminal already lives in this container", () => {
    const container = { } as Element;
    expect(planAttach(container, container)).toBe("skip");
  });

  it("adopts (re-parents) when the container node was swapped", () => {
    // Toggling Shell↔Console unmounts the old pty div forever; xterm's
    // open() early-returns on an opened terminal, so only a physical move
    // of term.element keeps the buffer visible and keystrokes alive.
    const staleDetached = { } as Element;
    const fresh = { } as Element;
    expect(planAttach(staleDetached, fresh)).toBe("adopt");
  });
});

describe("TerminalPanel flicker-hardening contracts", () => {
  it("constructs exactly one XTerm instance for the panel lifetime", () => {
    expect(source.match(/new XTerm\(/g)?.length).toBe(1);
  });

  it("decouples creation from attachment: ensureTerm never opens a container", () => {
    const ensureBody = source.slice(
      source.indexOf("function ensureTerm"),
      source.indexOf("function refitIfResized"),
    );
    expect(ensureBody).not.toContain(".open(");
  });

  it("re-parents through an effect keyed on the bound container", () => {
    const attachIdx = source.indexOf("const container = ptyContainer;");
    expect(attachIdx).toBeGreaterThan(-1);
    const planIdx = source.indexOf("planAttach(", attachIdx);
    const adoptIdx = source.indexOf("replaceChildren(", attachIdx);
    const openIdx = source.indexOf(".open(container)", attachIdx);
    expect(planIdx).toBeGreaterThan(attachIdx);
    expect(openIdx).toBeGreaterThan(planIdx);
    expect(adoptIdx).toBeGreaterThan(openIdx);
    // The observer must follow whichever container is current.
    expect(source.indexOf("resizeObserver.observe(container)", attachIdx)).toBeGreaterThan(adoptIdx);
  });

  it("guards ResizeObserver refits with proposeDimensions + shouldRefit", () => {
    const refitBody = source.slice(
      source.indexOf("function refitIfResized"),
      source.indexOf("function writePty"),
    );
    expect(refitBody).toContain("proposeDimensions()");
    expect(refitBody).toContain("shouldRefit(");
    expect(refitBody).toContain("fitAddon.fit()");
  });

  it("keeps the resize IPC wired to real dimension changes via onResize", () => {
    const onResizeBody = source.slice(
      source.indexOf("created.onResize("),
      source.indexOf("term = created;"),
    );
    expect(onResizeBody).toContain('"cmd_terminal_resize"');
  });

  it("applies the theme palette reactively from themeStore", () => {
    expect(source).toContain('import { themeStore } from "../stores/themeStore";');
    const effectIdx = source.indexOf("$themeStore;");
    expect(effectIdx).toBeGreaterThan(-1);
    expect(source.indexOf("term.options.theme = termTheme()", effectIdx)).toBeGreaterThan(effectIdx);
    // Construction-time palette stays too: a term created between emissions
    // still gets the current CSS-variable palette immediately.
    expect(source.slice(0, effectIdx)).toContain("theme: termTheme()");
  });

  it("reserves one fixed-height status row instead of swapping py-2/py-1.5 strips", () => {
    expect(source).not.toContain("py-2 border-t");
    expect(source).not.toContain("py-1.5 border-t");
    expect(source).toContain("px-4 h-8");
  });

  it("has a single mount that focuses the command input", () => {
    expect(source.match(/onMount\(/g)?.length).toBe(1);
    expect(source).toContain("inputEl?.focus()");
  });
});

describe("TerminalPanel listener lifecycle hygiene", () => {
  const cleanupStart = () => source.indexOf("return () => {", source.indexOf("onMount("));

  it("routes resolved unlisten fns through the tracker, not a dead array", () => {
    // The old pattern pushed into an array the cleanup had already drained:
    // a listen() resolving after teardown leaked the listener for the
    // webview lifetime.
    expect(source).toContain("createListenerTracker()");
    expect(source).toContain("unlisteners.track(fn)");
    expect(source).not.toContain("unlisteners.push(...");
    expect(source).not.toContain("unlisteners.splice(0)");
    expect(source).not.toContain("UnlistenFn[]");
  });

  it("disposes the tracker as the first act of cleanup", () => {
    const start = cleanupStart();
    expect(start).toBeGreaterThan(-1);
    expect(source.indexOf("unlisteners.dispose()", start)).toBeGreaterThan(start);
  });

  it("cancels the pending copy-feedback timer on teardown", () => {
    // The handle is captured so a fast unmount cannot fire the reset into a
    // dead component; rapid copies replace the timer instead of stacking.
    expect(source).toContain("let copiedResetTimer: ReturnType<typeof setTimeout> | null = null;");
    // Two clears total: one in copyOutput (replacing a pending reset), one in cleanup.
    expect(source.match(/clearTimeout\(copiedResetTimer\)/g)?.length).toBe(2);
    const start = cleanupStart();
    expect(source.indexOf("clearTimeout(copiedResetTimer)", start)).toBeGreaterThan(start);
  });
});
