import { describe, expect, it } from "vitest";
import { wallClockBudget } from "../testing/wallClock";
import {
  AGENT_COPY_COMPLETION,
  AGENT_GUIDANCE_SECTION,
  MAX_AGENT_COPY_TASKS,
  MAX_SUBTASKS,
  SUBTASK_CAP,
  appendCriteria,
  applyNotesToDraft,
  canAskManvi,
  criteriaFromPaste,
  composeFromNotes,
  describeChecklistMove,
  describeRefusedCriteria,
  extractDraftSubtasks,
  extractSubtasks,
  extractSubtasksFromContent,
  formatDraftAgentCopy,
  joinAgentCopies,
  mergeSubtasks,
  sanitizeNotes,
  suggestionDiffers,
  titleFromNotes,
  wrapSavedBriefForAgent,
} from "./taskCompose";

describe("titleFromNotes", () => {
  it("takes the first sentence, strips controls, and caps length", () => {
    expect(titleFromNotes("")).toBe("");
    expect(titleFromNotes("   ")).toBe("");
    expect(titleFromNotes(12)).toBe("");
    expect(titleFromNotes("Keep E42.\nThen add metrics.")).toBe("Keep E42.");
    expect(titleFromNotes("line1\nline2")).toBe("line1");
    expect(titleFromNotes("a\u0000b")).toBe("ab");
    expect(titleFromNotes("x".repeat(400)).length).toBe(300);
  });

  it("does not treat list markers, abbreviations, or markup as the title", () => {
    expect(titleFromNotes("1. Fix the auth bug in both repos")).toBe("1. Fix the auth bug in both repos");
    expect(titleFromNotes("12. Restore the original E42 path")).toBe("12. Restore the original E42 path");
    expect(titleFromNotes("Dr. Smith reported E42 on login.")).toBe("Dr. Smith reported E42 on login.");
    expect(titleFromNotes("e.g. keep the original stack trace.")).toBe("e.g. keep the original stack trace.");
    expect(titleFromNotes("i.e. the original E42 must stay.")).toBe("i.e. the original E42 must stay.");
    expect(titleFromNotes("# Keep the original E42 across both links")).toBe("Keep the original E42 across both links");
    expect(titleFromNotes("- [ ] Preserve repository scope")).toBe("Preserve repository scope");
    expect(titleFromNotes("* Keep both repository links")).toBe("Keep both repository links");
  });

  it("skips pasted links and strips quotes and invisible characters", () => {
    expect(titleFromNotes("https://github.com/acme/app/issues/42\nLogin loops after the token refresh")).toBe(
      "Login loops after the token refresh",
    );
    expect(titleFromNotes("https://github.com/acme/app/issues/42")).toBe("");
    expect(titleFromNotes("---\nKeep the retry bounded")).toBe("Keep the retry bounded");
    expect(titleFromNotes("> Keep the retry bounded")).toBe("Keep the retry bounded");
    expect(titleFromNotes("“Keep the retry bounded”")).toBe("Keep the retry bounded");
    // Zero-width and bidi controls make a title read other than what it holds.
    expect(titleFromNotes("Fix\u200B the \u202Elogin\u202C loop")).toBe("Fix the login loop");
    expect(titleFromNotes("Fix the login loop\u2028Second line")).toBe("Fix the login loop");
  });
});

