import { get } from "svelte/store";
import { beforeEach, describe, expect, it } from "vitest";
import { AGENT_MESSAGE_MAX_BYTES } from "../workbench/sendToAgent";
import {
  MAX_TRAY_ENTRIES,
  MAX_TRAY_PATH,
  addToTray,
  clearTray,
  contextTrays,
  trayBytes,
  trayFor,
  trayProblem,
  trayText,
  type TrayEntry,
} from "./contextTray";

const repo = "/work/repo";
const entry = (path: string, startLine: number, endLine = startLine, checkout = repo): TrayEntry => ({ checkout, path, startLine, endLine });

beforeEach(() => contextTrays.set({}));

describe("addToTray", () => {
  it("adds ranges in order, per repository", () => {
    expect(addToTray(repo, entry("a.ts", 1, 3))).toEqual({ ok: true, added: true });
    expect(addToTray(repo, entry("b.ts", 7))).toEqual({ ok: true, added: true });
    expect(addToTray("/other", entry("c.ts", 2))).toEqual({ ok: true, added: true });
    expect(trayFor(repo).map((e) => e.path)).toEqual(["a.ts", "b.ts"]);
    expect(trayFor("/other")).toHaveLength(1);
  });

  it("does not add an identical range twice, and says so without failing", () => {
    addToTray(repo, entry("a.ts", 1, 3));
    expect(addToTray(repo, entry("a.ts", 1, 3))).toEqual({ ok: true, added: false });
    expect(addToTray(repo, entry("a.ts", 3, 1))).toEqual({ ok: true, added: false });
    expect(trayFor(repo)).toHaveLength(1);
  });

  it("keeps the same range in another checkout, since it is another file's text", () => {
    addToTray(repo, entry("a.ts", 1));
    expect(addToTray(repo, entry("a.ts", 1, 1, "/work/repo-wt/x"))).toEqual({ ok: true, added: true });
  });

  it("refuses at the cap with a reason and evicts nothing", () => {
    for (let i = 1; i <= MAX_TRAY_ENTRIES; i += 1) addToTray(repo, entry("a.ts", i));
    const refused = addToTray(repo, entry("a.ts", 999));
    expect(refused.ok).toBe(false);
    expect(refused.ok === false && refused.reason).toContain(String(MAX_TRAY_ENTRIES));
    expect(trayFor(repo)).toHaveLength(MAX_TRAY_ENTRIES);
    expect(trayFor(repo)[0].startLine).toBe(1);
  });

  it("refuses what is not a range: no repository, no path, an oversized path, a line below 1 or not whole", () => {
    expect(addToTray("", entry("a.ts", 1)).ok).toBe(false);
    expect(addToTray(repo, entry("", 1)).ok).toBe(false);
    expect(addToTray(repo, entry("p".repeat(MAX_TRAY_PATH + 1), 1)).ok).toBe(false);
    expect(addToTray(repo, entry("a.ts", 0)).ok).toBe(false);
    expect(addToTray(repo, entry("a.ts", 1.5)).ok).toBe(false);
    expect(addToTray(repo, entry("a.ts", Number.NaN)).ok).toBe(false);
    expect(trayFor(repo)).toHaveLength(0);
  });
});

describe("clearTray and what Insert all types", () => {
  it("clears one repository's tray and leaves the others", () => {
    addToTray(repo, entry("a.ts", 1));
    addToTray("/other", entry("b.ts", 1));
    clearTray(repo);
    expect(trayFor(repo)).toHaveLength(0);
    expect(trayFor("/other")).toHaveLength(1);
    expect(Object.keys(get(contextTrays))).toEqual(["/other"]);
  });

  it("joins references with spaces and ends with one, on a single line", () => {
    expect(trayText([entry("a.ts", 1, 3), entry("b.ts", 7)])).toBe("@a.ts#L1-L3 @b.ts#L7 ");
    expect(trayText([])).toBe("");
  });

  it("shows a control character in a path instead of letting it break the line", () => {
    expect(trayText([entry("a\nb.ts", 1)])).toBe("@a\\nb.ts#L1 ");
  });

  it("reports the size in bytes of UTF-8, not characters", () => {
    expect(trayBytes([entry("a.ts", 1)])).toBe("@a.ts#L1 ".length);
    expect(trayBytes([entry("é.ts", 1)])).toBe("@é.ts#L1 ".length + 1);
  });

  it("refuses to insert an empty tray or one past the message bound, with the size", () => {
    expect(trayProblem([])).toBe("The tray is empty.");
    expect(trayProblem([entry("a.ts", 1)])).toBeNull();
    const big = Array.from({ length: 20 }, (_, i) => entry("d/".repeat(MAX_TRAY_PATH / 4) + `f${i}.ts`, 1));
    expect(trayBytes(big)).toBeGreaterThan(AGENT_MESSAGE_MAX_BYTES);
    expect(trayProblem(big)).toContain(String(trayBytes(big)));
  });
});
