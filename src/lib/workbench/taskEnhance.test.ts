import { describe, expect, it } from "vitest";
import { ENHANCEMENT_STATES, type Enhancement, type EnhancementSummary, type Task } from "./client";
import { acceptEnhancementInput, createEnhancementInput, runAppleEnhancement,
  assistEngineName,
  enhancementApplyBlock,
  enhancementOptionLabel,
  DEFAULT_ASSIST_ENGINE,
  ASSIST_ENGINE_LIST,
  ENHANCEMENT_STATE_LABELS,
} from "./taskEnhance";

const task: Task = {
  id: "t1", revision: 4, updated_at: 1, title: "Keep E42", description: "evidence",
  kind: "bug", status: "ready", priority: 1, severity: null, owner: null, due_at: null,
  labels: [], acceptance_criteria: [], repository_ids: ["r"], primary_repository_id: "r",
  home_workspace_id: null, position: 1, locked_fields: ["title"],
};

describe("createEnhancementInput", () => {
  it("drops locked fields, empty providers and oversize models", () => {
    expect(createEnhancementInput(task, ["title", "description"], "local", "m", { id: "e", requestId: "r" }))
      .toMatchObject({ fields: ["description"], task_id: "t1", source_revision: 4, expected_revision: 0 });
    expect(createEnhancementInput(task, ["title"], "local", "m", { id: "e", requestId: "r" })).toBeNull();
    expect(createEnhancementInput(task, ["description"], "  ", "m", { id: "e", requestId: "r" })).toBeNull();
    expect(createEnhancementInput(task, ["description"], "local", "x".repeat(513), { id: "e", requestId: "r" })).toBeNull();
    expect(createEnhancementInput(task, ["description"], "local", "m", { id: "", requestId: "r" })).toBeNull();
  });
});

describe("acceptEnhancementInput", () => {
  it("accepts only ready, unlocked, proposed fields with a real request identity", () => {
    const ready = { id: "e", task_id: "t1", source_revision: 4, revision: 3, state: "ready", fields: ["title", "description"] } as never;
    expect(acceptEnhancementInput(ready, task, ["title", "description"], "r")).toMatchObject({
      fields: ["description"],
      expected_task_revision: 4,
    });
    expect(acceptEnhancementInput(ready, task, ["title"], "r")).toBeNull();
    expect(acceptEnhancementInput({ id: "e", revision: 3, state: "pending", fields: ["description"] } as never, task, ["description"], "r")).toBeNull();
    expect(acceptEnhancementInput(ready, task, ["description"], "")).toBeNull();
  });
});