describe("applyNotesToDraft", () => {
  it("fills empty title and description from notes without inventing when notes are empty", () => {
    expect(applyNotesToDraft({ title: "", description: "" }, "")).toEqual({
      title: "",
      description: "",
      extracted: false,
    });
    expect(applyNotesToDraft({ title: "", description: "" }, "Preserve repository scope.\nKeep both links.")).toEqual({
      title: "Preserve repository scope.",
      description: "Preserve repository scope.\nKeep both links.",
      extracted: true,
    });
    expect(applyNotesToDraft({ title: "Keep E42", description: "old" }, "New notes about E42")).toEqual({
      title: "Keep E42",
      description: "old\n\nNew notes about E42",
      extracted: true,
    });
    expect(applyNotesToDraft({ title: "", description: "" }, "   \n  ")).toMatchObject({ extracted: false });
  });

  it("does not silently discard notes beyond the save limit", () => {
    const notes = "x".repeat(65_537);
    expect(applyNotesToDraft({ title: "Keep", description: "" }, notes).description).toBe(notes);
    expect(canAskManvi({ title: "Keep", repository_ids: ["r"] }, notes)).toMatch(/64 KB/);
    expect(canAskManvi({ title: "Keep", repository_ids: ["r"] }, "界".repeat(22_000))).toMatch(/64 KB/);
  });

  it("does not split Unicode code points when deriving a title", () => {
    const title = titleFromNotes("😀".repeat(301));
    expect(title.isWellFormed()).toBe(true);
    expect([...title]).toHaveLength(300);
  });

  it("keeps numbered and constrained notes as a usable draft Manvi can rewrite", () => {
    const numbered = applyNotesToDraft({ title: "", description: "" }, "1. Fix the auth bug in both repos\nMust keep E42.");
    expect(numbered.title).toBe("1. Fix the auth bug in both repos");
    expect(numbered.description).toContain("Must keep E42.");
    expect(canAskManvi({ title: "", repository_ids: ["r"] }, "1. Fix the auth bug in both repos")).toBeNull();
    const constrained = composeFromNotes({ title: "", description: "" }, "Must keep E42. Do not drop the reproduction steps.");
    expect(constrained.title).toBe("Must keep E42.");
    expect(constrained.description).toContain("Do not drop the reproduction steps.");
    expect(constrained.notes).toBe("");
  });

  it("consumes notes into the draft once so they cannot be re-applied", () => {
    const first = composeFromNotes({ title: "", description: "" }, "Fix notification routing\nKeep saved evidence.");
    expect(first).toMatchObject({
      title: "Fix notification routing",
      // The title is a whole line, so it is not written a second time below.
      description: "Keep saved evidence.",
      notes: "",
      extracted: true,
      titleFromNotes: true,
    });
    expect(composeFromNotes({ title: first.title, description: first.description }, first.notes).extracted).toBe(false);
    const leftover = composeFromNotes(
      { title: "Prepare Demo for Seattle start-up event", description: "Prepare Demo for Seattle start-up event" },
      "Prepare Demo for Seattle start-up event",
    );
    expect(leftover.extracted).toBe(true);
    expect(leftover.notes).toBe("");
    expect(leftover.titleFromNotes).toBe(false);
  });
});

describe("canAskManvi", () => {
  it("requires a repository and either a title or notes", () => {
    expect(canAskManvi({ title: "Keep", repository_ids: [] }, "notes")).toMatch(/repository/);
    expect(canAskManvi({ title: "", repository_ids: ["r"] }, "")).toMatch(/title/);
    expect(canAskManvi({ title: "", repository_ids: ["r"] }, "Draft this bug")).toBeNull();
    expect(canAskManvi({ title: "Keep", repository_ids: ["r"] }, "")).toBeNull();
  });

  it("measures the description the save would write, so a mostly-checklist paste is not refused", () => {
    // ~90 KB of notes, nearly all of it checklist items that become criteria.
    const notes = `Fix the importer\n\nSubtasks:\n${Array.from({ length: 100 }, (_, i) => `- [ ] step ${i} ${"x".repeat(900)}`).join("\n")}`;
    expect(new TextEncoder().encode(notes).length).toBeGreaterThan(65_536);
    expect(canAskManvi({ title: "", repository_ids: ["r"] }, notes)).toBeNull();
  });
});

