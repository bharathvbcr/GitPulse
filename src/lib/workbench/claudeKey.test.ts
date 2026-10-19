import { describe, expect, it } from "vitest";
import { claudeKeyProblem } from "./claudeKey";

describe("claudeKeyProblem", () => {
  it("accepts an Anthropic key, trimmed", () => {
    expect(claudeKeyProblem("  sk-ant-api03-Abc_def-0123456789  ")).toBeNull();
  });

  it("refuses what the backend would refuse", () => {
    for (const raw of [
      "",
      "   ",
      "sk-proj-0123456789abcdefghij",
      "sk-ant-short",
      "sk-ant-api03-has space-0123456789",
      "sk-ant-api03-0123456789abcdef;rm",
      `sk-ant-${"a".repeat(300)}`,
    ]) {
      expect(claudeKeyProblem(raw), raw).not.toBeNull();
    }
  });
});
