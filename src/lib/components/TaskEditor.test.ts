import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { compile } from "svelte/compiler";

const source = readFileSync(new URL("./TaskEditor.svelte", import.meta.url), "utf8");

/**
 * The Task pane's markup, bounded by the next pane's anchor.
 *
 * The bounds are asserted rather than assumed, and that is not ceremony.
 * These slices used to end at `panelId(group, "organize")`; when that pane was
 * merged into Task, `indexOf` returned -1 and `slice(start, -1)` quietly
 * returned *almost the entire file* instead of nothing. Every `toContain`
 * below would still have passed, and the ordering checks would have started
 * comparing positions across the whole component — green tests measuring
 * nothing. Fail on the anchor instead.
 */
function taskPane(): string {
  const start = source.indexOf('panelId(group, "task")');
  const end = source.indexOf('panelId(group, "agent")');
  expect(start, "Task pane anchor is missing").toBeGreaterThanOrEqual(0);
  expect(end, "Agent pane anchor is missing, so the Task pane has no end").toBeGreaterThan(start);
  return source.slice(start, end);
}

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

  it("keeps its keyboard shortcuts working from the overlays it opened", () => {
    // The repository popover is portaled out of the sheet to escape
    // `.sheet-body`'s scroller, so `sheet.contains(target)` is false for it.
    // A guard that asked only that left Cmd+S and Cmd+Shift+C dead whenever
    // focus was in the picker.
    expect(source).toMatch(/const owns = \(node: Node\) =>[\s\S]*?data-task-repo-popup/);
    expect(source).toMatch(/!owns\(e\.target\)\) return/);
  });

  it("uses typed controls for kind, severity, labels, and criteria bounds", () => {
    expect(source).toContain("KIND_OPTIONS");
    expect(source).toContain("Custom…");
    expect(source).toContain("SEVERITY_OPTIONS");
    expect(source).toContain("LabelInput");
    expect(source).toContain('maxlength="65536"');
    expect(source).toContain("Notifications (profile-wide; mute this task)");
    expect(source).not.toContain("TaskEnhancements");
    expect(source).toContain("TaskManviAssist");
  });

  it("gives a draft one pane and no tab strip, and a saved task two", () => {
    // The rule lives in `taskEditorTabs`; what this guards is that the sheet
    // actually asks. A strip rendered unconditionally would put Agent in front
    // of someone creating a task, where it is empty by definition.
    expect(source).toContain("const tabs = $derived(editorTabs(Boolean(current)))");
    expect(source).toContain("const tab = $derived(resolveEditorTab(requestedTab, Boolean(current)))");
    expect(source).toMatch(/\{#if tabs\.length > 1\}[\s\S]*?role="tablist"/);
    // Panes and tabs are addressed through the shared tablist owner, so their
    // ids match by construction rather than by two hand-written templates.
    expect(source).toContain('tabProps(group, entry.id, tab === entry.id)');
    for (const pane of ["task", "agent"]) {
      expect(source).toContain(`panelId(group, "${pane}")`);
      expect(source).toContain(`tabId(group, "${pane}")`);
    }
    // Organize and AI were merged into Task. A pane left behind here would
    // render permanently hidden, because no tab can ever select it again.
    for (const gone of ["organize", "ai"]) {
      expect(source, `the ${gone} pane was merged into Task`).not.toContain(`panelId(group, "${gone}")`);
    }
    // Tailwind's preflight `[hidden]` rule has zero specificity, so a plain
    // `.pane{display:flex}` here would leave every pane on screen at once.
    expect(source).toContain(".pane[hidden]{display:none}");
  });

  it("carries the whole task — links, text, schedule and the assist — on one pane", () => {
    // Everything a task needs to exist stays in one place, and so does
    // everything about how it is handled. The reader writes a title, sets a due
    // date and asks for a better description in one sitting; those used to be
    // three panes, with the AI pane's *output* drawn on the Task pane.
    const pane = taskPane();
    expect(pane).toContain('name="task-title"');
    expect(pane).toContain("<TaskRepositoryPicker");
    expect(pane).toContain("Acceptance criteria");
    expect(pane).toContain("<TaskManviAssist");
    expect(pane).toContain("LabelInput");
    expect(pane).toContain("Notifications (profile-wide; mute this task)");
    expect(source).not.toContain("More details");
    expect(source).not.toContain("showDetails");
    // A draft used to hide the scheduling fields behind a disclosure because
    // one column had no room for them. Two columns do, and a draft that hides
    // what a saved task shows is an asymmetry with nothing behind it.
    expect(source).not.toContain("showOrganize");
    expect(source).not.toContain("Schedule and labels");
  });

  it("numbers the sections instead of splitting them into columns", () => {
    // The two-column grid only applied past 520px and the dock's floor is
    // 380px, so which layout a reader got depended on how far they had dragged
    // the splitter. Five numbered sections read the same at every width.
    const pane = taskPane();
    expect(source).not.toContain("pane-grid");
    expect(source).not.toContain("@container");
    expect(pane).toContain('<div class="steps">');
    for (const [n, heading] of [
      ["1", "Repositories"],
      ["2", "Quick add"],
      ["3", "Title and description"],
      ["4", "Status and scheduling"],
      ["5", "Owner"],
    ] as const) {
      expect(pane).toContain(`<span class="step-n" aria-hidden="true">${n}</span><h3>${heading}</h3>`);
    }
    // In that order, and with nothing unnumbered between them.
    const order = [...pane.matchAll(/<span class="step-n" aria-hidden="true">(\d)<\/span>/g)].map((m) => m[1]);
    expect(order).toEqual(["1", "2", "3", "4", "5"]);
  });

  it("gives the due date the picker rather than the platform's datetime box", () => {
    const pane = taskPane();
    expect(pane).toContain("<TaskDuePicker");
    expect(source).not.toContain("datetime-local");
    expect(source).not.toContain("dueInputValue");
    expect(source).not.toContain("parseDueInput");
    // A day is chosen with a button, which fires no `input` event, so the
    // form's own `oninput`/`onchange` dirty-marking cannot see it.
    expect(pane).toContain("onChange={(next) => { draft.due_at = next; dirty = true; }}");
    // The popover is portaled out of the sheet, so a disabled `<fieldset>`
    // around the component would not reach the controls inside it.
    expect(pane).toMatch(/<TaskDuePicker[\s\S]*?disabled=\{saving \|\| reloading \|\| pending !== null \|\| pendingDelete !== null \|\| enhancementBusy\}/);
  });

  it("drops the Home workspace control without dropping the value it held", () => {
    // The control was removed because picking a home workspace from the sheet
    // did not work; the field itself is still part of a task and still has to
    // survive a save. The draft carries whatever was loaded (or seeded for a
    // new task in a workspace) straight back to `taskWrite`.
    const pane = taskPane();
    expect(pane).not.toContain("Home workspace");
    expect(pane).not.toMatch(/bind:value=\{draft\.home_workspace_id\}/);
    expect(source).toContain("home_workspace_id: initial.home");
    // And it is still read, for the membership notice under the picker.
    expect(source).toContain("const workspaceId = draft.home_workspace_id");
    expect(source).toContain("workspace={draft.home_workspace_id ? { name: homeWorkspaceName, error: homeError } : null}");
  });

  it("leads the sheet with the repository control, ahead of the title", () => {
    // A task cannot be saved without a linked repository — the footer's Save
    // is disabled on `!draft.repository_ids.length`. This used to be the last
    // control on the pane, so the only mandatory field was the one you had to
    // scroll to, and on a draft the assist's notes box came before it.
    expect(source).toContain("!draft.repository_ids.length");
    const pane = taskPane();
    const picker = pane.indexOf("<TaskRepositoryPicker");
    expect(picker).toBeGreaterThanOrEqual(0);
    expect(picker).toBeLessThan(pane.indexOf('name="task-title"'));
    expect(picker).toBeLessThan(pane.indexOf("Acceptance criteria"));
    // The notes box no longer takes the cursor on open; a draft lands on its
    // title, which keeps the picker directly above it on screen.
    expect(source).not.toContain("autofocus={!current}");
  });

  it("hands the repository control one owner for linking and the primary choice", () => {
    // The rows and the primary radio live in TaskRepositoryPicker; the sheet
    // owns the mutations, because they write `draft` and `dirty`. What must
    // never come back is a second control that can disagree with the first.
    expect(source).toContain("repositoryRows(known, draft.repository_ids, draft.primary_repository_id, homeMembers, repoFilter)");
    expect(source).toContain("onToggle={membership}");
    expect(source).toContain("onPrimary={setPrimary}");
    // Replaced, not accumulated: the standalone Primary repository select is gone.
    expect(source).not.toContain("bind:value={draft.primary_repository_id}");
    // The pane-level fieldset must not become a second scroller around the
    // control that already owns its own.
    expect(source).not.toMatch(/\.repositories\{[^}]*overflow:auto/);
  });

  it("marks the draft dirty from the handlers, not from a bubbling input", () => {
    // The picker is portaled out of `<form oninput={() => { dirty = true; }}>`,
    // so its events no longer reach the form at all. The behaviour is the same
    // as before, but the reason it is correct moved: these two handlers are now
    // the only thing that can mark a repository change unsaved.
    expect(source).toMatch(/function membership\([\s\S]*?dirty = true/);
    expect(source).toMatch(/function setPrimary\([\s\S]*?dirty = true/);
    // Joining a workspace writes the workspace, not the task, so it must not.
    // Sliced by index rather than matched with a lazy regex: `[\s\S]*?` happily
    // runs past the closing brace and finds a `dirty = true` in some later
    // function, which would make this negative unfailable.
    const start = source.indexOf("async function attachOutsiders()");
    expect(start).toBeGreaterThan(0);
    const body = source.slice(start, source.indexOf("\n  }", start));
    expect(body).not.toContain("dirty = true");
  });

  it("reads the home workspace's own membership and offers to close the gap", () => {
    // Membership follows the draft, not the board's scope: the Home workspace
    // control can move the task to another workspace while the sheet is open.
    expect(source).toContain("const workspaceId = draft.home_workspace_id");
    expect(source).toContain("attachRepositories(workspaceId, summary.outsiders)");
    expect(source).toContain("onAttachOutsiders={() => void attachOutsiders()}");
    // An unread membership is never drawn as an empty workspace.
    expect(source).toContain("homeError = explainError(cause)");
    // `null` means "no workspace, or unread" and the picker renders marks only
    // for a real list, so the sheet must hand it through unflattened.
    expect(source).toContain("workspace={draft.home_workspace_id ? { name: homeWorkspaceName, error: homeError } : null}");
  });

  it("leaves acceptance to the assist that produced the suggestion", () => {
    // The sheet used to draw "Use this title" under each field while the
    // assist's own review drew no buttons at all. One decision with two owners,
    // and it left the assist's history dropdown able to change nothing visible.
    // Acceptance now lives once, beside the diff; the sheet keeps only the
    // flash, so a field that was just rewritten says so.
    const pane = taskPane();
    expect(source).not.toContain("acceptFields");
    expect(source).not.toContain("hideSuggestion");
    expect(source).not.toContain("inline-suggestion");
    // On the pane, not the whole file: the comment above `flash` names the
    // button that used to be here, which is the point of the comment.
    expect(pane).not.toContain("Use this title");
    expect(pane).not.toContain("Use this description");
    expect(source).toContain("onFlash={(fields) => { flash = fields; }}");
    expect(source).toContain('class:flash={flash.includes("title")}');
    expect(source).toContain('class:flash={flash.includes("description")}');

    // The dictation and the fields it fills are in reading order: the capture
    // surface first, then Title and Description under it. Pressing a button on
    // one pane and reading the result on another is what the merge removed.
    expect(pane.indexOf("<TaskManviAssist")).toBeLessThan(pane.indexOf('name="task-title"'));
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

  it("deletes through the in-app confirm and retries the same delete identity", () => {
    expect(source).toContain("askConfirm");
    expect(source).toContain("deleteTask");
    expect(source).toContain("pendingDelete");
    expect(source).toContain("Retry delete");
    expect(source).not.toContain("window.confirm");
  });
});