describe("agent copy", () => {
  it("copies a draft with no priority as Normal, and keeps Urgent as Urgent", () => {
    const base = { title: "T", description: "d" };
    expect(formatDraftAgentCopy(base)).toContain("Priority: Normal");
    expect(formatDraftAgentCopy({ ...base, priority: 0 })).toContain("Priority: Urgent");
    expect(formatDraftAgentCopy({ ...base, priority: 3 })).toContain("Priority: Low");
  });

  it("labels drafts as unsaved and never invents a revision", () => {
    const copy = formatDraftAgentCopy({
      title: "Keep E42",
      description: "exact error: E42",
      kind: "bug",
      status: "ready",
      priority: 0,
      labels: ["regression"],
      acceptance_criteria: ["Both repository checks pass"],
      repositoryNames: ["GitPulse"],
    });
    expect(copy).toContain("Unsaved GitPulse task draft");
    expect(copy).not.toMatch(/revision \d/);
    expect(copy?.startsWith(AGENT_COPY_COMPLETION)).toBe(true);
    // Shaped like the store's brief: the guidance section, then the task.
    expect(copy).toContain(`${AGENT_GUIDANCE_SECTION}\n\n## Title\nKeep E42`);
    expect(copy).toContain("Keep E42");
    expect(copy).toContain("exact error: E42");
    expect(copy).toContain("- [ ] Both repository checks pass");
    expect(copy).toContain("GitPulse");
    expect(formatDraftAgentCopy({ title: "   ", description: "" })).toBeNull();
  });

  it("tells the agent how to mark a saved task done, conditionally and never for unfinished work", () => {
    // A copied prompt reaches agents GitPulse did not launch. They learn the
    // task id from the brief's Task: line, which only a saved brief carries.
    expect(AGENT_COPY_COMPLETION).toContain("gitpulse_complete_task");
    expect(AGENT_COPY_COMPLETION).toContain("Task: line");
    expect(AGENT_COPY_COMPLETION).toContain("status review");
    expect(AGENT_COPY_COMPLETION).toContain("Never mark done work you did not finish");
  });

  it("carries pasted raw logs as a fenced evidence section and omits them when empty", () => {
    const withLogs = formatDraftAgentCopy({
      title: "Keep E42",
      description: "exact error: E42",
      logs: "\u001b[31mpanic at 'E42'\u001b[0m\n    at src/main.rs:12",
    })!;
    expect(withLogs).toContain("## Raw logs");
    expect(withLogs).toContain("panic at 'E42'");
    expect(withLogs).toContain("src/main.rs:12");
    expect(withLogs).not.toContain("\u001b");
    expect(formatDraftAgentCopy({ title: "Keep E42", description: "exact error: E42", logs: "" })).not.toContain("## Raw logs");
    expect(formatDraftAgentCopy({ title: "Keep E42", description: "exact error: E42" })).not.toContain("## Raw logs");
  });

  it("announces a description it had to cut instead of shortening the copy in silence", () => {
    // `applyNotesToDraft` raises its own cap on purpose, so an unsaved draft
    // can carry more than a saved task's 64 KiB — this is the one path where
    // the packet can be shorter than what the author typed. A copy that stops
    // mid-sentence with no marker reads as the whole description.
    const tail = "the reproduction ends with E42";
    const oversized = `${"x".repeat(65_536)}\n${tail}`;
    const copy = formatDraftAgentCopy({ title: "Keep E42", description: oversized })!;
    expect(copy).not.toContain(tail);
    expect(copy).toContain(`[description truncated: 65536 of ${[...oversized].length} characters shown]`);
    // A description that fits is passed through untouched, marker and all.
    const exact = formatDraftAgentCopy({ title: "Keep E42", description: "x".repeat(65_536) })!;
    expect(exact).not.toContain("[description truncated");
    expect(formatDraftAgentCopy({ title: "Keep E42", description: `exact error: E42\n${tail}` }))
      .toContain(tail);
  });

  it("carries the tool guidance and the preservation rule into every packet", () => {
    // Both copy paths reach an agent that has never seen this repository:
    // whatever is missing here is missing entirely. A saved brief carries the
    // store's guidance section itself; a draft renders the same section. The
    // saved fixture is shaped as the store exports it, guidance first.
    const draft = formatDraftAgentCopy({ title: "Keep E42", description: "exact error: E42" })!;
    const saved = wrapSavedBriefForAgent(`# Task brief v1\n\n${AGENT_GUIDANCE_SECTION}\n\n## Title\nKeep E42`)!;
    for (const packet of [draft, saved]) {
      // Exactly once: a copy that added its own would hand over two.
      expect(packet.split(AGENT_GUIDANCE_SECTION).length - 1).toBe(1);
      expect(packet).toContain("Preserve the author's intent and message");
      expect(packet).toContain("gitpulse-insights");
      expect(packet).toContain("gitpulse-tasks");
      expect(packet).toContain("devmap-impact");
      expect(packet).toContain("devmap_* MCP tools");
      expect(packet).toContain("gitpulse_* MCP tools");
      expect(packet).toContain("absolute repo_path");
    }
  });

  it("wraps a saved brief and refuses empty or hostile markdown", () => {
    const wrapped = wrapSavedBriefForAgent("# Task brief v1\n\n## Title\nKeep E42");
    expect(wrapped?.startsWith(AGENT_COPY_COMPLETION)).toBe(true);
    expect(wrapped).toContain("# Task brief v1");
    expect(wrapSavedBriefForAgent("")).toBeNull();
    expect(wrapSavedBriefForAgent("\u0000")).toBeNull();
    expect(wrapSavedBriefForAgent(12)).toBeNull();
  });

  it("joins a capped set of packets and drops empties", () => {
    expect(joinAgentCopies(["a", "", "b"])).toContain("---");
    expect(joinAgentCopies(Array.from({ length: MAX_AGENT_COPY_TASKS + 5 }, (_, i) => `t${i}`))?.split("---")).toHaveLength(MAX_AGENT_COPY_TASKS);
    expect(joinAgentCopies([])).toBeNull();
    expect(sanitizeNotes("a\u0007b")).toBe("ab");
  });

  it("ignores identical or empty Manvi wording", () => {
    expect(suggestionDiffers("Keep E42", "Keep E42")).toBe(false);
    expect(suggestionDiffers("Keep E42", " Keep E42 ")).toBe(false);
    expect(suggestionDiffers("Keep E42", "Investigate E42")).toBe(true);
    expect(suggestionDiffers("Keep E42", "")).toBe(false);
  });
});

