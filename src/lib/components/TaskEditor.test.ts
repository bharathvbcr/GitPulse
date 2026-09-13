import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { compile } from "svelte/compiler";

const source = readFileSync(new URL("./TaskEditor.svelte", import.meta.url), "utf8");

describe("TaskEditor", () => {
  it("compiles", () => {
    const { warnings } = compile(source, { generate: "client", filename: "TaskEditor.svelte" });
    expect(warnings.filter((w) => w.code !== "css-unused-selector")).toEqual([]);
  });

  it("exposes data-task-revision for harness save waits", () => {
    expect(source).toContain("data-task-revision={current?.revision");
  });

  it("keeps Save task in unpainted sheet chrome so glass cannot composite a black bar", () => {
    const footer = source.slice(source.indexOf("<style>")).match(/(?:^|})\s*footer\{([^}]*)\}/)?.[1];
    expect(footer).toBeDefined();
    expect(footer).toContain("flex-shrink:0");
    expect(footer).not.toContain("position:sticky");
    expect(footer).not.toMatch(/background(?:-color)?:/);
    expect(source).toContain('form="task-editor-form-{id}"');
    expect(source).toMatch(/<TaskAgentPanel[\s\S]*?<\/div>\s*\{\/if\}\s*<\/div>\s*<footer>/);
  });

  it("guards dispose, reload confirm, and enhancement-busy shortcuts", () => {
    expect(source).toContain("if (disposed) return");
    expect(source).toMatch(/confirming = true[\s\S]*askConfirm\(\{title: "Reload saved task\?/);
    expect(source).toContain("shortcutBlocked = `Wait for ${assistName} to finish before saving.`");
    expect(source).toContain("shortcutBlocked = `Wait for ${assistName} to finish before closing.`");
    // The sheet never spells an engine name itself: the assist section owns the
    // picker, so a task drafted on-device cannot be refused by a message that
    // names Manvi. `assistName` is the only spelling, and it is seeded once.
    expect(source).toContain('onEngine={(name) => { assistName = name; }}');
    expect(
      [...source.matchAll(/\bManvi\b/g)].map((match) => source.slice(Math.max(0, match.index - 40), match.index + 40)),
      "the sheet must not name an engine; read it from onEngine",
    ).toEqual([]);
    expect(source).toContain('role="status"');
  });

  it("uses typed controls for kind, severity, labels, and criteria bounds", () => {
    expect(source).toContain("KIND_OPTIONS");
    expect(source).toContain("Custom…");
    expect(source).toContain("SEVERITY_OPTIONS");
    expect(source).toContain("LabelInput");
    expect(source).toContain('maxlength="65536"');
    expect(source).toContain("Notifications (profile-wide; mute this task)");
    expect(source).toContain("showOrganize = $state(untrack(() => Boolean(initial.seed)))");
    expect(source).not.toContain("TaskEnhancements");
    expect(source).toContain("TaskManviAssist");
  });

  it("gives a draft one pane and no tab strip, and a saved task four", () => {
    // The rule lives in `taskEditorTabs`; what this guards is that the sheet
    // actually asks. A strip rendered unconditionally would put Agent and AI
    // in front of someone creating a task, where both are empty by definition.
    expect(source).toContain("const tabs = $derived(editorTabs(Boolean(current)))");
    expect(source).toContain("const tab = $derived(resolveEditorTab(requestedTab, Boolean(current)))");
    expect(source).toMatch(/\{#if tabs\.length > 1\}[\s\S]*?role="tablist"/);
    // Panes and tabs are addressed through the shared tablist owner, so their
    // ids match by construction rather than by two hand-written templates.
    expect(source).toContain('tabProps(group, entry.id, tab === entry.id)');
    for (const pane of ["task", "organize", "agent", "ai"]) {
      expect(source).toContain(`panelId(group, "${pane}")`);
      expect(source).toContain(`tabId(group, "${pane}")`);
    }
    // Tailwind's preflight `[hidden]` rule has zero specificity, so a plain
    // `.pane{display:flex}` here would leave every pane on screen at once.
    expect(source).toContain(".pane[hidden]{display:none}");
  });

  it("keeps the title, primary repository and linked repositories on the first pane", () => {
    // Everything a task needs to exist stays in one place. The panes split
    // what is optional, never what a save requires.
    const taskPane = source.slice(source.indexOf('panelId(group, "task")'), source.indexOf('panelId(group, "organize")'));
    expect(taskPane).toContain('name="task-title"');
    expect(taskPane).toContain("Primary repository");
    expect(taskPane).toContain("Linked repositories");
    expect(taskPane).toContain("Acceptance criteria");
    expect(source).not.toContain("More details");
    expect(source).not.toContain("showDetails");
  });

  it("draws Manvi's suggestion beside the field it would change", () => {
    expect(source).toContain('assist?.acceptFields(["title"])');
    expect(source).toContain('assist?.acceptFields(["description"])');
    expect(source).toContain('assist?.acceptFields(["title", "description"])');
    expect(source).toContain("assist?.hideSuggestion()");
    // The assist stays mounted whichever pane is open: it owns the polling and
    // the accept/undo history, and unmounting it would abandon a live request.
    expect(source).toMatch(/hidden=\{Boolean\(current\) && tab !== "ai"\}[\s\S]*?<TaskManviAssist/);
  });

  it("deletes through the in-app confirm and retries the same delete identity", () => {
    expect(source).toContain("askConfirm");
    expect(source).toContain("deleteTask");
    expect(source).toContain("pendingDelete");
    expect(source).toContain("Retry delete");
    expect(source).not.toContain("window.confirm");
  });
});
