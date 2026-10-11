import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { compile } from "svelte/compiler";

/**
 * What the task sheet does is checked by rendering it: `harness/tasksChecks.js`
 * (`node scripts/browser-regressions.mjs --harness tasks`) opens the real
 * sheet over the real board and drives its tabs, repository picker, due
 * picker, logs, subtasks, assist, home workspace, save, discard and delete in
 * Chrome. Only what the rendered page cannot show stays here.
 */
const source = readFileSync(new URL("./TaskEditor.svelte", import.meta.url), "utf8");

describe("TaskEditor source contracts", () => {
  it("compiles", () => {
    const { warnings } = compile(source, { generate: "client", filename: "TaskEditor.svelte" });
    expect(warnings.filter((w) => w.code !== "css-unused-selector")).toEqual([]);
  });

  it("asks through the in-app dialog, never the native confirm", () => {
    expect(source).not.toContain("window.confirm");
  });

  it("never spells an engine name itself, so a task drafted on-device cannot be refused by a message naming Manvi", () => {
    // The assist section owns the engine picker and reports its choice through
    // `onEngine`. A hand-written name appears only on screen while a
    // suggestion is running, which a render check would have to time.
    expect(
      [...source.matchAll(/\bManvi\b/g)].map((match) => source.slice(Math.max(0, match.index - 40), match.index + 40)),
      "the sheet must not name an engine; read it from onEngine",
    ).toEqual([]);
  });

  it("keeps the assist outside the fieldset a running suggestion disables", () => {
    // `enhancementBusy` greys the fields a suggestion may rewrite. The assist
    // owns that suggestion's Cancel button, so putting it inside would let a
    // running generation disable the only control that can stop it.
    const assistAt = source.indexOf("<TaskManviAssist");
    expect(assistAt).toBeGreaterThan(0);
    const before = source.slice(0, assistAt);
    const lastOpen = before.lastIndexOf("disabled={enhancementBusy}");
    const lastClose = before.lastIndexOf("</fieldset>");
    expect(lastOpen, "no enhancement-busy fieldset precedes the assist").toBeGreaterThan(0);
    expect(lastClose, "the enhancement-busy fieldset still encloses the assist").toBeGreaterThan(lastOpen);
  });
});