describe("subtask extraction and merging", () => {
  it("extracts subtasks from markdown checkboxes across marker formats", () => {
    const markdown = [
      "- [ ] First criterion",
      "- [x] Second completed criterion",
      "* [ ] Star bullet criterion",
      "+ [x] Plus bullet criterion",
      "[ ] Bracket only criterion",
      "[X] Uppercase X criterion",
    ].join("\n");
    expect(extractSubtasks(markdown)).toEqual([
      "First criterion",
      "Second completed criterion",
      "Star bullet criterion",
      "Plus bullet criterion",
      "Bracket only criterion",
      "Uppercase X criterion",
    ]);
  });

  it("extracts subtasks from dedicated checklist and subtask sections", () => {
    const brief = [
      "## Description",
      "Some descriptive text about the bug.",
      "",
      "## Task checklist",
      "- [x] DevMap status/search/impact on symbols",
      "- [ ] Expand attention latch for failed/interrupted states",
      "- [ ] Replace sheet latch with riseAssistOpen",
      "",
      "## Acceptance criteria",
      "No acceptance criteria recorded.",
      "",
      "Some trailing notes.",
    ].join("\n");

    const result = extractSubtasksFromContent(brief);
    expect(result.subtasks).toEqual([
      "DevMap status/search/impact on symbols",
      "Expand attention latch for failed/interrupted states",
      "Replace sheet latch with riseAssistOpen",
    ]);
    expect(result.remainingText).toContain("Some descriptive text about the bug.");
    expect(result.remainingText).toContain("Some trailing notes.");
    expect(result.remainingText).not.toContain("## Task checklist");
    expect(result.remainingText).not.toContain("DevMap status/search");
  });

  it("extracts numbered and bulleted lists under subtask headings but not general lists", () => {
    const text = [
      "Supported browsers:",
      "- Chrome",
      "- Firefox",
      "",
      "Subtasks:",
      "1. Wire parser",
      "2. Add test suite",
      "3. Deploy fix",
    ].join("\n");

    const extracted = extractSubtasks(text);
    expect(extracted).toEqual([
      "Wire parser",
      "Add test suite",
      "Deploy fix",
    ]);
  });

  it("ignores empty, non-string, and placeholder criteria", () => {
    expect(extractSubtasks("")).toEqual([]);
    expect(extractSubtasks(null)).toEqual([]);
    expect(extractSubtasks(undefined)).toEqual([]);
    expect(extractSubtasks("## Acceptance criteria\nNo acceptance criteria recorded.")).toEqual([]);
    expect(extractSubtasks("- [ ] None\n- [ ] N/A")).toEqual([]);
  });

  it("merges subtasks deduplicating case-insensitively and respecting bounds", () => {
    const existing = ["Reproduce bug", "Add test"];
    const incoming = ["add test", "Verify fix", "reproduce BUG"];
    expect(mergeSubtasks(existing, incoming)).toEqual([
      "Reproduce bug",
      "Add test",
      "Verify fix",
    ]);

    const capped = mergeSubtasks([], ["a", "b", "c"], 2);
    expect(capped).toEqual(["a", "b"]);
    expect(mergeSubtasks([], Array.from({ length: MAX_SUBTASKS + 10 }, (_, i) => `item ${i}`))).toHaveLength(MAX_SUBTASKS);

    // Raw pasted lines carry their markers; every shape comes out bare.
    expect(mergeSubtasks([], ["- [ ] Gamma", "* [x] Done one", "2. Delta", "3) [ ] Epsilon", "[ ] Zeta"])).toEqual([
      "Gamma", "Done one", "Delta", "Epsilon", "Zeta",
    ]);

    const longItem = "x".repeat(SUBTASK_CAP + 50);
    const mergedLong = mergeSubtasks([], [longItem]);
    expect(mergedLong[0]?.length).toBe(SUBTASK_CAP);
  });

  it("extractDraftSubtasks moves the description's checklist and reports the new count", () => {
    const draft = {
      description: "Fix loop\n\n## Subtasks\n- [ ] Item 1\n- [ ] Item 2",
      acceptance_criteria: ["Item 1"],
    };
    const result = extractDraftSubtasks(draft);
    expect(result.extracted).toBe(true);
    expect(result.count).toBe(1); // Item 2 is new; Item 1 was already listed
    expect(result.duplicates).toBe(1);
    expect(result.subtasks).toEqual(["Item 1", "Item 2"]);
    expect(result.description).toBe("Fix loop");
    // Notes are composeFromNotes's: Extract never reads them, so the preview
    // and the button cannot both move the same checklist.
    expect(result).not.toHaveProperty("notes");
  });
});

