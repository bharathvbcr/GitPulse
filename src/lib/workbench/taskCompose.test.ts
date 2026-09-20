import { describe, expect, it } from "vitest";
import {
  AGENT_COPY_PREAMBLE,
  MAX_AGENT_COPY_TASKS,
  MAX_SUBTASKS,
  SUBTASK_CAP,
  applyNotesToDraft,
  canAskManvi,
  consumeNotes,
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
    const constrained = consumeNotes({ title: "", description: "" }, "Must keep E42. Do not drop the reproduction steps.");
    expect(constrained.title).toBe("Must keep E42.");
    expect(constrained.description).toContain("Do not drop the reproduction steps.");
    expect(constrained.notes).toBe("");
  });

  it("consumes notes into the draft once so they cannot be re-applied", () => {
    const first = consumeNotes({ title: "", description: "" }, "Fix notification routing\nKeep saved evidence.");
    expect(first).toEqual({
      title: "Fix notification routing",
      description: "Fix notification routing\nKeep saved evidence.",
      notes: "",
      extracted: true,
    });
    expect(consumeNotes({ title: first.title, description: first.description }, first.notes).extracted).toBe(false);
    const leftover = consumeNotes(
      { title: "Prepare Demo for Seattle start-up event", description: "Prepare Demo for Seattle start-up event" },
      "Prepare Demo for Seattle start-up event",
    );
    expect(leftover.extracted).toBe(true);
    expect(leftover.notes).toBe("");
  });
});

describe("canAskManvi", () => {
  it("requires a repository and either a title or notes", () => {
    expect(canAskManvi({ title: "Keep", repository_ids: [] }, "notes")).toMatch(/repository/);
    expect(canAskManvi({ title: "", repository_ids: ["r"] }, "")).toMatch(/title/);
    expect(canAskManvi({ title: "", repository_ids: ["r"] }, "Draft this bug")).toBeNull();
    expect(canAskManvi({ title: "Keep", repository_ids: ["r"] }, "")).toBeNull();
  });
});

describe("agent copy", () => {
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
    expect(copy).toContain(AGENT_COPY_PREAMBLE);
    expect(copy).toContain("Keep E42");
    expect(copy).toContain("exact error: E42");
    expect(copy).toContain("- [ ] Both repository checks pass");
    expect(copy).toContain("GitPulse");
    expect(formatDraftAgentCopy({ title: "   ", description: "" })).toBeNull();
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
    // Both copy paths share one preamble, and both reach an agent that has
    // never seen this repository: whatever is missing here is missing entirely.
    const draft = formatDraftAgentCopy({ title: "Keep E42", description: "exact error: E42" })!;
    const saved = wrapSavedBriefForAgent("# Task brief v1\n\n## Title\nKeep E42")!;
    for (const packet of [draft, saved]) {
      expect(packet).toContain("Preserve the author's intent and message");
      expect(packet).toContain("gitpulse-insights");
      expect(packet).toContain("devmap-impact");
      expect(packet).toContain("devmap_* MCP tools");
      expect(packet).toContain("gitpulse_* MCP tools");
      expect(packet).toContain("absolute repo_path");
    }
  });

  it("wraps a saved brief and refuses empty or hostile markdown", () => {
    const wrapped = wrapSavedBriefForAgent("# Task brief v1\n\n## Title\nKeep E42");
    expect(wrapped?.startsWith(AGENT_COPY_PREAMBLE)).toBe(true);
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

    const longItem = "x".repeat(SUBTASK_CAP + 50);
    const mergedLong = mergeSubtasks([], [longItem]);
    expect(mergedLong[0]?.length).toBe(SUBTASK_CAP);
  });

  it("extractDraftSubtasks extracts from description and notes and reports new count", () => {
    const draft = {
      description: "Fix loop\n\n## Subtasks\n- [ ] Item 1\n- [ ] Item 2",
      acceptance_criteria: ["Item 1"],
    };
    const notes = "- [ ] Item 3";
    const result = extractDraftSubtasks(draft, notes);
    expect(result.extracted).toBe(true);
    expect(result.count).toBe(2); // Item 2 and Item 3 are new
    expect(result.subtasks).toEqual(["Item 1", "Item 2", "Item 3"]);
    expect(result.description).toBe("Fix loop");
  });
});

