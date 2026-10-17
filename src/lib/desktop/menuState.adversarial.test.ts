import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { afterEach, describe, expect, it } from "vitest";
import { blockedWhilePrompting } from "./menuState";
import { askConfirm, askText, cancelPrompt, isPromptOpen } from "../stores/modalStore";

const HERE = dirname(fileURLToPath(import.meta.url));
const NATIVE_ACTIONS = readFileSync(join(HERE, "nativeActions.ts"), "utf8");

/** Native menu ids paired with the handler they run, read from dispatchNativeMenu. */
function idsByHandler(): Map<string, string[]> {
  const out = new Map<string, string[]>();
  for (const m of NATIVE_ACTIONS.matchAll(/case "([\w-]+)":\s*(?:if \([^)]*\)(?:[^;]*;)?\s*)?handlers\.(\w+)\(/g)) {
    out.set(m[2], [...(out.get(m[2]) ?? []), m[1]]);
  }
  return out;
}

describe("attack 4: native menu ids while a prompt is open", () => {
  afterEach(() => cancelPrompt());

  it("HOLDS: every id whose handler switches, opens or closes a tab is held", () => {
    const handlers = idsByHandler();
    const tabHandlers = ["activateRepo", "openRecent", "openRepo", "closeRepoTab", "nextRepoTab", "prevRepoTab", "reopenRepoTab", "open"];
    const missing = tabHandlers.flatMap((h) => {
      const ids = handlers.get(h);
      return ids ? ids.filter((id) => !blockedWhilePrompting(id)).map((id) => `${id} -> ${h}`) : [`${h} not found in nativeActions.ts`];
    });
    expect(missing).toEqual([]);
  });

  it("HOLDS: read-only and view-only ids are not held (copy, zoom, theme, refresh, section navigation)", () => {
    for (const id of ["copy-repo-path", "zoom-in", "theme-dark", "refresh", "section:history:graph", "palette", "shortcuts"]) {
      expect(blockedWhilePrompting(id)).toBe(false);
    }
  });

  it("RULED: clone is held, because a finished clone opens and activates a new tab under the open prompt", () => {
    expect(blockedWhilePrompting("clone")).toBe(true);
  });

  it("RULED: prompt-starting ids are held, so a second prompt never silently cancels the first", async () => {
    for (const id of ["create-branch", "rename-branch", "quick-commit", "stash", "stash-pop", "operation-abort", "operation-skip", "operation-continue"]) {
      expect(blockedWhilePrompting(id)).toBe(true);
    }
    // The store itself still retires an older prompt when asked directly; the
    // dispatcher is what keeps the menu from asking.
    const first = askConfirm({ title: "Delete branch", message: "m" } as never);
    void askText({ title: "Create Branch", message: "m" } as never);
    expect(await first).toBe(false);
    expect(isPromptOpen()).toBe(true);
  });
});