describe("checklist extraction keeps everything it does not take", () => {
  it("never reads a checklist out of a code fence", () => {
    const text = ["Run this:", "```md", "- [ ] not a task", "Subtasks:", "- also code", "```", "- [ ] Real item"].join("\n");
    const result = extractSubtasksFromContent(text);
    expect(result.subtasks).toEqual(["Real item"]);
    expect(result.remainingText).toBe(["Run this:", "```md", "- [ ] not a task", "Subtasks:", "- also code", "```"].join("\n"));
    const tilde = extractSubtasksFromContent("~~~\n- [ ] code\n~~~");
    expect(tilde.subtasks).toEqual([]);
    expect(tilde.remainingText).toBe("~~~\n- [ ] code\n~~~");
  });

  it("keeps an item that wraps onto an indented line as one item, and the list going after it", () => {
    const text = ["Subtasks:", "- Do the long thing that", "  continues on this line", "- Second", "", "Afterword."].join("\n");
    const result = extractSubtasksFromContent(text);
    expect(result.subtasks).toEqual(["Do the long thing that continues on this line", "Second"]);
    expect(result.remainingText).toBe("Afterword.");
    // A nested bullet is its own item, not a continuation.
    expect(extractSubtasks("- [ ] Parent\n  - [ ] Child")).toEqual(["Parent", "Child"]);
  });

  it("leaves items past the cap in the text instead of dropping them", () => {
    const existing = Array.from({ length: MAX_SUBTASKS - 1 }, (_, i) => `have ${i}`);
    const result = extractDraftSubtasks({ description: "- [ ] fits\n- [ ] spills", acceptance_criteria: existing });
    expect(result.subtasks).toHaveLength(MAX_SUBTASKS);
    expect(result.subtasks.at(-1)).toBe("fits");
    expect(result.description).toBe("- [ ] spills");
    expect(result.overflow).toBe(1);
    expect(describeChecklistMove(result)).toBe("Moved 1 into acceptance criteria · 1 left in the text (list full)");
  });

  it("does not touch spacing it did not remove, and returns untouched text unchanged", () => {
    const spaced = "Intro\n\n\n\n```\nkeep\n\n\nthis\n```";
    expect(extractSubtasksFromContent(spaced).remainingText).toBe(spaced);
    expect(extractSubtasksFromContent("  indented first line\n- [ ] item").remainingText).toBe("  indented first line");
    const around = "Before\n\n- [ ] gone\n\nAfter\n\n\n\nEnd";
    expect(extractSubtasksFromContent(around).remainingText).toBe("Before\n\nAfter\n\n\n\nEnd");
    // A list that keeps some items is not split by a blank where one left.
    const listed = extractSubtasksFromContent("Steps:\n- keep one\n- [ ] gone\n- keep two");
    expect(listed.remainingText).toBe("Steps:\n- keep one\n- keep two");
    // Prose that a removed item separated keeps its paragraph break.
    expect(extractSubtasksFromContent("Intro\n- [ ] gone\nOutro").remainingText).toBe("Intro\n\nOutro");
  });

  it("reports a list that was already complete as moved, not as zero extracted", () => {
    const result = extractDraftSubtasks({ description: "Plan\n- [ ] Ship it", acceptance_criteria: ["ship it"] });
    expect(result).toMatchObject({ extracted: true, count: 0, duplicates: 1, description: "Plan" });
    expect(describeChecklistMove(result)).toBe("Removed 1 line already in acceptance criteria");
    expect(describeChecklistMove(extractDraftSubtasks({ description: "Nothing here" }))).toMatch(/No checklist found/);
  });

  it("keeps a checklist heading whose items had to stay", () => {
    const full = Array.from({ length: MAX_SUBTASKS }, (_, i) => `have ${i}`);
    const result = extractSubtasksFromContent("## Subtasks\n- one\n- two", { existing: full });
    expect(result.subtasks).toEqual([]);
    expect(result.overflow).toBe(2);
    expect(result.remainingText).toBe("## Subtasks\n- one\n- two");
  });

  it("knows definition-of-done sections", () => {
    expect(extractSubtasks("Definition of done:\n- Tests pass\n- Docs updated")).toEqual(["Tests pass", "Docs updated"]);
  });

  it("reads checklist headings written in bold, with a note, or with other names", () => {
    expect(extractSubtasks("**Acceptance criteria:**\n- Tests pass")).toEqual(["Tests pass"]);
    expect(extractSubtasks("__To do__:\n- Write it")).toEqual(["Write it"]);
    expect(extractSubtasks("Requirements (must-have):\n- Offline mode")).toEqual(["Offline mode"]);
    expect(extractSubtasks("AC:\n1. Loads in 2s")).toEqual(["Loads in 2s"]);
    // A sentence that starts with a name is not a heading.
    expect(extractSubtasks("Tasks are slow to load\n- not an item")).toEqual([]);
    expect(extractSubtasks("Requirements: must run offline\n- not an item")).toEqual([]);
  });

  it("reads the bullets and boxes Notes, Slack and word processors paste", () => {
    expect(extractSubtasks("Subtasks:\n• Reproduce\n– Add a test\n— Ship")).toEqual(["Reproduce", "Add a test", "Ship"]);
    expect(extractSubtasks("☐ Open box\n☑ Ticked box")).toEqual(["Open box", "Ticked box"]);
    expect(extractSubtasks("1. [ ] Numbered checkbox")).toEqual(["Numbered checkbox"]);
  });

  it("keeps a detail line under a checkbox with its item instead of orphaning it", () => {
    const result = extractSubtasksFromContent("- [ ] Parent\n  - detail one\nAfter");
    expect(result.subtasks).toEqual(["Parent; detail one"]);
    expect(result.remainingText).toBe("After");
  });

  it("caps a criterion by the store's bytes, not characters, and never splits a character", () => {
    const [item] = mergeSubtasks([], ["界".repeat(2000)]);
    expect(new TextEncoder().encode(item).length).toBeLessThanOrEqual(SUBTASK_CAP);
    expect(item!.isWellFormed()).toBe(true);
    expect([...item!]).toHaveLength(Math.floor(SUBTASK_CAP / 3));
  });

  it("keeps an item that wraps onto an unindented line, the way a hard-wrapped paste arrives", () => {
    const result = extractSubtasksFromContent("- [ ] Retry the upload when the\nnetwork drops\n- [ ] Second");
    expect(result.subtasks).toEqual(["Retry the upload when the network drops", "Second"]);
    expect(result.remainingText).toBe("");
    // A line ending mid-clause pulls the next one in even when it is capitalised.
    expect(extractSubtasks("Subtasks:\n- Check every size we ship,\nPNG and JPEG")).toEqual(["Check every size we ship, PNG and JPEG"]);
    // A new sentence after an item is prose, not the item's tail.
    const thanks = extractSubtasksFromContent("- [ ] Ship it\nThanks");
    expect(thanks.subtasks).toEqual(["Ship it"]);
    expect(thanks.remainingText).toBe("Thanks");
    // An item that ends its sentence does not take the next line, lowercase
    // or not: a wrap here merges prose into a criterion.
    const ended = extractSubtasksFromContent("- [ ] Ship it.\nthen we celebrate");
    expect(ended.subtasks).toEqual(["Ship it."]);
    expect(ended.remainingText).toBe("then we celebrate");
    expect(extractSubtasks("- [ ] Does it load?\nmaybe not")).toEqual(["Does it load?"]);
    const doc = extractSubtasksFromContent("Subtasks:\n- a\nSee the doc.\n- b");
    expect(doc.subtasks).toEqual(["a"]);
    // The prose ends the list, so "- b" under it is not an item.
    expect(doc.remainingText).toBe("See the doc.\n- b");
  });

  it("keeps an item and the code block under it together, and the list going after it", () => {
    const text = ["Subtasks:", "- Run it", "  ```sh", "  make", "  - not an item", "  ```", "- Second"].join("\n");
    const result = extractSubtasksFromContent(text);
    expect(result.subtasks).toEqual(["Second"]);
    expect(result.remainingText).toBe(["Subtasks:", "- Run it", "  ```sh", "  make", "  - not an item", "  ```"].join("\n"));
    // Deeper than three spaces is still the item's code, not its wrapped text.
    const deep = extractSubtasksFromContent("  - [ ] Build\n      ```\n      cargo b\n      ```\n  - [ ] Test");
    expect(deep.subtasks).toEqual(["Test"]);
    expect(deep.remainingText).toBe("  - [ ] Build\n      ```\n      cargo b\n      ```");
  });

  it("reads a list whose heading has one introducing line before the items", () => {
    const result = extractSubtasksFromContent("Intro\n\n## Tasks\nDo these in order:\n- a\n- b\n\nAfter");
    expect(result.subtasks).toEqual(["a", "b"]);
    expect(result.remainingText).toBe("Intro\n\nAfter");
    // The heading and its intro stay when an item had to.
    const full = Array.from({ length: MAX_SUBTASKS - 1 }, (_, i) => `have ${i}`);
    const kept = extractSubtasksFromContent("## Tasks\nDo these in order:\n- a\n- b", { existing: full });
    expect(kept.subtasks).toEqual(["a"]);
    expect(kept.remainingText).toBe("## Tasks\nDo these in order:\n- b");
    // Markdown spacing: blank lines around the intro are the usual way to write it.
    const spaced = extractSubtasksFromContent("Intro\n\n## Tasks\n\nDo these in order:\n\n- a\n- b\n\nAfter");
    expect(spaced.subtasks).toEqual(["a", "b"]);
    expect(spaced.remainingText).toBe("Intro\n\nAfter");
    // Only one line, only right under the heading, and only one ending in a colon.
    expect(extractSubtasks("## Tasks\nFirst read this.\n- a")).toEqual([]);
    expect(extractSubtasks("## Tasks\nOne:\nTwo:\n- a")).toEqual([]);
  });

  it("stays linear on a long run of blank lines inside a list", () => {
    const text = `Tasks:\n- a${"\n".repeat(60_000)}- b`;
    const started = performance.now();
    expect(extractSubtasks(text)).toEqual(["a", "b"]);
    // Generous on purpose: the quadratic version took seconds, not milliseconds.
    expect(performance.now() - started).toBeLessThan(wallClockBudget(1_000));
  });

  it("stays linear on long lines that look like the start of a heading or a wrap", () => {
    const opener = `Tasks (${"(".repeat(64_000)}`;
    const wrapped = `- [ ] ${"a, ".repeat(20_000)}\n${"b ".repeat(20_000)}`;
    const started = performance.now();
    expect(extractSubtasks(opener)).toEqual([]);
    expect(extractSubtasks(wrapped)).toHaveLength(1);
    expect(composeFromNotes({ title: "", description: "" }, `${opener}\n${wrapped}`).extracted).toBe(true);
    expect(performance.now() - started).toBeLessThan(wallClockBudget(1_000));
  });
});