describe("runAppleEnhancement", () => {
  const open: Task = { ...task, locked_fields: [] };
  const source = { kind: "draft" as const, notes: "the menu has no due date", title: "", description: "", context: "Repository: GitPulse" };
  const proposal = (over: Record<string, unknown> = {}) => ({
    id: "e", task_id: "t1", source_revision: 4, revision: 1, state: "pending",
    fields: ["title", "description"], proposed: {}, ...over,
  }) as never;

  function harness(over: Partial<Parameters<typeof runAppleEnhancement>[3]> = {}) {
    const calls: { create: unknown[]; complete: unknown[]; draft: unknown[] } = { create: [], complete: [], draft: [] };
    const io = {
      create: async (_method: never, input: Record<string, unknown>) => {
        calls.create.push(input);
        return proposal({ id: input.id, fields: input.fields });
      },
      complete: async (input: Record<string, unknown>) => {
        calls.complete.push(input);
        return proposal({ id: input.id, revision: 2, state: "ready", proposed: { title: input.title, description: input.description } });
      },
      draft: async (input: unknown) => {
        calls.draft.push(input);
        return { title: "Add a due date to the board menu", description: "Set a due date without opening the sheet.", rationale: "on device" };
      },
      ...over,
    } as Parameters<typeof runAppleEnhancement>[3];
    return { io, calls };
  }

  it("creates the proposal in the store and completes it here", async () => {
    const { io, calls } = harness();
    const result = await runAppleEnhancement(open, ["title", "description"], source, io);
    expect(result.state).toBe("ready");
    // The engine is recorded on the proposal, so the history drawer can say
    // which one wrote a given revision months later.
    expect(calls.create[0]).toMatchObject({ provider: "apple-intelligence", model: "on-device", source_revision: 4 });
    expect(calls.draft[0]).toMatchObject({ kind: "draft", fields: ["title", "description"], notes: source.notes });
    // The completion must target the proposal that was just created, not a
    // fresh identity: a mismatch would publish text onto someone else's record.
    const created = calls.create[0] as { id: string };
    expect(calls.complete[0]).toMatchObject({
      id: created.id, expected_revision: 1,
      title: "Add a due date to the board menu",
      description: "Set a due date without opening the sheet.",
      rationale: "on device",
    });
    // No `failure` on the success path: the store refuses a completion that
    // carries both a proposal and a reason it failed.
    expect(calls.complete[0]).not.toHaveProperty("failure");
  });

  it("never proposes a field the store was not told to expect", async () => {
    // `enhancements.complete` refuses a proposal touching an unrequested
    // field, and it refuses it after the model has already run.
    const { io, calls } = harness();
    await runAppleEnhancement({ ...open, locked_fields: ["title"] }, ["title", "description"], source, io);
    expect(calls.create[0]).toMatchObject({ fields: ["description"] });
    expect(calls.complete[0]).not.toHaveProperty("title");
    expect(calls.complete[0]).toMatchObject({ description: "Set a due date without opening the sheet." });
  });

  it("writes a failed proposal back before rethrowing, so nothing stays pending", async () => {
    const { io, calls } = harness({
      draft: async () => { throw { code: "refused", message: "Apple Intelligence declined." }; },
    });
    await expect(runAppleEnhancement(open, ["title"], source, io)).rejects.toMatchObject({ code: "refused" });
    // A pending record holds the store's "one live attempt" lock for its whole
    // lease. Leaving one behind would block the next try for three minutes.
    expect(calls.complete).toHaveLength(1);
    expect(calls.complete[0]).toMatchObject({ id: (calls.create[0] as { id: string }).id, failure: "Apple Intelligence declined." });
    expect(calls.complete[0]).not.toHaveProperty("title");
  });

  it("reports the generation failure even when the cleanup also fails", async () => {
    const { io } = harness({
      draft: async () => { throw { code: "too_large", message: "Too long for the on-device model." }; },
      complete: async () => { throw new Error("store is closed"); },
    });
    await expect(runAppleEnhancement(open, ["title"], source, io))
      .rejects.toMatchObject({ code: "too_large", message: "Too long for the on-device model." });
  });

  it("refuses a confirmation that does not match what it asked for", async () => {
    const wrongTask = harness({ create: async () => proposal({ task_id: "other" }) });
    await expect(runAppleEnhancement(open, ["title"], source, wrongTask.io)).rejects.toMatchObject({ code: "protocol_error" });
    const wrongRevision = harness({ create: async () => proposal({ source_revision: 3 }) });
    await expect(runAppleEnhancement(open, ["title"], source, wrongRevision.io)).rejects.toMatchObject({ code: "protocol_error" });
    const noAdvance = harness({ complete: async () => proposal({ revision: 1, state: "ready" }) });
    await expect(runAppleEnhancement(open, ["title"], source, noAdvance.io)).rejects.toMatchObject({ code: "protocol_error" });
  });

  it("refuses when every requested field is locked", async () => {
    const { io, calls } = harness();
    await expect(runAppleEnhancement(task, ["title"], source, io)).rejects.toMatchObject({ code: "invalid_input" });
    expect(calls.create).toHaveLength(0);
    expect(calls.draft).toHaveLength(0);
  });
});

describe("engine names", () => {
  /**
   * Two components write this name — the picker in the assist section, and the
   * sheet around it (placeholders, and the message that refuses a shortcut
   * while a draft is running). Before this, the sheet spelled "Manvi" inline,
   * so selecting Apple Intelligence produced a field that still offered to let
   * Manvi write it. Asserting the table here, rather than the components'
   * markup, is what keeps a third spelling from appearing.
   */
  it("names every engine it can run, with no unnamed member", () => {
    expect(ASSIST_ENGINE_LIST).toEqual(["manvi", "apple"]);
    for (const engine of ASSIST_ENGINE_LIST) {
      expect(assistEngineName(engine), engine).toMatch(/^\S.*\S$/);
    }
    expect(assistEngineName("manvi")).toBe("Manvi");
    expect(assistEngineName("apple")).toBe("Apple Intelligence");
  });

  it("defaults to an engine every build has", () => {
    // Apple Intelligence is compiled in only on a macOS host with the SDK, so
    // it cannot be the default a sheet seeds itself with.
    expect(DEFAULT_ASSIST_ENGINE).toBe("manvi");
    expect(ASSIST_ENGINE_LIST).toContain(DEFAULT_ASSIST_ENGINE);
  });

  it("cannot be reached through a polluted prototype", () => {
    expect(assistEngineName("constructor" as never)).toBeUndefined();
    expect(assistEngineName("__proto__" as never)).toBeUndefined();
  });
});

const summary = (over: Partial<EnhancementSummary> = {}): EnhancementSummary => ({
  id: "e1", revision: 2, updated_at: 10, task_id: "t1", source_revision: 4,
  fields: ["title"], state: "ready", provider: "local", model: "qwen", automatic: false,
  created_at: 100, expires_at: 200, worker_id: null, failure: "",
  accepted_fields: [], edited_fields: [], outcome_uncertain: false, ...over,
});

