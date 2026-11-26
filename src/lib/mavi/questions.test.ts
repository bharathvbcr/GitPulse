import { describe, expect, it } from "vitest";
import { applyLine, emptyThread } from "./reduce";
import { toRows } from "./rows";
import {
  answerFor,
  answersFromResult,
  buildAnswers,
  encodeAnswers,
  parseQuestions,
  type AskQuestion,
} from "./questions";

const single: AskQuestion = {
  question: "Which scope?",
  header: "Scope",
  multiSelect: false,
  options: [
    { label: "Quick", description: "" },
    { label: "Full", description: "" },
  ],
};
const multi: AskQuestion = { ...single, question: "Which checks?", multiSelect: true };

describe("parseQuestions", () => {
  it("reads Claude Code's input shape", () => {
    const parsed = parseQuestions({
      questions: [
        {
          question: "Which scope?",
          header: "Scope",
          multiSelect: false,
          options: [{ label: "Quick", description: "fast" }, { label: "Full" }],
        },
      ],
    });
    expect(parsed).toEqual([
      {
        question: "Which scope?",
        header: "Scope",
        multiSelect: false,
        options: [
          { label: "Quick", description: "fast" },
          { label: "Full", description: "" },
        ],
      },
    ]);
  });

  it("refuses a malformed or oversized set", () => {
    expect(parseQuestions({})).toBeNull();
    expect(parseQuestions({ questions: [] })).toBeNull();
    expect(parseQuestions({ questions: [{ header: "x" }] })).toBeNull();
    expect(parseQuestions({ questions: Array.from({ length: 5 }, () => ({ question: "q" })) })).toBeNull();
  });
});

describe("answers", () => {
  it("a single choice is its label; free text replaces it", () => {
    expect(answerFor(single, { labels: ["Full"], other: "" })).toBe("Full");
    expect(answerFor(single, { labels: ["Full"], other: "  only mavi/  " })).toBe("only mavi/");
    expect(answerFor(single, { labels: [], other: "" })).toBeNull();
    expect(answerFor(single, undefined)).toBeNull();
  });

  it("a multi-select joins labels with a comma and a space, free text added last", () => {
    expect(answerFor(multi, { labels: ["lint", "tests"], other: "" })).toBe("lint, tests");
    expect(answerFor(multi, { labels: ["lint"], other: "docs" })).toBe("lint, docs");
    expect(answerFor(multi, { labels: [], other: "" })).toBeNull();
  });

  it("buildAnswers is keyed by question text and null until every question is answered", () => {
    const picks = [{ labels: ["Quick"], other: "" }, undefined];
    expect(buildAnswers([single, multi], picks)).toBeNull();
    picks[1] = { labels: ["lint"], other: "" };
    expect(buildAnswers([single, multi], picks)).toEqual({ "Which scope?": "Quick", "Which checks?": "lint" });
  });

  it("encodes only the answers, never an updatedInput", () => {
    const encoded = encodeAnswers({ "Which scope?": "Full" });
    expect(JSON.parse(encoded)).toEqual({ answers: { "Which scope?": "Full" } });
  });

  it("reads the answers back out of the tool result", () => {
    const result = 'User has answered your questions: "Which scope?"="Full", "Which checks?"="lint, tests". You can now continue.';
    expect(answersFromResult(result)).toEqual([
      { question: "Which scope?", answer: "Full" },
      { question: "Which checks?", answer: "lint, tests" },
    ]);
    expect(answersFromResult("The user did not answer the questions")).toBeNull();
  });
});

describe("the timeline", () => {
  function ask() {
    const state = emptyThread();
    applyLine(
      state,
      JSON.stringify({
        type: "assistant",
        message: {
          id: "m1",
          content: [
            {
              type: "tool_use",
              id: "t1",
              name: "AskUserQuestion",
              input: { questions: [{ question: "Which scope?", header: "Scope", multiSelect: false, options: [{ label: "A" }, { label: "B" }] }] },
            },
          ],
        },
      }),
    );
    return state;
  }

  it("shows an answered question as its own row, not folded into 'Asked a question'", () => {
    const state = ask();
    const rows = toRows(state.items, "/repo");
    expect(rows.map((r) => r.kind)).toContain("question");
    expect(rows.some((r) => r.kind === "tools")).toBe(false);
  });

  it("keeps an open request as the answerable approval row", () => {
    const state = ask();
    applyLine(
      state,
      JSON.stringify({
        type: "control_request",
        request_id: "q1",
        request: { subtype: "can_use_tool", tool_name: "AskUserQuestion", tool_use_id: "t1", input: {} },
      }),
    );
    expect(toRows(state.items, "/repo").map((r) => r.kind)).toEqual(["approval"]);
  });
});