describe("criteriaFromPaste", () => {
  it("keeps the pasted order, drops only checklist headings, and keeps every other line", () => {
    expect(criteriaFromPaste("Subtasks:\nReproduce\nAdd test")).toEqual(["Reproduce", "Add test"]);
    expect(criteriaFromPaste("Login works\n- [ ] Tests pass\nDocs updated")).toEqual(["Login works", "- [ ] Tests pass", "Docs updated"]);
    expect(criteriaFromPaste("## Acceptance criteria\n- a\nHandle the admin case:\n- b")).toEqual(["- a", "Handle the admin case:", "- b"]);
    expect(criteriaFromPaste("\n\n")).toEqual([]);
  });
});

describe("appendCriteria", () => {
  it("adds after the rows a person has without rewriting them, and counts what it refused", () => {
    const result = appendCriteria(["Ship it", "ship it", "N/A", "1) Deploy"], ["New", "SHIP IT", "- [ ] Deploy"]);
    expect(result.list).toEqual(["Ship it", "ship it", "N/A", "1) Deploy", "New"]);
    expect(result).toMatchObject({ added: ["New"], duplicates: 2, overflow: 0 });
    expect(describeRefusedCriteria(result)).toBe("2 not added: already listed.");
    const full = appendCriteria(Array.from({ length: MAX_SUBTASKS }, (_, i) => `r${i}`), ["one more"]);
    expect(full).toMatchObject({ added: [], overflow: 1 });
    expect(describeRefusedCriteria(full)).toBe(`1 not added: the list holds ${MAX_SUBTASKS}.`);
  });

  it("reports an item shortened to the store's 4 KB instead of cutting it in silence", () => {
    const result = appendCriteria([], ["界".repeat(2000)]);
    expect(result.truncated).toBe(1);
    expect(describeRefusedCriteria(result)).toBe("1 shortened to 4 KB, the most a criterion holds.");
  });

  it("counts room from the rows as they are, so a near-full list never loses a moved item", () => {
    // 128 rows, two differing only in case: the list is full.
    const rows = [...Array.from({ length: MAX_SUBTASKS - 1 }, (_, i) => `r${i}`), "R0"];
    const result = extractDraftSubtasks({ description: "Keep\n- [ ] new item", acceptance_criteria: rows });
    expect(result.count).toBe(0);
    expect(result.description).toContain("- [ ] new item");
    expect(result.subtasks).toHaveLength(MAX_SUBTASKS);
  });

  it("is what extraction and notes append with, so existing rows survive both", () => {
    expect(extractDraftSubtasks({ description: "- [ ] New", acceptance_criteria: ["N/A", "a", "A"] }).subtasks).toEqual(["N/A", "a", "A", "New"]);
    expect(composeFromNotes({ title: "t", description: "", acceptance_criteria: ["N/A"] }, "- [ ] New").acceptance_criteria).toEqual(["N/A", "New"]);
  });
});