describe("ENHANCEMENT_STATE_LABELS", () => {
  it("names every state the store can return", () => {
    // A total map: a state added upstream must not render `undefined` in a
    // picker option, which is where a missing label would first be seen.
    for (const state of ENHANCEMENT_STATES) {
      expect(ENHANCEMENT_STATE_LABELS[state], state).toBeTruthy();
    }
    expect(Object.keys(ENHANCEMENT_STATE_LABELS).sort()).toEqual([...ENHANCEMENT_STATES].sort());
  });

  it("cannot be reached through a polluted prototype", () => {
    expect(ENHANCEMENT_STATE_LABELS["constructor" as never]).toBeUndefined();
  });
});

describe("enhancementOptionLabel", () => {
  it("names the attempt, its state, its model, when it ran and what it was written against", () => {
    expect(enhancementOptionLabel(summary(), { ordinal: 7, when: "3m ago" }))
      .toBe("#7 · Ready for review · qwen · 3m ago · rev 4");
  });

  it("tells two runs of the same model on the same revision apart", () => {
    // The old rows read `state · model` + `Task revision N` — for two attempts
    // against one revision with one model that is the same string twice, which
    // is what made the list unusable. Same second, same everything else: the
    // ordinal is what has to separate them.
    const a = enhancementOptionLabel(summary({ id: "a" }), { ordinal: 7, when: "just now" });
    const b = enhancementOptionLabel(summary({ id: "b" }), { ordinal: 6, when: "just now" });
    expect(a).not.toBe(b);
  });

  it("adds a qualifier only when it applies, and only at the end", () => {
    expect(enhancementOptionLabel(summary({ automatic: true }), { ordinal: 1, when: "x" }))
      .toBe("#1 · Ready for review · qwen · x · rev 4 · Automatic");
    expect(enhancementOptionLabel(summary({ edited_fields: ["title"] }), { ordinal: 1, when: "x" }))
      .toMatch(/· Edited$/);
    expect(enhancementOptionLabel(summary({ outcome_uncertain: true }), { ordinal: 1, when: "x" }))
      .toMatch(/· Uncertain$/);
    expect(enhancementOptionLabel(summary(), { ordinal: 1, when: "x" })).not.toMatch(/Automatic|Edited|Uncertain/);
  });

  it("stays readable when the summary is missing its model or its timestamp", () => {
    // `gp-select` truncates, so the identifying part is written first and a
    // blank tail must not leave a dangling separator.
    expect(enhancementOptionLabel(summary({ model: "" }), { ordinal: 2, when: "" }))
      .toBe("#2 · Ready for review · unnamed model · rev 4");
  });
});

describe("enhancementApplyBlock", () => {
  const ready = { state: "ready" as const, source_revision: 4 };

  it("allows a ready suggestion written against the task in front of the reader", () => {
    expect(enhancementApplyBlock(ready, { revision: 4, locked_fields: [] })).toBe("");
  });

  it("names the revision gap rather than rendering no button", () => {
    // `acceptEnhancementInput` returns null here and the store re-checks
    // `expected_task_revision`, so this is a fact to state, not a UI choice.
    expect(enhancementApplyBlock(ready, { revision: 6, locked_fields: [] }))
      .toBe("Written against revision 4; this task is at revision 6. Ask for a fresh suggestion.");
  });

  it("names the state when there is nothing left to apply", () => {
    expect(enhancementApplyBlock({ state: "accepted", source_revision: 4 }, { revision: 4, locked_fields: [] }))
      .toBe("This suggestion is accepted, so there is nothing left to apply.");
    expect(enhancementApplyBlock({ state: "dismissed", source_revision: 4 }, { revision: 4, locked_fields: [] }))
      .toMatch(/^This suggestion is dismissed/);
  });

  it("names a fully locked task", () => {
    expect(enhancementApplyBlock(ready, { revision: 4, locked_fields: ["title", "description"] }))
      .toBe("Title and description are locked against enhancement.");
  });

  it("says nothing when there is nothing selected", () => {
    expect(enhancementApplyBlock(null, { revision: 4, locked_fields: [] })).toBe("");
    expect(enhancementApplyBlock(ready, null)).toBe("");
  });

  it("agrees with the input builder about what can be accepted", () => {
    // The message and the refusal must not be able to disagree: a block with
    // an empty reason beside a null input would be a dead Accept button, and a
    // reason beside a live input would be a lie.
    const cases: { proposal: Pick<Enhancement, "state" | "source_revision">; revision: number; locked: Task["locked_fields"] }[] = [
      { proposal: ready, revision: 4, locked: [] },
      { proposal: ready, revision: 5, locked: [] },
      { proposal: { state: "accepted", source_revision: 4 }, revision: 4, locked: [] },
      { proposal: ready, revision: 4, locked: ["title", "description"] },
    ];
    for (const entry of cases) {
      const full = { ...task, revision: entry.revision, locked_fields: entry.locked } satisfies Task;
      const proposal = { ...entry.proposal, id: "e1", revision: 2, task_id: full.id, fields: ["title", "description"] } as Enhancement;
      const input = acceptEnhancementInput(proposal, full, ["title", "description"], "req");
      expect(Boolean(enhancementApplyBlock(entry.proposal, full)), JSON.stringify(entry)).toBe(input === null);
    }
  });
});
