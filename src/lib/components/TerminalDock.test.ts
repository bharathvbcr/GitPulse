import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import TerminalDock from "./TerminalDock.svelte";

const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, "TerminalDock.svelte"), "utf8");
const app = readFileSync(join(here, "..", "..", "App.svelte"), "utf8");

const loader = () => Promise.resolve({ default: (() => {}) as never });

describe("TerminalDock", () => {
  it("mounts nothing until it is first opened", () => {
    // The dock carries a PTY and the 334 KB xterm runtime. A user who never
    // opens it must never pay for it.
    const body = render(TerminalDock, {
      props: { open: false, onClose: () => {}, load: loader },
    }).body;
    expect(body).not.toContain("data-terminal-dock");
  });

  it("renders the dock once open", () => {
    const body = render(TerminalDock, {
      props: { open: true, onClose: () => {}, load: loader },
    }).body;
    expect(body).toContain("data-terminal-dock");
    expect(body).toContain("Terminal");
  });

  it("hides rather than unmounts, because closing must not kill the shell", () => {
    // The whole reason the terminal could never really be a view: unmounting
    // the pane ends the process. `mounted` latches true and only the
    // repository switch above it tears the session down.
    expect(source).toContain("let mounted = $state(open)");
    expect(source).toContain("if (open) mounted = true;");
    expect(source).toContain("class:hidden={!open}");
  });

  it("offers the WAI-ARIA splitter, keyboard included", () => {
    expect(source).toContain('role="separator"');
    expect(source).toContain('aria-valuenow={height}');
    expect(source).toContain('event.key === "ArrowUp"');
    expect(source).toContain('event.key === "ArrowDown"');
  });

  it("sizes itself through the clamp rather than trusting the stored height", () => {
    expect(source).toContain("fitTerminalDockHeight($interfaceStore.terminalDockHeight");
  });
});

describe("App hosts the terminal as a dock, not a view", () => {
  it("renders the dock inside the view column", () => {
    expect(app).toContain("<TerminalDock");
    expect(app).toContain("open={terminalDockOpen}");
  });

  it("no longer swaps the main pane out for a terminal", () => {
    // The old shape hid <main> whenever the terminal tab was active, so the
    // shell replaced whatever you were reading.
    expect(app).not.toContain('activeTab === "terminal"');
    expect(app).not.toContain("class:hidden={terminalActive}");
    expect(app).not.toContain("terminalMounted");
  });

  it("binds the chord every terminal-hosting editor uses", () => {
    // Control, not Command, on macOS too: ⌘` is the OS window cycler.
    expect(app).toContain('e.key === "`"');
    expect(app).toContain("interfaceStore.toggleTerminalDock()");
  });
});