describe("composeFromNotes", () => {
  it("turns a pasted brief into a title, prose description and criteria, with no second copy of the list", () => {
    const notes = ["Login loops after token refresh", "", "Seen on Safari only.", "", "Subtasks:", "- [ ] Reproduce", "- [ ] Add a test"].join("\n");
    const result = composeFromNotes({ title: "", description: "", acceptance_criteria: ["Add a test"] }, notes);
    expect(result.title).toBe("Login loops after token refresh");
    expect(result.titleFromNotes).toBe(true);
    expect(result.description).toBe("Seen on Safari only.");
    expect(result.description).not.toContain("Reproduce");
    expect(result.acceptance_criteria).toEqual(["Add a test", "Reproduce"]);
    expect(result.addedCriteria).toEqual(["Reproduce"]);
    expect(result.notes).toBe("");
  });

  it("gives notes that are only a checklist a title and an untouched description", () => {
    const result = composeFromNotes({ title: "", description: "Existing words" }, "- [ ] Keep `src/main.go` and E42\n- [ ] Second");
    expect(result.title).toBe("Keep `src/main.go` and E42");
    expect(result.description).toBe("Existing words");
    expect(result.addedText).toBe("");
    expect(result.acceptance_criteria).toEqual(["Keep `src/main.go` and E42", "Second"]);
  });

  it("keeps an existing title and appends the notes after the description", () => {
    const result = composeFromNotes({ title: "Mine", description: "Old" }, "New context");
    expect(result).toMatchObject({ title: "Mine", titleFromNotes: false, description: "Old\n\nNew context", addedText: "New context" });
  });

  it("never titles a task with the heading over its list or a bare section label", () => {
    expect(composeFromNotes({ title: "", description: "" }, "Subtasks:\n- Reproduce\n- Add a test").title).toBe("Reproduce");
    expect(composeFromNotes({ title: "", description: "" }, "## Acceptance criteria\n- Tests pass").title).toBe("Tests pass");
    expect(composeFromNotes({ title: "", description: "" }, "## Description\nLogin loops after refresh").title).toBe("Login loops after refresh");
    expect(titleFromNotes("Steps to reproduce:\n1. Open the app")).toBe("1. Open the app");
    // A title that merely starts with a label word is still a title.
    expect(titleFromNotes("Notes app crashes on save")).toBe("Notes app crashes on save");
  });

  it("moves a whole-line title out of the description, and only a whole line", () => {
    // One line of notes is a title and nothing else.
    expect(composeFromNotes({ title: "", description: "" }, "Prepare the Seattle demo")).toMatchObject({ title: "Prepare the Seattle demo", description: "", addedText: "" });
    // A first sentence of a longer line: the line stays, it holds the rest.
    const clause = composeFromNotes({ title: "", description: "" }, "Must keep E42. Do not drop the reproduction steps.");
    expect(clause.title).toBe("Must keep E42.");
    expect(clause.description).toBe("Must keep E42. Do not drop the reproduction steps.");
    // Cut at the cap: the full line stays in the description.
    const long = composeFromNotes({ title: "", description: "" }, "x".repeat(400));
    expect(long.description).toBe("x".repeat(400));
    // Markup around the title line goes with it; a link before it is evidence and stays.
    expect(composeFromNotes({ title: "", description: "" }, "# Fix the login loop\nIt loops.").description).toBe("It loops.");
    expect(composeFromNotes({ title: "", description: "" }, "https://example.com/i/42\nFix the login loop\nIt loops.").description)
      .toBe("https://example.com/i/42\nIt loops.");
    // A bare label that only introduced the title line goes too.
    expect(composeFromNotes({ title: "", description: "" }, "## Description\nFix the login loop\n\nIt loops.").description).toBe("It loops.");
    // A title the person typed keeps the notes whole.
    expect(composeFromNotes({ title: "Mine", description: "" }, "Fix the login loop\nIt loops.").description).toBe("Fix the login loop\nIt loops.");
  });

  it("never lifts a list item or the first line of a hard-wrapped paragraph out of the description", () => {
    const steps = composeFromNotes({ title: "", description: "" }, "Steps to reproduce:\n1. Open the app\n2. Click save\n3. Crash");
    expect(steps.description).toBe("Steps to reproduce:\n1. Open the app\n2. Click save\n3. Crash");
    const wrapped = composeFromNotes({ title: "", description: "" }, "Login loops after token\nrefresh on Safari when the\ncookie expires.");
    expect(wrapped.description).toBe("Login loops after token\nrefresh on Safari when the\ncookie expires.");
    const bullets = composeFromNotes({ title: "", description: "" }, "- Fix A\n- Fix B");
    expect(bullets.title).toBe("Fix A");
    expect(bullets.description).toBe("- Fix A\n- Fix B");
    // A line ending mid-clause continues on the next one.
    expect(composeFromNotes({ title: "", description: "" }, "Retry the upload when the network drops,\nThen log it.").description)
      .toBe("Retry the upload when the network drops,\nThen log it.");
  });

  it("does not promise description text the save would not add", () => {
    const same = composeFromNotes({ title: "", description: "Fix X" }, "Fix X");
    expect(same.description).toBe("Fix X");
    expect(same.addedText).toBe("");
  });
});

