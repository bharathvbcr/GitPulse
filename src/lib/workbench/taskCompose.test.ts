import { describe, expect, it } from "vitest";
import {
  AGENT_COPY_PREAMBLE,
  applyNotesToDraft,
  canAskManvi,
  consumeNotes,
  formatDraftAgentCopy,
  joinAgentCopies,
  MAX_AGENT_COPY_TASKS,
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
